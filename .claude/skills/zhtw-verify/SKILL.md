---
name: zhtw-verify
description: How a zhtw-mcp change is validated - make check as the gate, the generated tables and the ruleset normalization that have to be current before it passes, the formatter chain in scripts/indent.sh, the git hooks and their own suite, the corpus thresholds a new rule has to clear, and the lanes that need a network or a browser and therefore sit outside. Use before calling work done, when a gate fails on drift rather than on a bug, when adding a rule or a test, or when a change touches src/engine.
---

# Validating a zhtw-mcp change

One gate, and it is the thing to run:

```sh
make check       # cargo test, clippy on three feature shapes, the formatters,
                 # the ruleset lint, the hook suite, shellcheck
make check-size  # the release binary has to stay under 20 MiB
```

Python 3 is a build requirement, not just a test requirement: `make` regenerates
`src/engine/s2t_data.rs` from the pinned OpenCC dictionaries before it builds.

## What skips, and why that matters

Every formatter lane skips when its tool is missing rather than failing, and
that is every tool `scripts/indent.sh` drives: `commentflow`, `cargo`, `black`,
`shfmt` and `python3`, plus `shellcheck` in the Makefile. That is right on a
laptop and wrong on a runner, so CI installs them on the Linux leg and sets
`ZHTW_REQUIRE_TOOLS=1` there, which turns a skip into a hard failure. The macOS
leg still skips, and the verdict line names what it skipped. A green local run
is therefore weaker evidence than a green CI run, and so is what the local gate
leaves out: CI also
holds a pull request's own commit messages to the rules, runs `cargo audit`
against `Cargo.lock`, builds the browser extension, and runs the whole suite on
macOS and Windows. Two of the three Windows-only regressions this project has
shipped were found by that last one and by nothing else.

## The indent gate is the one that surprises people

`scripts/indent.sh --check` copies the tree, runs the whole formatter chain
over the copy, and diffs: comment reflow with `commentflow`, then `cargo fmt`,
`black`, `shfmt`, and the `assets/ruleset.json` normalization that
`scripts/check-ruleset.py` owns. Checking the composition rather than each tool
is not a flourish. commentflow puts a blank line before a comment inside a
method chain and `cargo fmt` takes it straight back out, so `commentflow
--check` alone can never be satisfied on Rust. `make indent` runs the same
script with `--write`, so the fix for a failure is always that one command.

The chain runs to a fixed point rather than once. commentflow wraps a comment
against the indentation it finds, and `cargo fmt` or `shfmt` can then reindent
the block around it, leaving the comment wrapped for the width it used to have.
One pass would let `make indent` write a tree the gate rejects.

No formatter here takes a style flag. `shfmt` reads `.editorconfig` and
commentflow reads `ColumnLimit` from `.clang-format`, which exists for that one
number and for no C in this tree: without it, commentflow's search walks out of
the repository and the limit becomes whatever it finds or its own default. Both
files travel into the copy the check runs against, along with `Cargo.toml` and
`scripts/schema-facts.json`, so the copy is judged by the rules that wrote the
tree.

The copy carries a one-newline stub at `src/engine/s2t_data.rs`, because
rustfmt follows the module declaration that names it and the real file is
generated and gitignored.

## Drift is the usual failure

Two trees are generated, and a failure there is not a bug in your change; it
means the source moved and the output did not.

```sh
python3 scripts/gen-s2t-tables.py       # src/engine/s2t_data.rs, then rustfmt it
python3 scripts/check-ruleset.py        # rewrites assets/ruleset.json in place
python3 scripts/check-ruleset.py --lint # reports conflicts without rewriting

UPDATE_SCHEMA_FACTS=1 cargo test schema_facts_file_is_current  # scripts/schema-facts.json
```

Never hand-edit `src/engine/s2t_data.rs`; it is 43k generated lines. Never hand
format `assets/ruleset.json` either: `check-ruleset.py` owns its dedup, sort
and field order, and the indent gate compares the committed bytes against what
it would write.

## The corpus thresholds

`tests/corpus-evaluation.rs` has no `[[test]]` stanza, so cargo autodiscovers it
and the `cargo test` inside `make check` already runs every assertion in it.
`make corpus` is the same suite with `--nocapture`, and the only thing it adds is
the printed table of precision, recall, false-positive rate and safe-fix rate
over the synthetic corpora in `tests/corpus/`. Run it to read the numbers, not to
gate on them; a green `make check` has already cleared them.

Those assertions are the gate, and there are ten. Aggregate
precision at 90% or better; two native zh-TW false-positive rates, per fixture
and repeat-weighted, each at 5% or less; three safe-fix rates, 85% on the
AI-generated corpus and 99% on the zh-CN conversion and native ones; and a
per-corpus loop at the end gating recall and precision on each positive corpus,
94% and 91% for AI-generated, 98% and 96% for the zh-CN conversion.

