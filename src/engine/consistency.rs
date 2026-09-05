// Document-wide terminology consistency report.
//
// Groups scan issues by their english field (natural equivalence class), then
// for each group checks whether the canonical zh-TW form also appears elsewhere
// in the document. Mixed usage produces a Consistency diagnostic alerting the
// author that the same concept is referred to with both regional variants.
//
// TM-suppressed issues are excluded from consistency grouping: those are
// user-approved overrides, not inadvertent inconsistency.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::engine::excluded::{is_excluded, merge_ranges_pub, ByteRange};
use crate::rules::glossary::ProjectGlossary;
use crate::rules::ruleset::{Issue, IssueType, Severity};

/// One occurrence of a calque in the document: used to anchor the
/// consistency diagnostic.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConsistencyOccurrence {
    pub offset: usize,
    pub line: usize,
    pub col: usize,
    pub found: String,
}

/// Aggregated consistency record for one equivalence class.  All fields
/// are populated only when both the calque AND a canonical zh-TW form
/// appear in the same document.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ConsistencyGroup {
    /// English anchor (natural equivalence-class key).
    pub term_group: String,
    /// The TW-preferred form the linter recommends.
    pub preferred: String,
    /// All occurrences of the calque(s) in this group.
    pub occurrences: Vec<ConsistencyOccurrence>,
}

/// Top-level consistency report.  Empty `groups` means no mixed usage.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ConsistencyReport {
    pub groups: Vec<ConsistencyGroup>,
}

impl ConsistencyReport {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }
}

/// Build a consistency report from raw scan issues.
///
/// Algorithm:
///   1. Filter to CrossStrait / Confusable issues with non-empty
///      `english`.  Those are the cleanest equivalence-class anchors.
///   2. Skip issues whose severity is Info: TM-suppressed downgrades
///      land at Info; they are user-approved and should not count.
///   3. Group by `english`.  For each group, choose the TW-preferred
///      canonical form from `glossary.preferred` when that preferred
///      form appears outside the group's flagged spans; otherwise fall back
///      to the first suggestion.
///   4. Check whether that canonical form ALSO appears as a substring
///      outside those spans. If yes, both regional variants coexist → emit
///      a group.
pub fn compute_consistency_report(
    text: &str,
    issues: &[Issue],
    glossary: &ProjectGlossary,
) -> ConsistencyReport {
    let mut grouped: BTreeMap<String, Vec<&Issue>> = BTreeMap::new();

    for issue in issues {
        let eligible = matches!(
            issue.rule_type,
            IssueType::CrossStrait | IssueType::Confusable
        ) && issue.severity != Severity::Info;
        if !eligible {
            continue;
        }
        let Some(english) = issue.english.as_deref().filter(|e| !e.is_empty()) else {
            continue;
        };
        grouped.entry(english.to_string()).or_default().push(issue);
    }

    let mut report = ConsistencyReport::default();

    for (english, issues_in_group) in grouped {
        // Normalize once per group so repeated source forms do not require a
        // full issue scan for every candidate occurrence. Public callers may
        // supply overlapping spans or an unsorted issue list.
        let flagged_spans = merge_ranges_pub(
            issues_in_group
                .iter()
                .filter(|issue| issue.length > 0)
                .map(|issue| ByteRange {
                    start: issue.offset,
                    end: issue.offset.saturating_add(issue.length),
                })
                .collect(),
        );
        let canonical =
            preferred_canonical_for_group(text, &issues_in_group, &flagged_spans, glossary);
        let Some(canonical) = canonical else { continue };

        // 厄瓜多 inside 厄瓜多爾 is one usage, not two regional variants.
        if !has_independent_occurrence(text, &canonical, &flagged_spans) {
            continue;
        }

        let occurrences: Vec<ConsistencyOccurrence> = issues_in_group
            .iter()
            .map(|i| ConsistencyOccurrence {
                offset: i.offset,
                line: i.line,
                col: i.col,
                found: i.found.clone(),
            })
            .collect();

        report.groups.push(ConsistencyGroup {
            term_group: english,
            preferred: canonical,
            occurrences,
        });
    }

    report
}

fn has_independent_occurrence(text: &str, canonical: &str, flagged_spans: &[ByteRange]) -> bool {
    if canonical.is_empty() {
        return false;
    }
    let mut search_from = 0;
    while let Some(relative) = text[search_from..].find(canonical) {
        let start = search_from + relative;
        let end = start + canonical.len();
        if !is_excluded(start, end, flagged_spans) {
            return true;
        }

        // Advance one character so a rejected match cannot hide an overlapping
        // match whose full span lies outside the calque.
        search_from = text.ceil_char_boundary(start + 1);
    }
    false
}

