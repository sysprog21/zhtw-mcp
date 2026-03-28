// Google Translate calibration layer for cross-strait term verification.
//
// Pipeline (calibrate, not confirm):
//   1. Scanner finds issues; some have 'english' fields from matched rules.
//   2. Extract ±sentence context around each issue, deduplicate.
//   3. Single google_translate_raw() call (zh→en) on a sentinel-delimited payload.
//   4. For each issue with 'english' field, check if content-word anchors
//      appear in the corresponding translated segment.
//   5. Set issue.anchor_match = Some(true/false/None) as annotation.
//   6. No severity mutation. Pure annotation. Fail-open on API failure.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::time::Duration;

use crate::rules::ruleset::Issue;

/// Errors from the Google Translate API layer.
#[derive(Debug)]
pub enum TranslateError {
    /// Network or I/O error.
    Io(String),
    /// HTTP rate limit (429) or server error (5xx).
    RateLimit(u16),
    /// JSON parse error in the response.
    Parse(String),
}

impl fmt::Display for TranslateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "translate I/O error: {msg}"),
            Self::RateLimit(code) => write!(f, "translate rate-limited (HTTP {code})"),
            Self::Parse(msg) => write!(f, "translate parse error: {msg}"),
        }
    }
}

const GOOGLE_TRANSLATE_URL: &str = "https://translate.googleapis.com/translate_a/single";
const USER_AGENT: &str = "Mozilla/5.0 (compatible; zhtw-anchor/2.0)";

/// Maximum payload bytes sent to Google Translate in a single request.
/// The free endpoint rejects overly long URLs; keep well under the ~8KB
/// practical limit for GET query strings.
const MAX_PAYLOAD_BYTES: usize = 4096;

/// English stopwords to exclude from anchor matching.  These are so common
/// in any translation that matching on them provides zero signal.
const STOPWORDS: &[&str] = &[
    "a", "an", "the", "is", "are", "was", "were", "be", "been", "being", "have", "has", "had",
    "do", "does", "did", "will", "would", "shall", "should", "may", "might", "must", "can",
    "could", "of", "in", "to", "for", "with", "on", "at", "from", "by", "as", "into", "through",
    "during", "before", "after", "above", "below", "between", "under", "again", "further", "then",
    "once", "here", "there", "when", "where", "why", "how", "all", "each", "every", "both", "few",
    "more", "most", "other", "some", "such", "no", "nor", "not", "only", "own", "same", "so",
    "than", "too", "very", "just", "about", "also", "and", "but", "or", "if", "while", "that",
    "this", "these", "those", "it", "its", "he", "she", "they", "them", "we", "you", "i", "me",
    "my", "your", "his", "her", "our", "their", "what", "which", "who", "whom", "s", "t", "don",
    "doesn", "didn", "won", "wouldn", "shouldn", "couldn",
];

/// Tokenize English text into lowercase words, splitting on whitespace and
/// punctuation (preserving hyphens and apostrophes within words).  Hyphenated
/// and possessive tokens also emit their sub-parts.
pub(crate) fn tokenize_words(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for chunk in text.split(|c: char| {
        c.is_ascii_whitespace() || (c.is_ascii_punctuation() && c != '-' && c != '\'')
    }) {
        if chunk.is_empty() {
            continue;
        }
        tokens.push(chunk.to_string());
        // Emit sub-parts for hyphenated or possessive tokens.
        if chunk.contains('-') || chunk.contains('\'') {
            for sub in chunk.split(['-', '\'']) {
                if !sub.is_empty() && sub != chunk {
                    tokens.push(sub.to_string());
                }
            }
        }
    }
    tokens
}

