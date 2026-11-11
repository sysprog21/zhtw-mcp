# Evaluation plan

The checked-in corpora under tests/corpus/ are synthetic: we wrote the inputs and
we wrote the expected answers. Precision and recall measured on them show
self-consistency, not accuracy. This plan lists
what would turn those numbers into evidence, and says what exists today.

Nothing here is a CI gate. Published translations are evidence, not ground truth,
so a disagreement with one is a list to read, not a failure.

## Done

- A pinned external benchmark. tests/external-corpus.rs scores convert against
  Microsoft's published VS Code zh-Hant strings, with OpenCC s2twp as the baseline.
  scripts/fetch-external-corpus.py fetches the Simplified and Traditional bundles
  at one upstream commit and checks every file against the sha256 pins in
  tests/fixtures/external-corpus-pins.json (190 files). It is ignored and needs
  network, so it is not part of make check. Run it with
  cargo test --test external-corpus -- --ignored --nocapture.
- A first baseline over 24,334 string pairs. Exact line match: zhtw-mcp 14.4%,
  OpenCC 18.4%. Edit-level precision, recall and F1, where an inserted character
  must match the reference: zhtw-mcp 71.0, 45.8, 55.7 percent; OpenCC 75.8, 49.0,
  59.5 percent. Exact match is low for both because
  the reference is an independent translation that rewords strings, not a
  conversion of the Simplified text. Disagreements are written to
  target/external-corpus/disagreements.tsv.
- A digest of the inputs. The corpus report in tests/corpus-evaluation.rs prints
  one inputs line per corpus: the case count and a blake3 digest of the sorted
  case ids and inputs. A quoted figure can be tied to the exact inputs behind it.

## Planned

Each item says what would count as done. None is scheduled.

1. Frozen answers. The digest covers inputs only. Extend it to the expected
   answers, record the digests in a checked-in file, and have the report say
   whether they changed since the last recorded run. Done when a change to an
   expected answer cannot happen without a visible diff of that file.
2. Pinned competitor versions. The baseline pins the data and names OpenCC s2twp,
   but not the OpenCC package version. Record it with the numbers. Add other
   converters only with a pinned version and configuration each.
3. Published hashes. Publish the pins and digests beside every result figure in
   the docs, so a reader can rerun the same inputs. Re-check with
   grep -n '[0-9]%' docs/*.md README.md whenever a figure is added.
4. Paired significance. Report McNemar's test on the per-pair outcome of
   zhtw-mcp against OpenCC (exact match or not), and a bootstrap confidence
   interval on the span precision, recall and F1. A bare percentage on 24,334
   pairs hides that the pairs share keys and files. Resample by file, not by pair.
5. More upstreams. The VS Code bundle is the smallest and most permissively
   licensed, so it went first. Add further pinned localizations only where the
   license allows redistribution of hashes and the Traditional side is Taiwan
   usage, not Hong Kong. Record each commit and per-file hashes the same way.
6. 國教院 vocabulary. Use the National Academy for Educational Research terminology
   as a source of reference answers for vocabulary rules, checked against the
   ruleset for terms we get wrong. Confirm the reuse terms before copying any
   data, and keep copied data out of the tree if they do not allow it.
7. Separate rule authors from answer authors. A rule and its expected answer
   should not come from the same person or the same session. Until a second
   author exists, prefer references from a published source (the benchmark above)
   over cases we wrote. Record who wrote each new corpus case in the commit
   message so the split can be audited.

## Reading the numbers

The span metric counts a source position as changed when a system replaced or
dropped the character, or inserted text at that gap, found by a longest common
subsequence alignment. It credits a conversion for the edits the reference made
and penalizes edits the reference did not. A reference that rewords a string
lowers both systems, so compare systems on the same pairs and do not read the
absolute figure as accuracy.
