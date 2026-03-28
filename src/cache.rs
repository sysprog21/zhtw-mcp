// Incremental scan cache for CLI lint.
//
// Two-tier lookup to avoid reading files on cache hit:
//   1. Fast path: stat(file) -> check (path, mtime, size, params) -> return
//      cached ScanOutput without reading the file.
//   2. Slow path (mtime miss): read file, blake3 hash, full cache key check.
//
// TTL-based expiry (default 24h) and MAX_ENTRIES cap prevent unbounded
// growth.  Atomic writes via tempfile+rename with flock serialization.
//
// MCP path does NOT use this cache (stateless by design).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::engine::scan::ScanOutput;

/// Default TTL in seconds (24 hours).
const DEFAULT_TTL_SECS: u64 = 24 * 60 * 60;

/// Maximum number of cached entries.  Prevents unbounded growth when
/// scanning large monorepos.  Oldest entries evicted first on overflow.
const MAX_ENTRIES: usize = 2000;

/// BLAKE3 hash of `data`, returned as a 64-char lowercase hex string.
/// ~3-4x faster than SHA-256 thanks to SIMD acceleration.  Non-cryptographic
/// use (local cache keys) makes this a safe choice.
fn blake3_hex(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

/// Filesystem metadata used for the fast-path cache check.
/// Avoids reading the file and computing a content hash when mtime+size match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct FileMeta {
    mtime_secs: u64,
    size: u64,
}

/// Scan parameters that affect output (excluding file content).
#[derive(Debug, Clone, Hash, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanParams {
    pub ruleset_hash: String,
    pub profile: String,
    pub content_type: String,
    // Currently always "None" because caching is disabled when fix_mode
    // is active.  Kept for forward-compatibility.
    pub fix_mode: String,
    // Whether AI detection is active — changes scan results.
    pub detect_ai: bool,
    // AI threshold level (formatted f32) — different multipliers produce different results.
    pub ai_threshold: String,
}

/// A single cached entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    file_path: String,
    content_hash: String,
    file_meta: FileMeta,
    params: ScanParams,
    output: ScanOutput,
    input_was_sc: bool,
    timestamp_secs: u64,
}

/// Persistent scan cache backed by a JSON file.
/// Entries are loaded lazily on first access to avoid upfront I/O
/// and deserialization cost when all files are new/modified.
#[derive(Debug)]
pub struct ScanCache {
    path: PathBuf,
    entries: Option<HashMap<String, CacheEntry>>,
    ttl_secs: u64,
    dirty: bool,
}

/// Cached scan result including script classification.
pub struct CacheHit {
    pub output: ScanOutput,
    pub input_was_sc: bool,
}

/// Result of a cache lookup.
pub enum CacheResult {
    /// Fast-path hit: mtime+size match, no file read needed.
    Hit(Box<CacheHit>),
    /// mtime changed or no entry: caller must read file and call `check_content`.
    Miss,
}

impl CacheResult {
    /// Extract the cached hit, or None on miss.
    pub fn into_hit(self) -> Option<CacheHit> {
        match self {
            CacheResult::Hit(h) => Some(*h),
            CacheResult::Miss => None,
        }
    }
}

impl ScanCache {
    /// Open (or create) the scan cache at the default location.
    pub fn open_default() -> Self {
        Self::open(default_cache_path())
    }

    /// Open (or create) the scan cache at a specific path.
    /// Entries are NOT loaded until the first lookup; this avoids
    /// deserializing 2000 entries when all files are cache-misses.
    pub fn open(path: PathBuf) -> Self {
        ScanCache {
            path,
            entries: None,
            ttl_secs: DEFAULT_TTL_SECS,
            dirty: false,
        }
    }

    /// Ensure entries are loaded from disk (lazy initialization).
    fn ensure_loaded(&mut self) {
        if self.entries.is_none() {
            self.entries = Some(load_entries(&self.path, self.ttl_secs));
        }
    }

    /// Get a reference to entries, loading if necessary.
    fn entries(&mut self) -> &HashMap<String, CacheEntry> {
        self.ensure_loaded();
        self.entries.as_ref().unwrap()
    }