/// Call Google Translate free endpoint (client=gtx).
///
/// Returns `TranslateError::Parse` if the response is valid JSON but contains
/// no translated text fragments (endpoint shape change).
pub(crate) fn google_translate_raw(
    text: &str,
    src: &str,
    tgt: &str,
) -> Result<String, TranslateError> {
    let url = format!(
        "{GOOGLE_TRANSLATE_URL}?client=gtx&sl={src}&tl={tgt}&dt=t&q={}",
        urlencoding::encode(text)
    );

    let agent = ureq::Agent::new_with_config(
        ureq::config::Config::builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build(),
    );
    let body_str = match agent.get(&url).header("User-Agent", USER_AGENT).call() {
        Ok(mut resp) => match resp.body_mut().read_to_string() {
            Ok(s) => s,
            Err(e) => return Err(TranslateError::Io(e.to_string())),
        },
        Err(ureq::Error::StatusCode(code @ (429 | 500..=599))) => {
            return Err(TranslateError::RateLimit(code));
        }
        Err(e) => {
            return Err(TranslateError::Io(e.to_string()));
        }
    };

    let body: serde_json::Value =
        serde_json::from_str(&body_str).map_err(|e| TranslateError::Parse(e.to_string()))?;

    // Response format: [[["translated text","source text",null,null,N], ...], ...]
    let mut result = String::new();
    if let Some(outer) = body.as_array() {
        if let Some(inner) = outer.first().and_then(|v| v.as_array()) {
            for item in inner {
                if let Some(arr) = item.as_array() {
                    if let Some(s) = arr.first().and_then(|v| v.as_str()) {
                        result.push_str(s);
                    }
                }
            }
        }
    }

    // #4: Detect endpoint shape change — valid JSON but no translated fragments.
    if result.is_empty() {
        return Err(TranslateError::Parse(
            "response JSON had no translated text fragments".into(),
        ));
    }

    Ok(result)
}

/// Result of a calibration run.
#[derive(Debug, Clone)]
pub struct CalibrateResult {
    /// Whether the API call succeeded.
    pub api_ok: bool,
    /// The full translated text (empty if API failed).
    pub translated: String,
    /// Number of tokens in the translation.
    pub token_count: usize,
    /// Issues where anchor was found in translation.
    pub matched: usize,
    /// Issues where anchor was NOT found in translation.
    pub unmatched: usize,
    /// Issues with no `english` field (left as None).
    pub no_english: usize,
}

/// Sentinel prefix for segment delimiters in the translation payload.
/// Chosen to be unlikely to appear in Chinese text and stable through
/// translation (numbers are preserved verbatim by Google Translate).
const SENTINEL_PREFIX: &str = "###SEG";

/// Extract ±sentence context around an issue offset, bounded by CJK sentence
/// punctuation (。！？) or paragraph breaks (\n), up to ~40 characters in
/// each direction.
fn extract_issue_context(text: &str, offset: usize) -> &str {
    let offset = offset.min(text.len());
    let offset = text.floor_char_boundary(offset);

    fn is_sentence_boundary(c: char) -> bool {
        matches!(c, '\n' | '\r' | '。' | '！' | '？' | '；')
    }

    // Scan backward by characters.
    let mut start = offset;
    let mut chars_back = 0;
    for (idx, c) in text[..offset].char_indices().rev() {
        if is_sentence_boundary(c) {
            start = idx + c.len_utf8();
            break;
        }
        start = idx;
        chars_back += 1;
        if chars_back >= 40 {
            break;
        }
    }

    // Scan forward by characters.
    let mut end = offset;
    let mut chars_fwd = 0;
    for (idx, c) in text[offset..].char_indices() {
        if is_sentence_boundary(c) {
            end = offset + idx;
            break;
        }
        end = offset + idx + c.len_utf8();
        chars_fwd += 1;
        if chars_fwd >= 40 {
            break;
        }
    }

    &text[start..end]
}

/// Filter anchor words to content words only (remove stopwords and
/// single-character tokens that are likely noise).
fn content_anchor_words(english: &str) -> Vec<String> {
    let stopset: HashSet<&str> = STOPWORDS.iter().copied().collect();
    english
        .split('/')
        .flat_map(|v| tokenize_words(v.trim()))
        .map(|t| t.to_lowercase())
        .filter(|w| w.len() > 1 && !stopset.contains(w.as_str()))
        .collect()
}

