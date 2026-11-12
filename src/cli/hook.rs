// Claude Code hook integration: the hook install registrar and the internal
// hook-callback entry point it registers.
//
// The callback runs after every Write/Edit the agent performs, so it is bound
// by two contracts at once. The hook protocol's: a non-zero exit blocks the
// agent's turn, so this code never propagates an error -- every failure is a
// silent no-op. And the token budget's: everything printed to stdout lands in
// the model's context at the user's expense, so a clean file, a repeated write,
// and an unchanged issue set all print nothing.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use zhtw_mcp::engine::scan::{ProfileFilter, Scanner};
use zhtw_mcp::rules::ruleset::{Issue, Profile, ProfileConfig};

/// Cap on the hook payload read from stdin.
///
/// A Write payload carries the whole file in tool_input.content, so this has
/// to clear [MAX_FILE_BYTES] with room for JSON escaping, or a large file
/// would be truncated here into unparseable JSON and skipped when written
/// while the same file, reached through a small Edit payload, gets scanned.
/// Eight times the file cap covers the worst escaping expansion.
const MAX_PAYLOAD_BYTES: u64 = 8 * MAX_FILE_BYTES;

/// Files above this size are skipped rather than scanned: the callback sits
/// on the agent's write path and a document this large blows the latency
/// budget for a report nobody asked for.
const MAX_FILE_BYTES: u64 = 2 << 20;

/// Issue lines printed before collapsing into a "+N more" tail.
const MAX_REPORTED_ISSUES: usize = 3;

// Callback

/// Entry point for `internal hook-callback`.
///
/// Returns `()` rather than `Result` on purpose: `main` exits non-zero on any
/// `Err`, and in the hook protocol a non-zero exit is an intervention -- exit
/// 2 feeds stderr back to the model as a blocking error.  A linter that
/// cannot read its input has nothing to say, not something to block on.
pub(crate) fn run_hook_callback() {
    let mut payload = String::new();
    if std::io::stdin()
        .lock()
        .take(MAX_PAYLOAD_BYTES)
        .read_to_string(&mut payload)
        .is_err()
    {
        return;
    }
    if let Some(output) = callback_inner(&payload, &default_hook_cache_path()) {
        // Not println!: Rust ignores SIGPIPE, so a host that closed our stdout
        // early would turn the write failure into a panic and an exit of 101,
        // the non-zero exit this module exists to never produce.
        use std::io::Write;
        let _ = writeln!(std::io::stdout().lock(), "{output}");
    }
}

