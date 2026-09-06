// Case rule scan using Aho-Corasick (case-insensitive).
//
// Checks that terms like "JavaScript", "TypeScript", "API" are correctly cased,
// rejecting matches that are already in canonical or alternative form.

use super::emit::Emitter;
use crate::engine::excluded::is_excluded;
use crate::rules::ruleset::{Issue, IssueType, Severity};

use super::Scanner;

impl Scanner {
    /// Case rule scan using Aho-Corasick (case-insensitive).
    ///
    /// For each match, check:
    /// 1. The matched text is NOT already in a valid form (canonical term
    ///    or one of the listed alternatives).
    /// 2. The match has word boundaries: no adjacent ASCII letter on either
    ///    side (prevents matching "React" inside "Unreactive").
    pub(crate) fn scan_case(&self, em: &mut Emitter<'_>) {
        let text = em.text;
        let excluded = em.excluded;
        let issues = &mut *em.issues;

        let case_ac = match self.case_ac.as_ref() {
            Some(ac) => ac,
            None => return,
        };
        let bytes = text.as_bytes();

        for mat in case_ac.find_iter(text) {
            let start = mat.start();
            let end = mat.end();

            if is_excluded(start, end, excluded) {
                continue;
            }

            let found = &text[start..end];
            let rule = &self.case_rules[mat.pattern().as_usize()];

            // Check if the matched text is already correct (canonical or
            // alternative).
            if found == rule.term {
                continue;
            }
            if let Some(ref alts) = rule.alternatives {
                if alts.iter().any(|a| a == found) {
                    continue;
                }
            }

            // Word boundary check: no adjacent ASCII alpha.
            if start > 0 && bytes[start - 1].is_ascii_alphabetic() {
                continue;
            }
            if end < bytes.len() && bytes[end].is_ascii_alphabetic() {
                continue;
            }

            issues.push(Issue::new(
                start,
                end - start,
                found,
                vec![rule.term.clone()],
                IssueType::Case,
                Severity::Info,
            ));
        }
    }
}