/// Calibrate issues by translating their context sentences and checking for
/// English anchor matches.
///
/// - `Some(true)`: anchor present in non-empty translation segment.
/// - `Some(false)`: anchor absent in non-empty translation segment.
/// - `None`: calibration not attempted (no `english` field, API failure,
///   empty input, empty translation, no content words in anchor).
///
/// Pure annotation. No severity mutation. Fail-open on API failure.
pub fn calibrate_issues(text: &str, issues: &mut [Issue]) -> CalibrateResult {
    let mut result = CalibrateResult {
        api_ok: false,
        translated: String::new(),
        token_count: 0,
        matched: 0,
        unmatched: 0,
        no_english: 0,
    };

    // Short-circuit: nothing to calibrate.
    if text.trim().is_empty() || issues.is_empty() {
        result.no_english = issues.len();
        return result;
    }

    // Collect unique context sentences for issues that have english fields.
    // Map each issue index to its context segment index.
    // #6: Use HashMap<String, usize> instead of HashSet + position() scan.
    let mut segments: Vec<String> = Vec::new();
    let mut segment_map: HashMap<String, usize> = HashMap::new();
    let mut issue_to_segment: Vec<Option<usize>> = Vec::with_capacity(issues.len());

    for issue in issues.iter() {
        if issue.english.is_none() {
            issue_to_segment.push(None);
            result.no_english += 1;
            continue;
        }

        let ctx = extract_issue_context(text, issue.offset).to_string();
        if ctx.trim().is_empty() {
            issue_to_segment.push(None);
            result.no_english += 1;
            continue;
        }

        let seg_idx = *segment_map.entry(ctx.clone()).or_insert_with(|| {
            let idx = segments.len();
            segments.push(ctx);
            idx
        });
        issue_to_segment.push(Some(seg_idx));
    }

    if segments.is_empty() {
        return result;
    }

    // #5: Cap payload size.  If the joined payload exceeds MAX_PAYLOAD_BYTES,
    // truncate to the segments that fit.  Issues referencing truncated segments
    // will get anchor_match = None (fail-open).
    // Also cap individual segments: a single oversized context must not blow
    // past the URL limit and disable calibration for the entire batch.
    let max_segment_bytes = MAX_PAYLOAD_BYTES / 2; // no single segment > half budget
    let max_segments = {
        let mut total = 0usize;
        let mut count = 0usize;
        for seg in &segments {
            // Account for sentinel + newline overhead per segment.
            let overhead = SENTINEL_PREFIX.len() + 6 + 1; // "###SEGnn\n"
            let seg_cost = seg.len().min(max_segment_bytes) + overhead;
            if total + seg_cost > MAX_PAYLOAD_BYTES && count > 0 {
                break;
            }
            total += seg_cost;
            count += 1;
        }
        count
    };

    // #2: Use sentinel markers instead of bare \n for segment delimiting.
    // Format: "###SEG0\n<segment0>\n###SEG1\n<segment1>\n..."
    // After translation, find markers to recover per-segment text.
    // Note: if user text literally contains "###SEG\d+", sentinel parsing could
    // be corrupted.  Acceptable risk — this pattern is vanishingly rare in zh-TW.
    let mut payload = String::new();
    let mut truncated_segments: HashSet<usize> = HashSet::new();
    for (i, seg) in segments.iter().enumerate() {
        if i >= max_segments {
            break;
        }
        if !payload.is_empty() {
            payload.push('\n');
        }
        // Truncate oversized segments at a char boundary to stay within budget.
        // Truncated segments get anchor_match = None (the anchor word might have
        // been beyond the truncation point — evaluating would create false negatives).
        let truncated = if seg.len() > max_segment_bytes {
            truncated_segments.insert(i);
            &seg[..seg.floor_char_boundary(max_segment_bytes)]
        } else {
            seg.as_str()
        };
        payload.push_str(&format!("{SENTINEL_PREFIX}{i}\n{truncated}"));
    }

    // Translate.
    let translation = match google_translate_raw(&payload, "zh", "en") {
        Ok(t) => t,
        Err(_) => {
            // Fail-open: all anchor_match = None.
            return result;
        }
    };

    result.api_ok = true;
    result.translated = translation.clone();

    // #2: Parse sentinel-delimited translation back into per-segment results.
    // Look for "###SEGn" markers (case-insensitive, Google may capitalize).
    let translated_segments = parse_sentinel_segments(&translation, segments.len());

    // Tokenize each segment.
    let segment_tokens: Vec<HashSet<String>> = translated_segments
        .iter()
        .map(|seg| {
            tokenize_words(seg)
                .into_iter()
                .map(|t| t.to_lowercase())
                .collect()
        })
        .collect();

    result.token_count = segment_tokens.iter().map(|s| s.len()).sum();

    // Check each issue against its corresponding segment.
    for (i, issue) in issues.iter_mut().enumerate() {
        let seg_idx = match issue_to_segment.get(i).copied().flatten() {
            Some(idx) => idx,
            None => continue, // no english field → anchor_match stays None
        };

        // Segment was dropped (payload cap) or individually truncated → no signal.
        // Truncated segments might have lost the anchor word beyond the cut point.
        if seg_idx >= max_segments || truncated_segments.contains(&seg_idx) {
            continue;
        }

        let english = match &issue.english {
            Some(e) => e.clone(),
            None => continue,
        };

        // Get the token set for this segment.
        let tokens = match segment_tokens.get(seg_idx) {
            Some(t) if !t.is_empty() => t,
            _ => continue, // empty/missing → no signal
        };

        // #1: Filter to content words only (no stopwords, no single chars).
        let anchors = content_anchor_words(&english);
        if anchors.is_empty() {
            // All anchor words were stopwords → no signal, not a false negative.
            continue;
        }

        let found = anchors.iter().any(|w| tokens.contains(w));

        issue.anchor_match = Some(found);
        if found {
            result.matched += 1;
        } else {
            result.unmatched += 1;
        }
    }

    result
}