/// The whole callback as a function of its inputs, so tests can drive it
/// with a temp cache path.  `None` means print nothing.
fn callback_inner(payload_json: &str, cache_path: &Path) -> Option<String> {
    let payload = parse_payload(payload_json)?;
    if payload.tool_name != "Write" && payload.tool_name != "Edit" {
        return None;
    }
    if !lintable_extension(&payload.file_path) {
        return None;
    }

    // Everything up to the cache probe is deliberately cheaper than loading the
    // ruleset: the common case is a file this callback has already seen.
    let content = read_lintable(&payload.file_path)?;
    let content_hash = blake3::hash(content.as_bytes()).to_hex().to_string();

    // The store is opened before the fingerprint is computed because opening
    // creates a default override file where none exists: fingerprinting first
    // would hash the absence, and the first scan would then invalidate every
    // entry by bringing the file into being.
    let overrides_path = zhtw_mcp::rules::store::default_overrides_path();
    let store = zhtw_mcp::rules::store::OverrideStore::open(&overrides_path)
        .map_err(|e| tracing::warn!("hook-callback: overrides unavailable ({e})"))
        .ok();

    // The project the written file sits in, not the process working directory:
    // a hook has no meaningful cwd of its own, and the config that should
    // govern a file is the one beside it.
    let anchor = Path::new(&payload.file_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let config_path = zhtw_mcp::config::find_config_file(anchor);
    let project_cfg = config_path
        .as_deref()
        .and_then(|p| zhtw_mcp::config::ProjectConfig::from_file(p).ok());
    let tm_path = project_cfg
        .as_ref()
        .and_then(|c| c.translation_memory.as_ref().map(PathBuf::from))
        .unwrap_or_else(|| zhtw_mcp::rules::store::discover_tm_path(anchor));

    // The packs the scan will merge, by file, so installing or editing one
    // moves the digest. A pack decides findings now, so a cached verdict that
    // ignored it would answer for a rule set that has since changed. Through
    // PackStore, which validates the name: the config is project controlled,
    // and a name carrying a separator or ".." would otherwise reach a path
    // outside the packs directory. The scan already refuses those, and the
    // digest has to refuse the same ones.
    let pack_store =
        zhtw_mcp::rules::store::PackStore::new(zhtw_mcp::rules::store::default_packs_dir());
    let pack_paths: Vec<PathBuf> = project_cfg
        .as_ref()
        .and_then(|c| c.packs.as_deref())
        .unwrap_or_default()
        .iter()
        .filter_map(|name| pack_store.pack_path(name).ok())
        .collect();
    let fingerprint = rules_fingerprint(
        &overrides_path,
        config_path.as_deref(),
        &tm_path,
        &pack_paths,
    );

    let cache = HookCache::open(cache_path);
    let previous = cache.lookup(&payload.file_path);
    if previous.is_some_and(|e| e.content_hash == content_hash && e.fingerprint == fingerprint) {
        // Same bytes under the same rules. The scanner is deterministic, so the
        // verdict is whatever it was then: already reported, or clean.
        return None;
    }

    // Convert Simplified input before scanning, as every other entry point
    // does: the rules match traditional forms, and an agent writing Simplified
    // Chinese is exactly the case this hook exists to catch. JSON is the
    // exception, as in lint: converting the whole text would rewrite keys.
    let content_type = crate::cli::lint::content_type_for(
        project_cfg.as_ref().and_then(|c| c.content_type.as_deref()),
        &payload.file_path,
    );
    let mut scan_text = content;
    if content_type.converts_s2t()
        && zhtw_mcp::engine::zhtype::detect_chinese_type(&scan_text)
            == zhtw_mcp::engine::zhtype::ChineseType::Simplified
    {
        scan_text = zhtw_mcp::engine::s2t::S2TConverter::new().convert(&scan_text);
    }

    let glossary = project_cfg
        .as_ref()
        .and_then(|c| c.glossary.as_ref())
        .map(|g| zhtw_mcp::rules::glossary::ProjectGlossary {
            banned: g.banned.clone().unwrap_or_default(),
            preferred: g.preferred.clone().unwrap_or_default(),
            proper_nouns: g.proper_nouns.clone().unwrap_or_default(),
        })
        .unwrap_or_default();
    let tm = zhtw_mcp::rules::store::TranslationMemoryStore::open(&tm_path).ok();

    let issues = scan_file(
        &scan_text,
        &payload.file_path,
        store.as_ref(),
        &glossary,
        tm.as_ref(),
        project_cfg.as_ref(),
    );
    let issues_hash = issues_digest(&issues);

    // Content changed but the issue set did not (an edit elsewhere in the
    // file): stay silent rather than re-reporting what the model has already
    // been told. Not re-nagging about surviving issues is the token-budget
    // spec, not an oversight.
    let already_reported = previous.is_some_and(|e| e.issues_hash == issues_hash);

    HookCache::persist(
        cache_path,
        &payload.file_path,
        HookCacheEntry {
            content_hash,
            issues_hash,
            fingerprint,
            timestamp_secs: now_secs(),
        },
    );

    if issues.is_empty() || already_reported {
        return None;
    }
    Some(wrap_hook_output(&format_context(
        &payload.file_path,
        &issues,
    )))
}

/// The two payload fields the callback acts on.
struct HookPayload {
    tool_name: String,
    file_path: String,
}

/// Extract `tool_name` and `tool_input.file_path` from a PostToolUse
/// payload.  Probes a `Value` rather than deserializing a struct: the
/// payload carries session fields this code must keep ignoring as the hook
/// protocol grows them.
fn parse_payload(payload: &str) -> Option<HookPayload> {
    let value: Value = serde_json::from_str(payload).ok()?;
    let tool_name = value.get("tool_name")?.as_str()?.to_string();
    let file_path = value
        .get("tool_input")?
        .get("file_path")?
        .as_str()?
        .to_string();
    Some(HookPayload {
        tool_name,
        file_path,
    })
}

/// The extensions worth linting: the three the upstream spec names plus the
/// two spellings `ContentType::from_file_name` already maps to the same
/// scanners.
fn lintable_extension(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".md", ".markdown", ".txt", ".yml", ".yaml"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Read the file through one handle: open, stat that same fd, then read
/// capped, so the size gate bounds the bytes actually read.  Stat-then-open
/// by path would race a swapped symlink, and a FIFO stats as zero length yet
/// blocks the read forever -- on the agent's write path.  Non-regular files
/// are skipped outright.
fn read_lintable(path: &str) -> Option<String> {
    // Stat by path before open: a FIFO blocks at open(), not just at read, and
    // stat is the only probe that sees its type without touching it. Racy on
    // its own, so the metadata gating the read is taken again from the opened
    // fd below.
    let pre = std::fs::metadata(path).ok()?;
    if !pre.is_file() {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_FILE_BYTES).read_to_string(&mut text).ok()?;
    Some(text)
}

/// Scan one document the way the lint front end would: the project config's
/// own profile merged with the user's overrides, then the project glossary,
/// the translation memory and `ignore_terms` applied on top.  Skipping any of
/// those layers would have the hook re-reporting what `zhtw-mcp lint`
/// deliberately keeps quiet, which is the nagging this hook exists to remove.
/// The active packs are the ones the project config names, which are the only
/// packs a hook can know about since it has no flag context; their files are
/// part of the cache fingerprint, so editing a pack re-scans.  A `None` store
/// or a `None` config degrades to the embedded rules and the base profile: a
/// broken config file should not switch the hook off.
///
/// Info-severity issues are dropped from the result.  TM, glossary and
/// `ignore_terms` suppression all express themselves by downgrading to Info,
/// and advisory findings are not worth a nag on every write, so the severity
/// floor and honoring the suppressions are the same cut.
fn scan_file(
    content: &str,
    file_path: &str,
    store: Option<&zhtw_mcp::rules::store::OverrideStore>,
    glossary: &zhtw_mcp::rules::glossary::ProjectGlossary,
    tm: Option<&zhtw_mcp::rules::store::TranslationMemoryStore>,
    project: Option<&zhtw_mcp::config::ProjectConfig>,
) -> Vec<Issue> {
    let ruleset = match zhtw_mcp::rules::loader::load_embedded_ruleset() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("hook-callback: embedded ruleset failed to load: {e}");
            return Vec::new();
        }
    };
    let cfg = project_config(project);
    let filter = ProfileFilter::from_config(&cfg);

    let scanner = match store {
        Some(store) => {
            let packs =
                zhtw_mcp::rules::store::PackStore::new(zhtw_mcp::rules::store::default_packs_dir());

            // The project's packs, the same list lint unions in from the
            // config. Without them the hook stays quiet about pack-only
            // findings that lint reports on the very same file.
            let active: Vec<String> = project.and_then(|c| c.packs.clone()).unwrap_or_default();
            let (spelling, case) = zhtw_mcp::rules::store::build_merged_rules(
                &ruleset.spelling_rules,
                &ruleset.case_rules,
                store,
                &packs,
                &active,
            );
            Scanner::new_filtered(spelling, case, &filter)
        }
        None => Scanner::new_filtered(ruleset.spelling_rules, ruleset.case_rules, &filter),
    };

    let content_type = crate::cli::lint::content_type_for(
        project.and_then(|c| c.content_type.as_deref()),
        file_path,
    );
    let issues = scanner
        .scan_for_content_type_with_config(content, content_type, cfg)
        .issues;

    let mut issues = zhtw_mcp::rules::glossary::apply_glossary_with_coordinates(
        content,
        content_type,
        &cfg,
        issues,
        glossary,
    );
    if let Some(tm) = tm {
        tm.suppress_issues(&mut issues);
    }
    if let Some(terms) = project.and_then(|c| c.ignore_terms.as_deref()) {
        let ignore_set: std::collections::HashSet<&str> =
            terms.iter().map(String::as_str).collect();
        zhtw_mcp::rules::ignore::apply_ignore_set(&mut issues, &ignore_set);
    }
    issues.retain(|i| i.severity != zhtw_mcp::rules::ruleset::Severity::Info);
    issues
}

