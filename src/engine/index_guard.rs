//! Debug-only guard against rebuilding a document-scoped index per paragraph.
//!
//! Four performance bugs in one week shared a shape: an index whose build
//! walks the whole document, called from inside a loop over paragraphs or
//! sentences. Each turned a linear pass quadratic, each was found by timing a
//! large file rather than by a test, and one was introduced while fixing
//! another. Tests cannot see it because the output is identical either way;
//! only the clock changes.
//!
//! So count the builds. A scan resets the counters and asserts on the way out
//! that no document-scoped index was built twice, which turns the class into a
//! failing test on any input with two paragraphs.
//!
//! Compiled out entirely unless `debug_assertions` is on.

/// A document-scoped index, named so a build site cannot misspell one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocIndex {
    Boundary,
    CloserTail,
    Attribution,
}

#[cfg(debug_assertions)]
mod imp {
    use super::DocIndex;
    use std::cell::Cell;

    // Only the counting build has a use for this table, and an inherent impl is
    // legal from any module of the defining crate, so it lives under the cfg
    // this file already draws rather than needing one of its own.
    impl DocIndex {
        const ALL: [Self; 3] = [Self::Boundary, Self::CloserTail, Self::Attribution];

        const fn slot(self) -> usize {
            self as usize
        }

        const fn name(self) -> &'static str {
            match self {
                Self::Boundary => "BoundaryIndex",
                Self::CloserTail => "CloserTailIndex",
                Self::Attribution => "AttributionIndex",
            }
        }
    }

    thread_local! {
        static BUILDS: Cell<[u32; DocIndex::ALL.len()]> = const { Cell::new([0; 3]) };
    }

    /// Record that a document-scoped index was built.
    pub(crate) fn note_build(which: DocIndex) {
        BUILDS.with(|b| {
            let mut counts = b.get();
            counts[which.slot()] += 1;
            b.set(counts);
        });
    }

    /// Start a scan: forget anything counted before it.
    pub(crate) fn reset() {
        BUILDS.with(|b| b.set([0; DocIndex::ALL.len()]));
    }

    /// End a scan: more than one build of the same index means it is being
    /// rebuilt inside a loop.
    pub(crate) fn assert_built_once_per_document() {
        let counts = BUILDS.with(|b| b.get());
        for which in DocIndex::ALL {
            assert!(
                counts[which.slot()] <= 1,
                "{} was built {} times in one scan. Build it once for the \
                 document and pass it down, rather than per paragraph or per \
                 sentence: that is what turns these passes quadratic on long \
                 input.",
                which.name(),
                counts[which.slot()]
            );
        }
    }
}

#[cfg(not(debug_assertions))]
mod imp {
    use super::DocIndex;

    #[inline(always)]
    pub(crate) fn note_build(_which: DocIndex) {}
    #[inline(always)]
    pub(crate) fn reset() {}
    #[inline(always)]
    pub(crate) fn assert_built_once_per_document() {}
}

pub(crate) use imp::{assert_built_once_per_document, note_build, reset};