    /// Get a mutable reference to entries, loading if necessary.
    fn entries_mut(&mut self) -> &mut HashMap<String, CacheEntry> {
        self.ensure_loaded();
        self.entries.as_mut().unwrap()
    }

    /// Fast-path lookup using filesystem metadata (mtime + size).
    /// Avoids reading the file when metadata matches.
    pub fn check_fast(
        &mut self,
        file_path: &str,
        mtime_secs: u64,
        size: u64,
        params: &ScanParams,
    ) -> CacheResult {
        let ttl = self.ttl_secs;
        self.ensure_loaded();
        let entries = self.entries.as_ref().unwrap();
        if let Some(entry) = entries.get(&fast_key(file_path, params)) {
            if now_secs().saturating_sub(entry.timestamp_secs) <= ttl
                && entry.file_meta.mtime_secs == mtime_secs
                && entry.file_meta.size == size
            {
                return CacheResult::Hit(Box::new(CacheHit {
                    output: entry.output.clone(),
                    input_was_sc: entry.input_was_sc,
                }));
            }
        }
        CacheResult::Miss
    }

    /// Slow-path lookup using content hash.  Called after file is read.
    /// Returns cached output if content hash matches (mtime changed but
    /// content didn't, e.g. after `touch`).
    pub fn check_content(
        &mut self,
        file_path: &str,
        content: &[u8],
        params: &ScanParams,
    ) -> Option<CacheHit> {
        let ttl = self.ttl_secs;
        let entry = self.entries().get(&fast_key(file_path, params))?;
        if now_secs().saturating_sub(entry.timestamp_secs) > ttl {
            return None;
        }
        (entry.content_hash == blake3_hex(content)).then(|| CacheHit {
            output: entry.output.clone(),
            input_was_sc: entry.input_was_sc,
        })
    }

    /// Store a scan result in the cache.
    #[allow(clippy::too_many_arguments)]
    pub fn put(
        &mut self,
        file_path: &str,
        content: &[u8],
        mtime_secs: u64,
        size: u64,
        params: &ScanParams,
        output: ScanOutput,
        input_was_sc: bool,
    ) {
        self.entries_mut().insert(
            fast_key(file_path, params),
            CacheEntry {
                file_path: file_path.to_owned(),
                content_hash: blake3_hex(content),
                file_meta: FileMeta { mtime_secs, size },
                params: params.clone(),
                output,
                input_was_sc,
                timestamp_secs: now_secs(),
            },
        );
        self.dirty = true;
    }

    /// Flush dirty cache to disk.  Prunes expired and overflow entries
    /// before writing.  Uses tempfile + rename for atomic writes.
    /// Acquires an exclusive flock to prevent concurrent CLI processes
    /// from clobbering each other's writes.
    /// Errors are silently ignored (cache is best-effort).
    pub fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        self.ensure_loaded();
        let entries = self.entries.as_mut().unwrap();

        // Prune expired entries.
        let now = now_secs();
        let ttl = self.ttl_secs;
        entries.retain(|_, e| now.saturating_sub(e.timestamp_secs) <= ttl);

        // Evict oldest entries if over the cap.
        if entries.len() > MAX_ENTRIES {
            let mut by_time: Vec<(String, u64)> = entries
                .iter()
                .map(|(k, e)| (k.clone(), e.timestamp_secs))
                .collect();
            by_time.sort_by_key(|(_, ts)| *ts);
            let to_remove = entries.len() - MAX_ENTRIES;
            for (k, _) in by_time.into_iter().take(to_remove) {
                entries.remove(&k);
            }
        }

        let Some(parent) = self.path.parent() else {
            return;
        };
        let _ = std::fs::create_dir_all(parent);

        let entries_vec: Vec<&CacheEntry> = entries.values().collect();
        let Ok(bytes) = serde_json::to_vec(&entries_vec) else {
            return;
        };

        // Acquire exclusive lock on the cache file (or a .lock sidecar)
        // to prevent concurrent CLI processes from clobbering writes.
        let lock_path = self.path.with_extension("lock");
        let lock_file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&lock_path);

        // Best-effort lock: if another process holds it, skip this flush
        // but keep dirty=true so Drop retries.
        let locked = lock_file
            .as_ref()
            .is_ok_and(|f| f.try_lock_exclusive().is_ok());

        // Write without lock if lock file creation failed; skip entirely
        // if lock is held by another process.
        if (locked || lock_file.is_err()) && atomic_write(parent, &self.path, &bytes) {
            self.dirty = false;
        }

        if let Ok(ref f) = lock_file {
            let _ = f.unlock();
        }
    }
}