/// The effective config for a hooked file: the project's `.zhtw-mcp.toml`
/// resolved the same way `zhtw-mcp lint` resolves it, minus the axes that
/// only exist as command-line flags.  A file whose project turns a family off
/// or picks a spacing policy must not be nagged about what that choice
/// silences, or the hook contradicts the linter it speaks for.
fn project_config(project: Option<&zhtw_mcp::config::ProjectConfig>) -> ProfileConfig {
    // A profile name this build does not know degrades to base rather than
    // failing the way lint does. The two differ on purpose: lint is a command
    // whose exit code the author is reading, and a hook that refuses to run
    // breaks the write loop it sits in. Base is the safe direction, since it
    // enforces a subset of strict and can only under-report.
    let profile = project
        .and_then(|c| c.profile.as_deref())
        .and_then(Profile::from_str_strict)
        .unwrap_or(Profile::Base);
    let mut cfg = profile.config();
    if project.and_then(|c| c.relaxed).unwrap_or(false) {
        cfg = cfg.with_relaxed();
    }
    if let Some(policy) = project.and_then(|c| c.spacing) {
        cfg = cfg.with_spacing_policy(policy);
    }
    if project
        .and_then(|c| c.markdown.as_ref())
        .and_then(|m| m.exempt_blockquotes)
        .unwrap_or(false)
    {
        cfg = cfg.with_exempt_blockquotes(true);
    }

    // Last, like every other front end: subtraction applies after the profile
    // and the capability flags have resolved.
    if let Some(off) = project.and_then(|c| c.off.as_deref()) {
        cfg = cfg.with_disabled(off);
    }
    cfg
}