/// Parse sentinel-delimited translation output.
///
/// Looks for `###SEGn` markers (case-insensitive) and extracts the text
/// between consecutive markers.  Returns a Vec indexed by segment number;
/// missing segments get empty strings.
fn parse_sentinel_segments(translation: &str, expected_count: usize) -> Vec<String> {
    let lower = translation.to_lowercase();
    let sentinel_lower = SENTINEL_PREFIX.to_lowercase();

    // Find all marker positions: (byte_offset, segment_number).
    let mut markers: Vec<(usize, usize)> = Vec::new();
    let mut search_from = 0;
    while let Some(pos) = lower[search_from..].find(&sentinel_lower) {
        let abs_pos = search_from + pos;
        let after_prefix = abs_pos + sentinel_lower.len();
        // Parse the segment number immediately after the prefix.
        let num_str: String = lower[after_prefix..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(n) = num_str.parse::<usize>() {
            let content_start = after_prefix + num_str.len();
            // Skip optional newline after marker.
            let content_start = if translation.as_bytes().get(content_start) == Some(&b'\n') {
                content_start + 1
            } else {
                content_start
            };
            markers.push((content_start, n));
        }
        search_from = abs_pos + sentinel_lower.len();
    }

    let mut result = vec![String::new(); expected_count];

    if markers.is_empty() {
        // No markers found — Google may have stripped them.  Fall back to
        // newline splitting for backward compat (best-effort).
        for (i, line) in translation.split('\n').enumerate() {
            if i < expected_count {
                result[i] = line.trim().to_string();
            }
        }
        return result;
    }

    // For each marker, extract text from content_start to the byte position
    // where the next marker begins (searching for the sentinel prefix in the
    // original case-insensitive text).
    for (idx, &(start, seg_num)) in markers.iter().enumerate() {
        if seg_num >= expected_count {
            continue;
        }
        let end = if idx + 1 < markers.len() {
            // Find where the next sentinel prefix starts in the original text.
            // markers[idx+1].0 is the content start (after "###SEGn\n"), so we
            // need to back up to find the "###SEG" prefix itself.
            let next_content = markers[idx + 1].0;
            // Search backwards from next_content for the sentinel prefix.
            lower[..next_content]
                .rfind(&sentinel_lower)
                .unwrap_or(next_content)
        } else {
            translation.len()
        };
        result[seg_num] = translation[start..end].trim().to_string();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_words_basic() {
        let tokens = tokenize_words("Hello, world! This is a test.");
        assert!(tokens.contains(&"Hello".to_string()));
        assert!(tokens.contains(&"world".to_string()));
        assert!(tokens.contains(&"test".to_string()));
    }

    #[test]
    fn tokenize_words_hyphenated() {
        let tokens = tokenize_words("multi-threaded server-side");
        assert!(tokens.contains(&"multi-threaded".to_string()));
        assert!(tokens.contains(&"multi".to_string()));
        assert!(tokens.contains(&"threaded".to_string()));
    }

    #[test]
    fn tokenize_words_possessive() {
        let tokens = tokenize_words("it's don't");
        assert!(tokens.contains(&"it's".to_string()));
        assert!(tokens.contains(&"it".to_string()));
        assert!(tokens.contains(&"s".to_string()));
    }

    // #3: Context extraction now counts chars, not bytes, and respects CJK punctuation.
    #[test]
    fn extract_context_respects_cjk_sentence_punctuation() {
        let text = "第一句話。這個軟件很好用。第三句話在這裡。";
        // Offset points into the second sentence (after 。).
        let second_sentence_offset = "第一句話。".len();
        let ctx = extract_issue_context(text, second_sentence_offset);
        // Should NOT include the first sentence (bounded by 。).
        assert!(!ctx.contains("第一句話"), "context leaked past 。: {ctx}");
        assert!(
            ctx.contains("軟件"),
            "context should contain the issue: {ctx}"
        );
    }

    #[test]
    fn extract_context_counts_chars_not_bytes() {
        // 50 CJK characters = 150 bytes.  With ±40 char window, context
        // should be bounded around ~40 chars each direction, not 40 bytes.
        let text: String = (0..50).map(|_| '測').collect();
        let ctx = extract_issue_context(&text, text.len() / 2);
        let char_count = ctx.chars().count();
        // Should be ~80 chars (40 back + 40 forward), not ~26 (80 bytes / 3).
        assert!(
            char_count >= 50,
            "context too short: {char_count} chars (byte-counting bug?)"
        );
    }

    #[test]
    fn extract_context_at_start() {
        let text = "軟件品質很好。";
        let ctx = extract_issue_context(text, 0);
        assert!(!ctx.is_empty());
    }

    #[test]
    fn extract_context_at_end() {
        let text = "測試文字";
        let ctx = extract_issue_context(text, text.len());
        assert!(!ctx.is_empty());
    }

    // #1: Stopword filtering prevents false matches on common words.
    #[test]
    fn content_anchor_words_filters_stopwords() {
        let words = content_anchor_words("if and only if");
        // "if", "and" are stopwords; "only" is also a stopword.
        assert!(
            words.is_empty(),
            "all stopwords should be filtered: {words:?}"
        );
    }

    #[test]
    fn content_anchor_words_keeps_content() {
        let words = content_anchor_words("memory (RAM)");
        assert!(words.contains(&"memory".to_string()));
        assert!(words.contains(&"ram".to_string()));
        assert!(!words.contains(&"a".to_string())); // single char filtered
    }

    #[test]
    fn content_anchor_words_multivariant() {
        let words = content_anchor_words("simulation/emulation");
        assert!(words.contains(&"simulation".to_string()));
        assert!(words.contains(&"emulation".to_string()));
    }

    // #2: Sentinel segment parsing.
    #[test]
    fn parse_sentinel_segments_basic() {
        let translation = "###SEG0\nThis is memory.\n###SEG1\nA friend afar.";
        let segs = parse_sentinel_segments(translation, 2);
        assert_eq!(segs.len(), 2);
        assert!(
            segs[0].contains("memory"),
            "seg0 should contain 'memory': {:?}",
            segs[0]
        );
        assert!(
            segs[1].contains("friend"),
            "seg1 should contain 'friend': {:?}",
            segs[1]
        );
    }

    #[test]
    fn parse_sentinel_segments_case_insensitive() {
        // Google Translate may capitalize the sentinel.
        let translation = "###Seg0\nTranslated text here.";
        let segs = parse_sentinel_segments(translation, 1);
        assert!(
            !segs[0].is_empty(),
            "should handle case-insensitive markers"
        );
    }

    #[test]
    fn parse_sentinel_segments_missing_markers() {
        // If Google strips all markers, fall back to newline splitting.
        let translation = "Line one translation.\nLine two translation.";
        let segs = parse_sentinel_segments(translation, 2);
        assert_eq!(segs.len(), 2);
        assert!(!segs[0].is_empty());
        assert!(!segs[1].is_empty());
    }

    #[test]
    fn calibrate_empty_text() {
        let mut issues = vec![];
        let r = calibrate_issues("", &mut issues);
        assert!(!r.api_ok);
        assert_eq!(r.matched, 0);
    }

    #[test]
    fn calibrate_empty_issues() {
        let mut issues = vec![];
        let r = calibrate_issues("Some text here", &mut issues);
        assert!(!r.api_ok);
        assert_eq!(r.matched, 0);
    }

    #[test]
    fn calibrate_no_english_field() {
        let mut issues = vec![Issue::new(
            0,
            6,
            "軟件",
            vec!["軟體".to_string()],
            crate::rules::ruleset::IssueType::CrossStrait,
            crate::rules::ruleset::Severity::Warning,
        )];
        // english is None by default
        assert!(issues[0].english.is_none());
        let r = calibrate_issues("這個軟件很好", &mut issues);
        assert_eq!(r.no_english, 1);
        assert!(issues[0].anchor_match.is_none());
    }

    // #1: Anchor with only stopwords should produce None, not false positive.
    #[test]
    fn calibrate_stopword_only_anchor_yields_none() {
        // "if and only if" — all stopwords.  Should not produce a false match.
        let _issues = [Issue::new(
            0,
            6,
            "當且僅當",
            vec!["若且唯若".to_string()],
            crate::rules::ruleset::IssueType::CrossStrait,
            crate::rules::ruleset::Severity::Warning,
        )
        .with_english("if and only if")];
        // We can't call the real API in unit tests, but we can verify the
        // content_anchor_words logic that gates the match.
        let anchors = content_anchor_words("if and only if");
        assert!(
            anchors.is_empty(),
            "stopword-only anchor should produce no content words"
        );
    }
}