fn preferred_canonical_for_group(
    text: &str,
    issues_in_group: &[&Issue],
    flagged_spans: &[ByteRange],
    glossary: &ProjectGlossary,
) -> Option<String> {
    // Prefer project glossary house terms when they also appear in the
    // document, but only when the rule already surfaced that term as a
    // canonical suggestion for this equivalence class. Short zh terms are too
    // collision-prone for edit-distance matching.
    if !glossary.preferred.is_empty() {
        for preferred in &glossary.preferred {
            if preferred.is_empty() {
                continue;
            }
            if glossary_preferred_matches_group(preferred, issues_in_group)
                && has_independent_occurrence(text, preferred, flagged_spans)
            {
                return Some(preferred.clone());
            }
        }
    }

    issues_in_group
        .iter()
        .find_map(|i| i.suggestions.first())
        .filter(|s| !s.is_empty())
        .cloned()
}

fn glossary_preferred_matches_group(preferred: &str, issues_in_group: &[&Issue]) -> bool {
    issues_in_group.iter().any(|issue| {
        issue
            .suggestions
            .iter()
            .any(|suggestion| suggestion == preferred)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn cross_strait(offset: usize, found: &str, suggestion: &str, english: &str) -> Issue {
        let mut issue = Issue::new(
            offset,
            found.len(),
            found,
            vec![suggestion.into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.english = Some(Arc::from(english));
        issue
    }

    #[test]
    fn empty_when_no_mixed_usage() {
        let text = "我們只用線程實作。";
        let issues = vec![cross_strait(3, "線程", "執行緒", "thread")];
        let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
        assert!(report.is_empty(), "no canonical 執行緒 in text → no group");
    }

    #[test]
    fn canonical_inside_calque_does_not_count_as_mixed_usage() {
        for text in ["厄瓜多爾", "厄瓜多爾和厄瓜多爾"] {
            let issues: Vec<Issue> = text
                .match_indices("厄瓜多爾")
                .map(|(offset, found)| cross_strait(offset, found, "厄瓜多", "Ecuador"))
                .collect();
            let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
            assert!(
                report.is_empty(),
                "only one regional form is present: {text}"
            );
        }
    }

    #[test]
    fn repeated_calques_with_unsorted_overlapping_spans_stay_unmixed() {
        let text = "厄瓜多爾 ".repeat(1000);
        let mut issues: Vec<Issue> = text
            .match_indices("厄瓜多爾")
            .map(|(offset, found)| cross_strait(offset, found, "厄瓜多", "Ecuador"))
            .collect();
        issues.push(cross_strait(0, "厄瓜多爾 厄瓜多爾", "厄瓜多", "Ecuador"));
        issues.reverse();
        let report = compute_consistency_report(&text, &issues, &ProjectGlossary::default());
        assert!(report.is_empty());
    }

    #[test]
    fn canonical_outside_calque_counts_before_or_after_it() {
        for text in ["厄瓜多和厄瓜多爾", "厄瓜多爾和厄瓜多"] {
            let offset = text.find("厄瓜多爾").unwrap();
            let issues = vec![cross_strait(offset, "厄瓜多爾", "厄瓜多", "Ecuador")];
            let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
            assert_eq!(report.groups.len(), 1, "both regional forms occur: {text}");
            assert_eq!(report.groups[0].preferred, "厄瓜多");
        }
    }

    #[test]
    fn canonical_overlapping_calque_edge_does_not_count() {
        let text = "甲乙丙";
        let issues = vec![cross_strait(0, "甲乙", "乙丙", "example")];
        let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
        assert!(
            report.is_empty(),
            "a partial overlap is not independent usage"
        );
    }

    #[test]
    fn independent_canonical_can_overlap_an_earlier_rejected_match() {
        let text = "哈哈哈";
        let issues = vec![cross_strait(0, "哈", "哈哈", "example")];
        let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
        assert_eq!(
            report.groups.len(),
            1,
            "the final two characters are independent"
        );
    }

    #[test]
    fn glossary_substring_does_not_displace_independent_default() {
        let text = "大實話和真心話";
        let mut issue = cross_strait(0, "大實話", "真心話", "blunt truth");
        issue.suggestions = vec!["真心話".into(), "實話".into()].into();
        let glossary = ProjectGlossary {
            preferred: vec!["實話".into()],
            ..ProjectGlossary::default()
        };
        let report = compute_consistency_report(text, &[issue], &glossary);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].preferred, "真心話");
    }

    #[test]
    fn fires_when_both_forms_present() {
        let text = "我們的線程很慢。執行緒設計需要重構。";
        let issues = vec![cross_strait(9, "線程", "執行緒", "thread")];
        let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
        assert_eq!(report.groups.len(), 1);
        let group = &report.groups[0];
        assert_eq!(group.term_group, "thread");
        assert_eq!(group.preferred, "執行緒");
        assert_eq!(group.occurrences.len(), 1);
        assert_eq!(group.occurrences[0].found, "線程");
    }

    #[test]
    fn groups_multiple_calques_for_same_english() {
        // Both 線程 and an alternative mainland form 線程數 share
        // english="thread". (Simulated for the test: real ruleset may differ.)
        let text = "我們的線程很慢，線程數量太多。執行緒重構。";
        let issues = vec![
            cross_strait(9, "線程", "執行緒", "thread"),
            cross_strait(24, "線程", "執行緒", "thread"),
        ];
        let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].occurrences.len(), 2);
    }

    #[test]
    fn ignores_info_severity_issues_tm_suppressed() {
        let text = "線程 ... 執行緒";
        let mut issue = cross_strait(0, "線程", "執行緒", "thread");
        issue.severity = Severity::Info;
        let report = compute_consistency_report(text, &[issue], &ProjectGlossary::default());
        assert!(report.is_empty(), "Info severity (TM-suppressed) skipped");
    }

    #[test]
    fn ignores_issues_without_english_anchor() {
        let text = "X ... Y";
        let mut issue = Issue::new(
            0,
            1,
            "X",
            vec!["Y".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.english = None;
        let report = compute_consistency_report(text, &[issue], &ProjectGlossary::default());
        assert!(report.is_empty());
    }

    #[test]
    fn separates_groups_by_english_anchor() {
        let text = "線程 執行緒 用戶 使用者";
        let issues = vec![
            cross_strait(0, "線程", "執行緒", "thread"),
            cross_strait(7, "用戶", "使用者", "user"),
        ];
        let report = compute_consistency_report(text, &issues, &ProjectGlossary::default());
        assert_eq!(report.groups.len(), 2);
        let groups: Vec<&str> = report
            .groups
            .iter()
            .map(|g| g.term_group.as_str())
            .collect();
        assert!(groups.contains(&"thread"));
        assert!(groups.contains(&"user"));
    }

    #[test]
    fn prefers_glossary_preferred_form_over_default_suggestion() {
        // The rule lists two acceptable TW forms; the glossary picks one as the
        // project-canonical. When both regional variants appear in the document
        // AND the glossary's choice is among the rule's suggestions
        // (matches_group), the consistency report surfaces the glossary's
        // choice instead of the rule's first suggestion.
        let text = "我們的線程很慢。緒程設計需要重構。";
        let mut issue = Issue::new(
            9,
            6,
            "線程",
            vec!["執行緒".into(), "緒程".into()],
            IssueType::CrossStrait,
            Severity::Warning,
        );
        issue.english = Some(Arc::from("thread"));
        let glossary = ProjectGlossary {
            preferred: vec!["緒程".into()],
            ..ProjectGlossary::default()
        };
        let report = compute_consistency_report(text, &[issue], &glossary);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(report.groups[0].preferred, "緒程");
    }

    #[test]
    fn glossary_preferred_outside_suggestions_falls_back_to_rule_suggestion() {
        let text = "我們的線程很慢。緒程設計需要重構。執行緒也要重構。";
        let issues = vec![cross_strait(9, "線程", "執行緒", "thread")];
        let glossary = ProjectGlossary {
            preferred: vec!["緒程".into()],
            ..ProjectGlossary::default()
        };
        let report = compute_consistency_report(text, &issues, &glossary);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(
            report.groups[0].preferred, "執行緒",
            "preferred terms outside rule suggestions must not hijack the group"
        );
    }

    #[test]
    fn edit_distance_neighbor_does_not_hijack_group() {
        // Regression guard for short zh terms: sharing one edge character with
        // the calque is not enough to join the same concept group.
        let text = "我們的線程很慢。執行緒設計需要重構。線性代數也出現。";
        let issues = vec![cross_strait(9, "線程", "執行緒", "thread")];
        let glossary = ProjectGlossary {
            preferred: vec!["線性".into()],
            ..ProjectGlossary::default()
        };
        let report = compute_consistency_report(text, &issues, &glossary);
        assert_eq!(report.groups.len(), 1);
        assert_eq!(
            report.groups[0].preferred, "執行緒",
            "must fall back to rule suggestion, not pick unrelated 線性"
        );
    }

    #[test]
    fn glossary_preference_does_not_leak_across_groups() {
        let text = "線程與使用者都出現在文件裡。執行緒也出現。";
        let issues = vec![
            cross_strait(0, "線程", "執行緒", "thread"),
            cross_strait(3, "用戶", "使用者", "user"),
        ];
        let glossary = ProjectGlossary {
            preferred: vec!["使用者".into()],
            ..ProjectGlossary::default()
        };
        let report = compute_consistency_report(text, &issues, &glossary);
        let thread_group = report
            .groups
            .iter()
            .find(|group| group.term_group == "thread")
            .expect("thread group should exist");
        assert_eq!(thread_group.preferred, "執行緒");
    }
}