/// Location-independent digest of an issue set.
///
/// Deliberately excludes offsets and line numbers: an edit elsewhere in the
/// file shifts every position without changing what is wrong, and shifted
/// positions must not defeat the "issue set unchanged" suppression.  A
/// multiset, not a set: a second occurrence of the same term is new
/// information and changes the digest.
fn issues_digest(issues: &[Issue]) -> String {
    let mut keys: Vec<String> = issues
        .iter()
        .map(|i| {
            format!(
                "{}\u{1}{}\u{1}{}\u{1}{:?}",
                i.found,
                i.suggestions.first().map(String::as_str).unwrap_or(""),
                i.severity.name(),
                i.rule_type,
            )
        })
        .collect();
    keys.sort_unstable();
    blake3::hash(keys.join("\n").as_bytes())
        .to_hex()
        .to_string()
}

/// The capped report: one summary line, at most [MAX_REPORTED_ISSUES] issue
/// lines, and a "+N more" tail carrying a runnable command for the full
/// list.  The model knows the file has problems and has seen examples; the
/// other N are its to fetch, not this hook's to pay for.
fn format_context(file_path: &str, issues: &[Issue]) -> String {
    let file_name = Path::new(file_path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| file_path.to_string());

    let mut text = format!("zhtw-mcp: {} zh-TW issue(s) in {file_name}", issues.len());
    for issue in issues.iter().take(MAX_REPORTED_ISSUES) {
        match issue.suggestions.first() {
            Some(s) => text.push_str(&format!(
                "\n  {}:{} {} → {s}",
                issue.line, issue.col, issue.found
            )),
            None => text.push_str(&format!(
                "\n  {}:{} {} ({})",
                issue.line,
                issue.col,
                issue.found,
                issue.severity.name(),
            )),
        }
    }
    if issues.len() > MAX_REPORTED_ISSUES {
        // Quoted so the command survives a path with spaces: this line exists
        // to be run as pasted, or it has no reason to be here.
        text.push_str(&format!(
            "\n  +{} more: zhtw-mcp lint {}",
            issues.len() - MAX_REPORTED_ISSUES,
            shell_quote(file_path),
        ));
    }
    text
}