impl Drop for ScanCache {
    fn drop(&mut self) {
        self.flush();
    }
}

/// Atomic write via tempfile + rename.  Returns true on success.
/// Writes directly to the open fd (not re-opening by path).
fn atomic_write(parent: &Path, dest: &Path, bytes: &[u8]) -> bool {
    use std::io::Write;
    tempfile::NamedTempFile::new_in(parent)
        .is_ok_and(|mut tmp| tmp.write_all(bytes).is_ok() && tmp.persist(dest).is_ok())
}

/// Default cache file location: ~/.cache/zhtw-mcp/scan-cache.json
fn default_cache_path() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("zhtw-mcp")
        .join("scan-cache.json")
}

/// Lookup key combining file path + scan parameters.
/// Hashes directly into blake3 without allocating an intermediate String.
/// One entry per (file, params) tuple — mtime/content validated on lookup.
fn fast_key(file_path: &str, params: &ScanParams) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(file_path.as_bytes());
    hasher.update(b"\0");
    hasher.update(params.ruleset_hash.as_bytes());
    hasher.update(b"\0");
    hasher.update(params.profile.as_bytes());
    hasher.update(b"\0");
    hasher.update(params.content_type.as_bytes());
    hasher.update(b"\0");
    hasher.update(params.fix_mode.as_bytes());
    hasher.update(b"\0");
    hasher.update(if params.detect_ai { b"ai" } else { b"" });
    hasher.update(b"\0");
    hasher.update(params.ai_threshold.as_bytes());
    hasher.finalize().to_hex()[..32].to_string()
}

