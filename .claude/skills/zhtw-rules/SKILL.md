---
name: zhtw-rules
description: What the gates cannot tell you about adding or changing a zh-TW rule - why assets/ruleset.json is the only place a vocabulary rule lives, the false-friend problem and the four gates that answer it, the corpus thresholds a rule has to clear, why positions are byte offsets through NFC, and how a rule reaches the scanner, the fixer and the browser build. Use when adding or disabling a rule, when a rule fires on native zh-TW prose, when a fix lands at the wrong offset, or when touching src/engine/scan.
---

# Changing what zhtw-mcp flags

`assets/ruleset.json` is the source of truth. `build.rs` serializes it into the
binary with postcard, `scripts/check-ruleset.py` owns its dedup, sort and field
order, and `src/rules/schema.rs` is the type the two agree on through the
generated `scripts/schema-facts.json`. Hand formatting is rewritten and the
indent gate fails on it, so the loop is: edit, `python3
scripts/check-ruleset.py --lint`, `make indent`.

## The false friend is the whole problem

A `from` term that is also valid zh-TW with a different meaning is the failure
mode this project has to defend against, because it turns the linter into
something people switch off. 文件 is "file" in zh-CN and "document" in zh-TW.
字體 is a typeface here and a font file there. An ungated rule for either fires
on correct prose.

Four answers, in order of how much they cost the reader:

- `disabled: true` when the term cannot be judged from the sentence at all.
- `context_clues` and `negative_context_clues` when a nearby word settles it.
- `exceptions` when a fixed phrase is the only safe carve-out.
- `editorial_confidence` for the milder case, and `context_suggestions` when
  the correction itself differs by domain.

The two terms above took different answers, which is the point of having four.
文件 to 檔案 is `disabled`, because a sentence holding it reads correctly under
either meaning. 字體 to 字型 ships enabled behind `context_clues`, because the
typeface sense travels with words the rule can look for.

A rule that needs none of these is a rule where the zh-CN form has no zh-TW
reading, which is most of the vocabulary list and none of the hard cases.

## The corpus is the argument, not the opinion

The assertions in `tests/corpus-evaluation.rs` are what settles whether a rule
pays for itself. `make corpus` prints their table, but it does not gate: cargo
autodiscovers that target, so `make check` has already run every one of them.
There are ten, not the three the printed table draws the eye to:

- Aggregate precision at 90% or better.
- Two native zh-TW false-positive rates, per fixture and repeat-weighted, each
  at 5% or less, because each is blind to what the other catches.
- Three safe-fix rates: 85% on the AI-generated corpus, which is the figure
  `CLAUDE.md` records as the contract, and 99% on both the zh-CN conversion and
  the native corpora.
- Four per-corpus floors, added after the other six because recall was printed
  on every run and asserted nowhere, which let two commits rework AI detection
  unnoticed: AI-generated recall 94% and precision 91%, zh-CN conversion recall
  98% and precision 96%. Per corpus rather than aggregate, because zh-CN carries
  roughly twice the true positives and would mask an AI detector going quiet.

Three more assert each corpus is still big enough to mean anything. A rule that
drops a false-positive gate is a rule that fires on `native-zh-tw.json`, which
is exactly the prose it is supposed to leave alone.

`expected_issues` and `expected_fixed` in a corpus fixture are deliberately
independent: the scanner reports confusable and clue-gated rules that the
`lexical_safe` fixer will not touch, so an issue without a replacement is
correct and not an omission.

## Positions are byte offsets, and not the obvious ones

Every offset a rule produces is a byte offset into the original text, mapped
back through NFC normalization and, for markdown, through pulldown-cmark event
ranges. Computing one on the normalized string alone gives a number that is
right for ASCII, right for most CJK, and wrong the moment a composed character
or a markdown construct appears before it. `src/engine/normalize.rs` holds the
offset map and `src/engine/lineindex.rs` turns an offset into a line and
column, in UTF-16 code units by default because that is what LSP clients read.

## One rule, three consumers

A rule reaches the CLI, the MCP server and the browser extension. The last one
is the one that gets forgotten: the extension builds the library with
`browser-wasm` and no `native`, and anything touching `std::fs`, `dirs` or
`rayon` has to be behind `#[cfg(feature = "native")]` or that build breaks.
`make check` lints the two non-default feature shapes for exactly this reason.

The fixer is the second consumer worth naming. A scanner detection is not
automatically a fix: `src/fixer.rs` applies only what is safe to apply without
reading the sentence, and a rule whose correction depends on context belongs in
`context_suggestions` rather than in the safe fixer.

## Where the passes live

```text
src/engine/scan/spelling.rs      Vocabulary rules out of the ruleset
src/engine/scan/case_rule.rs     Casing of Latin technical terms
src/engine/scan/punctuation.rs   Half-width to full-width, context sensitive;
                                 emits the quote issues quotes.rs decided on
src/engine/scan/quotes.rs        Which quotation marks convert at all, the
                                 depth-based pairing fix, hierarchy validation
src/engine/scan/spacing.rs       CJK to Latin and CJK to digit spacing
src/engine/scan/ellipsis.rs      Non-standard ellipsis to the MoE …… form
src/engine/scan/repetition.rs    Consecutive duplicates, an ASR and paste tell
src/engine/scan/acronym.rs       Rejoins a spaced acronym, C P U to CPU
src/engine/scan/grammar.rs       A-not-A, bare 是, nominalization, prepositions
src/engine/scan/rule_ir.rs       The matcher spelling.rs drives; the 臺/台 family
                                 is variant rules behind variant_normalization
src/engine/scan/overlap.rs       Resolves detections that cover the same span
src/engine/s2t.rs                Simplified to traditional, from the OpenCC tables
```

`src/engine/scan/tests_generated.rs` is misnamed and is not generated: it holds
the hand-written scanner tests split out of `scan/mod.rs`. Add a scanner test
there or in the pass's own module, and add the corpus fixture separately when
the rule is meant to move a metric.