/// Wrap the report in the PostToolUse output envelope.  Plain stdout on exit
/// 0 is shown to the user but not the model; only `additionalContext` inside
/// `hookSpecificOutput` reaches the model's context.
fn wrap_hook_output(text: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PostToolUse",
            "additionalContext": text,
        }
    })
    .to_string()
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// Hook cache

const HOOK_CACHE_SCHEMA_VERSION: u32 = 1;

/// Entry cap.  One entry per distinct file path the agent has written; the
/// oldest are dropped on flush once past this.
const HOOK_CACHE_MAX_ENTRIES: usize = 256;

#[derive(Serialize, Deserialize, Clone)]
struct HookCacheEntry {
    content_hash: String,
    issues_hash: String,

    // Per entry rather than per cache file, because the project config and
    // translation memory it folds in are discovered from the linted file's own
    // directory: one shared fingerprint would have two projects invalidating
    // each other's entries on every alternation. The serde default never
    // matches a real fingerprint, so an entry written before the field existed
    // re-scans, which is the right reading of "the rules may have changed".
    #[serde(default)]
    fingerprint: String,
    timestamp_secs: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct HookCacheFile {
    schema_version: u32,
    entries: HashMap<String, HookCacheEntry>,
}

/// Duplicate-suppression state: file path -> what was last hashed and
/// reported.  Purely derived data, so unlike the scan cache a corrupt or
/// stale-schema file is started over without a backup -- losing it costs one
/// re-emission, not a judgment.
struct HookCache {
    entries: HashMap<String, HookCacheEntry>,
}

impl HookCache {
    fn open(path: &Path) -> Self {
        let entries = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<HookCacheFile>(&text).ok())
            .filter(|file| file.schema_version == HOOK_CACHE_SCHEMA_VERSION)
            .map(|file| file.entries)
            .unwrap_or_default();
        Self { entries }
    }

    fn lookup(&self, file_path: &str) -> Option<&HookCacheEntry> {
        self.entries.get(file_path)
    }

    /// Write one entry through to disk, best-effort.
    ///
    /// Follows the scan cache's flush discipline (non-blocking lock, skip on
    /// contention) because a hook must never wait: two callbacks racing means
    /// at most one duplicate emission later.  Under the lock the file is
    /// re-read and only this run's entry upserted, so parallel callbacks on
    /// different files do not clobber each other's records.
    fn persist(path: &Path, file_path: &str, entry: HookCacheEntry) {
        let Some(parent) = path.parent() else { return };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let lock_path = path.with_extension("json.lock");
        let Ok(lock) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
        else {
            return;
        };
        if fs2::FileExt::try_lock_exclusive(&lock).is_err() {
            return;
        }

        let mut file = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<HookCacheFile>(&text).ok())
            .filter(|f| f.schema_version == HOOK_CACHE_SCHEMA_VERSION)
            .unwrap_or_default();
        file.schema_version = HOOK_CACHE_SCHEMA_VERSION;
        file.entries.insert(file_path.to_string(), entry);

