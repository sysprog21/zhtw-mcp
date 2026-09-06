// What a scan pass reads and writes.
//
// Every pass under this directory answers the same question about the same
// three things: the document, the mask saying which of it is not prose, and the
// list findings go into. They travelled as three separate parameters, which is
// what pushed the widest passes past ten arguments and put
// clippy::too_many_arguments allows on them.

use crate::engine::excluded::ByteRange;
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
        }
    }
}