A rule that is a false friend, valid zh-TW with a different meaning, stays
`disabled` or gets gated by `context_clues`, `negative_context_clues` or
`exceptions`. See zhtw-rules.

## The git hooks

Any `cargo build` installs them, through `build.rs`, and `make hooks` does it
on its own, unless `core.hooksPath` points outside the repository hooks
directory.
`scripts/git-pre-commit.sh` runs `rustfmt`, `black`, `shellcheck`, `shfmt`,
`commentflow` and the ruleset lint over a checkout of the index, so an unstaged
edit neither fails a commit nor rides along in one. It does not build, test or
regenerate anything; that is what the gate is for.
`scripts/git-commit-msg.sh` holds the message to the rules in zhtw-conventions,
and `scripts/git-pre-push.sh` replays them over commits a rebase or an amend
rewrote after the fact. CI runs the same script over a pull request's own
commits, so the rules bind someone who never installed the hooks as well.

The hooks have their own suite. `scripts/test-git-hooks.sh` builds a scratch
repository with `GIT_CONFIG_GLOBAL` and `GIT_CONFIG_SYSTEM` pointed at
`/dev/null`, so a contributor's own `core.hooksPath` cannot pull the cases into
a hooks directory somebody is using. The message and push cases run through the
installed wrapper and the rest call the scripts directly. It covers the messages
that must be rejected, a CJK subject accepted at
one width and rejected at another, the template splice above a `commit -v`
scissors line, a staged file failing while the same edit unstaged does not, a
staged file whose child module is the generated one, and a push carrying a
commit that skipped the hook. It runs in `make check`, so editing a hook without
running it is caught there.

## The comment prose gate

`scripts/check-comments.sh` is the other lane no formatter covers: no em dash
anywhere in a comment, and no backtick outside a `///` or `//!` doc comment.
It runs in `make check` and in the pre-commit hook over the staged files. Both
rules were swept out of the tree in one pass, and the gate is what keeps the
sweep from growing back a comment at a time. Run it on its own with
`./scripts/check-comments.sh`, or on named files.

## Writing a test

Integration suites sit beside the ones already in `tests/`, unit tests in the
module they cover. `src/engine/scan/tests_generated.rs` is misnamed: it holds
the hand-written scanner tests mechanically split out of `scan/mod.rs`, which is
why `mod.rs` ends in an `include!` of it. Edit it like any other test file, and
jump to the relevant test rather than reading the whole file.

Positions are byte offsets mapped back through NFC normalization and
pulldown-cmark event ranges. A test that computes a position on the
post-normalization string alone is asserting the wrong number, and it will pass
until the first composed character reaches it.

## What no gate here can enforce

Four things, worth knowing before trusting a green run:

- The pre-commit hook that runs is the one in the working tree, while the
  checks it performs read the index. Staging a change to a hook and restoring
  the working copy therefore commits through the old hook. The staged copy is
  not unexamined: it gets `sh -n`, `shellcheck`, `commentflow --check` and
  `shfmt -d` like any staged shell, and staging any hook script also runs the
  staged `scripts/test-git-hooks.sh`. What no hook can do is judge itself, so
  `make check` and the CI commit-log job remain the backstop.
- `scripts/check-comments.sh` reads full-line comments only. Telling a trailing
  comment from a string literal that happens to hold a backtick needs a parser
  rather than a grep, and a gate that fails a line nobody broke is a gate people
  learn to argue with. A trailing comment is short by nature and rarely carries
  either character, but it is unchecked.
- The commit-message rules are mechanical. Nothing checks that a body says the
  premise and the trade rather than retelling the diff, which is the rule that
  matters most and the one only a reader can apply. See zhtw-conventions.
- A rule's linguistic correctness. The corpus gates measure whether a rule
  fires where the fixtures say it should, not whether the zh-TW term is the one
  the Ministry of Education actually publishes. `python3
  scripts/check-ruleset.py --verify` checks a term against Wikipedia and the MoE
  dictionary, needs a network, and is not in any gate.

## What is deliberately outside the gate

```sh
cargo test --test anchor-benchmark -- --ignored  # needs network
python3 scripts/check-ruleset.py --verify        # Wikipedia and MoE dict lookups
sh extension/build-wasm.sh                       # needs wasm-pack
npm test --prefix extension                      # extension helpers
python3 scripts/measure-tokens.py                # telemetry calibration
```

CI runs the extension build and its tests, but only on a push to `main`, on a
pull request, or on a manual dispatch: a push to a topic branch runs nothing.
The two network lanes are run by hand when the vocabulary they check has moved.

`--verify` is the only thing in the binary that reaches the network, and
`ZHTW_NO_NETWORK` refuses it. Set that when running the gate somewhere the tree
should not be able to phone out; the run continues and reports `api_ok=false`.