        while file.entries.len() > HOOK_CACHE_MAX_ENTRIES {
            let Some(oldest) = file
                .entries
                .iter()
                .min_by_key(|(_, e)| e.timestamp_secs)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            file.entries.remove(&oldest);
        }

        match serde_json::to_vec(&file) {
            Ok(bytes) => {
                if let Err(e) = zhtw_mcp::atomic::replace_file(path, &bytes) {
                    tracing::warn!("hook cache write failed: {e}");
                }
            }
            Err(e) => tracing::warn!("hook cache serialize failed: {e}"),
        }
        let _ = fs2::FileExt::unlock(&lock);
    }
}

/// A cheap proxy for "the effective rules": the bytes of every file that
/// shapes a verdict -- overrides, the project config, the translation
/// memory -- plus the binary's mtime and size, hashed together.
///
/// The honest fingerprint would be the merged ruleset hash the scan cache
/// keys on, but computing it means loading the ruleset, which is the cost
/// the cache's fast path exists to skip.  The proxy reads three small files
/// and one stat: an edited input changes its bytes, and a reinstalled or
/// rebuilt binary changes the stamp, which covers the embedded ruleset on a
/// rolling release where the version string does not move.  A touched but
/// identical binary invalidates for nothing, costing one re-scan per file;
/// a swapped binary with identical mtime and size is not a case that
/// happens outside of construction.
fn rules_fingerprint(
    overrides_path: &Path,
    config_path: Option<&Path>,
    tm_path: &Path,
    pack_paths: &[PathBuf],
) -> String {
    let mut hasher = blake3::Hasher::new();
    for path in [Some(overrides_path), config_path, Some(tm_path)]
        .into_iter()
        .flatten()
        .chain(pack_paths.iter().map(|p| p.as_path()))
    {
        if let Ok(bytes) = std::fs::read(path) {
            hasher.update(&bytes);
        }
        // Keep adjacent files from concatenating into the same digest.
        hasher.update(&[0]);
    }
    if let Ok(meta) = std::env::current_exe().and_then(std::fs::metadata) {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        hasher.update(&mtime.to_le_bytes());
        hasher.update(&meta.len().to_le_bytes());
    }
    hasher.finalize().to_hex().to_string()
}

/// Where the cache lives.  Disposable derived state goes under the cache
/// directory; `XDG_CACHE_HOME` is honored explicitly (when absolute) rather
/// than through `dirs`, mirroring `store::config_dir`, because `dirs`
/// ignores it on macOS and hermetic tests point it at a tempdir.
fn default_hook_cache_path() -> PathBuf {
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(dirs::cache_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("zhtw-mcp")
        .join("hook-cache.json")
}

// hook install

/// Entry point for `hook install`: merge the PostToolUse registration into
/// the user's Claude Code settings.  Unlike the callback this reports
/// failures normally -- an install that could not install should say so and
/// exit non-zero.
pub(crate) fn run_hook_install() -> Result<()> {
    let settings_path = claude_settings_path()?;

    // Follow a symlinked settings file to its target before writing. The atomic
    // write renames a temp file over the destination, which would swap a
    // dotfiles symlink for a regular file and let the next dotfiles sync
    // silently revert the registration.
    let settings_path = std::fs::canonicalize(&settings_path).unwrap_or(settings_path);
    let mut root = match std::fs::read_to_string(&settings_path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| {
            format!(
                "refusing to modify {}: existing content is not valid JSON",
                settings_path.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(e) => {
            return Err(e).context(format!("read {}", settings_path.display()));
        }
    };

    let command = hook_command_string()?;
    if ensure_hook_entry(&mut root, &command)? {
        let mut text = serde_json::to_string_pretty(&root)?;
        text.push('\n');
        zhtw_mcp::atomic::replace_file(&settings_path, text.as_bytes())
            .with_context(|| format!("write {}", settings_path.display()))?;
        eprintln!("installed PostToolUse hook in {}", settings_path.display());
    } else {
        eprintln!("hook already installed in {}", settings_path.display());
    }
    Ok(())
}

/// The Claude Code settings file: `$CLAUDE_CONFIG_DIR/settings.json`, with
/// `~/.claude` as the documented default location.
fn claude_settings_path() -> Result<PathBuf> {
    let config_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".claude")))
        .context("cannot locate the Claude Code config directory (no home directory)")?;
    Ok(config_dir.join("settings.json"))
}

