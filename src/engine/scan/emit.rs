// What a scan pass reads and writes.
//
// Every pass under this directory answers the same question about the same
// three things: the document, the mask saying which of it is not prose, and the
// list findings go into. They travelled as three separate parameters, which is
// what pushed the widest passes past ten arguments and put
// clippy::too_many_arguments allows on them.

use std::cell::OnceCell;
use std::ops::Deref;

use crate::engine::excluded::{mask_excluded, ByteRange};
use crate::rules::ruleset::Issue;

/// The document a pass scans, the mask over it, and the sink it writes to.
///
/// One parameter rather than three, so a pass that later needs a fourth piece
/// of shared state gains a field here instead of an argument in every
/// signature that reaches it.
///
/// Offsets in "issues" are into "text", so a pass working on a sentence or a
/// clause takes that slice as its own argument and leaves the document here.
/// Nothing rebases an emitter onto a sub-slice, and nothing should: the offsets
/// it collects would silently become relative to the slice.
pub(crate) struct Emitter<'a> {
    pub(crate) text: &'a str,
    pub(crate) excluded: &'a [ByteRange],
    pub(crate) issues: &'a mut Vec<Issue>,
    /// Backing cell for the blanked view that "parts" hands out.
    visible: OnceCell<String>,
}

impl<'a> Emitter<'a> {
    pub(crate) fn new(
        text: &'a str,
        excluded: &'a [ByteRange],
        issues: &'a mut Vec<Issue>,
    ) -> Self {
        Self {
            text,
            excluded,
            issues,
            visible: OnceCell::new(),
        }
    }

    /// The document, the mask, the sink and the blanked view, split so a pass
    /// can hold them side by side: "issues" is a mutable borrow of one field
    /// and
    /// the others borrow disjoint ones.
    pub(crate) fn parts(&mut self) -> (&'a str, &'a [ByteRange], Visible<'_>, &mut Vec<Issue>) {
        let visible = Visible {
            cell: &self.visible,
            text: self.text,
            excluded: self.excluded,
        };
        (self.text, self.excluded, visible, &mut *self.issues)
    }
}

/// The document with excluded ranges (code spans, URLs) blanked to spaces of
/// equal byte length, so a cue search never reads them as the author's prose
/// and every offset matches "text". It dereferences to the blanked text, built
/// on first use and once per emitter, so a pass that returns before it needs a
/// cue pays nothing. Slices of a sentence are already clean, because the
/// sentence index cuts at every excluded range; search this for paragraph
/// slices
/// and fixed windows.
pub(crate) struct Visible<'a> {
    cell: &'a OnceCell<String>,
    text: &'a str,
    excluded: &'a [ByteRange],
}

impl Deref for Visible<'_> {
    type Target = str;

    fn deref(&self) -> &str {
        if self.excluded.is_empty() {
            return self.text;
        }
        self.cell
            .get_or_init(|| mask_excluded(self.text, self.excluded))
    }
}