/// Extract mtime (seconds since epoch) from filesystem metadata.
pub fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Load cache entries from disk.
/// Falls back gracefully: if the file is missing or corrupt,
/// returns an empty map.
fn load_entries(path: &Path, ttl_secs: u64) -> HashMap<String, CacheEntry> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return HashMap::new(),
    };

    let entries: Vec<CacheEntry> = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return HashMap::new(),
    };

    let now = now_secs();
    entries
        .into_iter()
        .filter(|e| now.saturating_sub(e.timestamp_secs) <= ttl_secs)
        .map(|e| {
            let key = fast_key(&e.file_path, &e.params);
            (key, e)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::scan::ScanOutput;
    use crate::engine::zhtype::ChineseType;
    use tempfile::TempDir;

    fn empty_output() -> ScanOutput {
        ScanOutput {
            issues: vec![],
            detected_script: ChineseType::Traditional,
            ai_signature: None,
        }
    }

    fn test_params() -> ScanParams {
        ScanParams {
            ruleset_hash: "rh".into(),
            profile: "default".into(),
            content_type: "md".into(),
            fix_mode: "none".into(),
            detect_ai: false,
            ai_threshold: "1.0".into(),
        }
    }

    fn test_params_plain() -> ScanParams {
        ScanParams {
            ruleset_hash: "rh".into(),
            profile: "default".into(),
            content_type: "plain".into(),
            fix_mode: "none".into(),
            detect_ai: false,
            ai_threshold: "1.0".into(),
        }
    }

    #[test]
    fn fast_path_hit() {
        let dir = TempDir::new().unwrap();
        let mut cache = ScanCache::open(dir.path().join("c.bin"));
        let p = test_params();

        cache.put("a.md", b"hello", 1000, 5, &p, empty_output(), false);

        // Same mtime+size = fast hit.
        assert!(matches!(
            cache.check_fast("a.md", 1000, 5, &p),
            CacheResult::Hit(_)
        ));

        // Different mtime = miss.
        assert!(matches!(
            cache.check_fast("a.md", 2000, 5, &p),
            CacheResult::Miss
        ));

        // Different size = miss.
        assert!(matches!(
            cache.check_fast("a.md", 1000, 99, &p),
            CacheResult::Miss
        ));

        // Different profile = miss (different entry entirely).
        let strict = ScanParams {
            profile: "strict".into(),
            ..p.clone()
        };
        assert!(matches!(
            cache.check_fast("a.md", 1000, 5, &strict),
            CacheResult::Miss
        ));
    }

    #[test]
    fn detect_ai_changes_cache_key() {
        let dir = TempDir::new().unwrap();
        let mut cache = ScanCache::open(dir.path().join("c.bin"));
        let p = test_params();

        cache.put("a.md", b"hello", 1000, 5, &p, empty_output(), false);

        // Same params with detect_ai=false: hit.
        assert!(matches!(
            cache.check_fast("a.md", 1000, 5, &p),
            CacheResult::Hit(_)
        ));

        // Same file + mtime + size but detect_ai=true: miss (different key).
        let p_ai = ScanParams {
            detect_ai: true,
            ..p.clone()
        };
        assert!(matches!(
            cache.check_fast("a.md", 1000, 5, &p_ai),
            CacheResult::Miss
        ));
    }

    #[test]
    fn ai_threshold_changes_cache_key() {
        let dir = TempDir::new().unwrap();
        let mut cache = ScanCache::open(dir.path().join("c.bin"));
        let p = ScanParams {
            detect_ai: true,
            ai_threshold: "1.0".into(),
            ..test_params()
        };

        cache.put("a.md", b"hello", 1000, 5, &p, empty_output(), false);

        // Same threshold: hit.
        assert!(matches!(
            cache.check_fast("a.md", 1000, 5, &p),
            CacheResult::Hit(_)
        ));

        // Different threshold (low sensitivity): miss.
        let p_low = ScanParams {
            ai_threshold: "0.5".into(),
            ..p.clone()
        };
        assert!(matches!(
            cache.check_fast("a.md", 1000, 5, &p_low),
            CacheResult::Miss
        ));

        // Different threshold (high sensitivity): also miss.
        let p_high = ScanParams {
            ai_threshold: "1.5".into(),
            ..p.clone()
        };
        assert!(matches!(
            cache.check_fast("a.md", 1000, 5, &p_high),
            CacheResult::Miss
        ));
    }

    #[test]
    fn slow_path_content_check() {
        let dir = TempDir::new().unwrap();
        let mut cache = ScanCache::open(dir.path().join("c.bin"));
        let p = test_params_plain();

        cache.put("b.md", b"data", 1000, 4, &p, empty_output(), false);

        // Same content despite mtime miss: slow-path hit.
        assert!(cache.check_content("b.md", b"data", &p).is_some());

        // Different content: slow-path miss.
        assert!(cache.check_content("b.md", b"changed", &p).is_none());
    }

    #[test]
    fn cache_persists_to_disk() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("c.bin");
        let p = test_params_plain();

        {
            let mut cache = ScanCache::open(path.clone());
            cache.put("f.md", b"x", 100, 1, &p, empty_output(), false);
            cache.flush();
        }

        let mut cache = ScanCache::open(path);
        assert!(matches!(
            cache.check_fast("f.md", 100, 1, &p),
            CacheResult::Hit(_)
        ));
    }

    #[test]
    fn expired_entries_pruned() {
        let dir = TempDir::new().unwrap();
        let mut cache = ScanCache::open(dir.path().join("c.bin"));
        let p = test_params_plain();
        cache.put("e.md", b"x", 100, 1, &p, empty_output(), false);
        for entry in cache.entries_mut().values_mut() {
            entry.timestamp_secs = 0;
        }
        assert!(matches!(
            cache.check_fast("e.md", 100, 1, &p),
            CacheResult::Miss
        ));
    }

    #[test]
    fn overflow_evicts_oldest() {
        let dir = TempDir::new().unwrap();
        let mut cache = ScanCache::open(dir.path().join("c.bin"));
        let p = test_params_plain();

        for i in 0..MAX_ENTRIES + 10 {
            let name = format!("file_{i}.md");
            cache.put(&name, b"x", 100, 1, &p, empty_output(), false);
            let key = fast_key(&name, &p);
            if let Some(e) = cache.entries_mut().get_mut(&key) {
                e.timestamp_secs = i as u64 + 1_700_000_000;
            }
        }

        assert!(cache.entries().len() > MAX_ENTRIES);
        cache.flush();
        assert!(cache.entries().len() <= MAX_ENTRIES);
    }
}