/// The command the hook entry runs: the absolute path of the running
/// binary, quoted because Claude Code hands hook commands to a shell.
fn hook_command_string() -> Result<String> {
    let exe = std::env::current_exe().context("cannot resolve the zhtw-mcp binary path")?;
    Ok(format!(
        "{} internal hook-callback",
        shell_quote(&exe.display().to_string())
    ))
}

/// Quote one path for the shell that will split the hook command.
///
/// Single quotes on Unix, because they suppress every expansion where double
/// quotes still expand a dollar sign or a backtick in the path; an embedded
/// single quote goes through the close-escape-reopen idiom.  Double quotes on
/// Windows, whose file names cannot carry a quote to escape.
fn shell_quote(path: &str) -> String {
    if cfg!(windows) {
        format!("\"{path}\"")
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

/// Merge the hook registration into a settings document, preserving every
/// other setting.  The file is re-serialized rather than patched in place, so
/// its formatting is normalized and, since serde_json is built without
/// `preserve_order`, object keys come back sorted; the values are untouched.
///
/// Works on `serde_json::Value` rather than a typed struct on purpose: this
/// file belongs to Claude Code, not to zhtw-mcp, and deserializing it into
/// a struct would silently drop every field the struct does not know about.
/// Returns `Ok(false)` when the hook is already registered (any command that
/// names zhtw-mcp and ends at the callback entry point, so a re-install
/// after `make install` moved the binary does not stack a second entry).
fn ensure_hook_entry(root: &mut Value, command: &str) -> Result<bool> {
    let obj = root
        .as_object_mut()
        .context("settings.json top level is not a JSON object")?;
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .context("settings.json 'hooks' is not a JSON object")?;
    let post_tool_use = hooks
        .entry("PostToolUse")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .context("settings.json 'hooks.PostToolUse' is not a JSON array")?;

    let is_ours = |cmd: &str| {
        let trimmed = cmd.trim_end().trim_end_matches(['"', '\'']);
        trimmed.contains("zhtw-mcp") && trimmed.ends_with("internal hook-callback")
    };

    // Registered means the command sits under a matcher that actually fires for
    // Write and Edit: the same command left under a narrower matcher is a hook
    // that never runs, and reporting it as installed would make this command a
    // no-op exactly when the user reaches for it to repair that.
    let matcher_covers = |entry: &Value| match entry.get("matcher").and_then(Value::as_str) {
        None | Some("") | Some("*") | Some(".*") => true,
        Some(m) => {
            let mut alternatives = m.split('|');
            m.split('|').any(|t| t == "Write") && alternatives.any(|t| t == "Edit")
        }
    };
    let already_installed = post_tool_use.iter().any(|entry| {
        matcher_covers(entry)
            && entry
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|hooks| {
                    hooks
                        .iter()
                        .filter_map(|hook| hook.get("command")?.as_str())
                        .any(is_ours)
                })
    });
    if already_installed {
        return Ok(false);
    }

    // A new top-level entry rather than a hook appended to an existing matcher
    // group: the groups already there belong to other tools.
    post_tool_use.push(json!({
        "matcher": "Write|Edit",
        "hooks": [{ "type": "command", "command": command }],
    }));
    Ok(true)
}

#[cfg(test)]
#[path = "../../tests/unit/cli/hook/tests.rs"]
mod tests;
