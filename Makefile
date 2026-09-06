S2T_DATA := src/engine/s2t_data.rs
OPENCC_DICT_DIR := data/opencc
# CI supplies the pull request's base commit so the provenance gate compares
# against the ruleset before the change under test. Local runs retain HEAD,
# which also catches uncommitted additions. `?=` does not replace a defined
# empty environment variable, so normalize it before it reaches argparse.
RULESET_BASELINE_EFFECTIVE := $(or $(strip $(RULESET_BASELINE)),HEAD)

# What make tracks, because $(S2T_DATA) cannot record that the generator ran:
# the generator rewrites it only when the tables change, so after any Cargo.toml
# edit its mtime stays behind that prerequisite and every later `make` reruns
# the generator and rustfmt for nothing.  Touching $(S2T_DATA) instead would
# defeat the point, since cargo fingerprints by mtime and would rebuild the
# crate for a file whose bytes did not change.  The stamp lives beside the
# dictionary cache so `distclean` takes it too.
S2T_STAMP := $(OPENCC_DICT_DIR)/.tables-generated

# The stamp stands in for the generated file, so a hand-deleted s2t_data.rs has
# to invalidate it: otherwise the stamp still looks current and the build fails
# on a file nothing would regenerate.  `distclean` removes both, so this covers
# only the by-hand case.
ifeq ($(wildcard $(S2T_DATA)),)
.PHONY: $(S2T_STAMP)
endif

all: $(S2T_STAMP)
	cargo build --release

# gen-s2t-tables.py handles downloading from GitHub + code generation.
# Cargo.toml is a prerequisite too: the OpenCC commit is pinned in its
# [package.metadata.opencc] table, so changing which dictionaries we build
# from means editing one of these two files.  Make cannot depend on a single
# table inside a file, so an unrelated manifest edit reruns the generator;
# it rewrites s2t_data.rs only when the tables change, so nothing rebuilds.
$(S2T_STAMP): scripts/gen-s2t-tables.py Cargo.toml
	python3 scripts/gen-s2t-tables.py
	rustfmt $(S2T_DATA)
	@touch $@

clean:
	cargo clean

distclean: clean
	rm -f $(S2T_DATA)
	rm -rf $(OPENCC_DICT_DIR)

check: $(S2T_STAMP)
# The release binaries are built with --locked, so a Cargo.toml bump against a
# stale Cargo.lock has to fail here and not in the last job of a CI run.  Same
# lock cargo audit reads.
	cargo metadata --locked --format-version 1 >/dev/null
	cargo test
# One script owns the lint lanes, so what the Windows leg of CI runs is what
# this runs: the feature shapes and the profiles are a grid, and the shipped
# cells of it live in one list rather than in two files that drift.
	./scripts/clippy-lanes.sh
# One script owns the formatter chain, so what `make indent` rewrites is what
# this checks: comment reflow, then cargo fmt, black, shfmt and the ruleset
# normalizer.  It runs them against a copy of the tree, so a check never
# rewrites what it is judging.  Lanes whose tool is missing report a skip, which
# is why a green local run is weaker evidence than a green CI run.
	./scripts/indent.sh --check
	python3 scripts/check-ruleset.py --lint --baseline-ref $(RULESET_BASELINE_EFFECTIVE)
# The hooks are the one part of this tree with no other gate behind them: they
# run in a directory the test suite never looks at, and a hook that stops
# rejecting looks exactly like a hook that passes.
	./scripts/test-git-hooks.sh
# No em dash and no backtick outside a doc comment.  Neither formatter knows
# about either rule, so without this the tree grows them back one comment at a
# time.
	./scripts/check-comments.sh
# Optional in the same way the formatter lanes are, and honouring
# ZHTW_REQUIRE_TOOLS for the same reason they do: CI installs shellcheck on the
# leg that sets it, so a runner that stops carrying it has to fail rather than
# quietly stop linting shell.
	@if command -v shellcheck > /dev/null 2>&1; then \
		shellcheck $$(git ls-files --cached --others --exclude-standard -- '*.sh'); \
	elif [ -n "$$ZHTW_REQUIRE_TOOLS" ]; then \
		echo "shellcheck is required here and is not installed"; \
		exit 1; \
	else \
		echo "shellcheck not installed, skipping the shell lint"; \
	fi

check-size: all
	@SIZE=$$(wc -c < target/release/zhtw-mcp | tr -d ' '); \
	MAX=20971520; \
	if [ "$$SIZE" -gt "$$MAX" ]; then \
		echo "FAIL: release binary $$SIZE bytes exceeds 20 MiB budget ($$MAX)"; \
		exit 1; \
	else \
		echo "OK: release binary $$SIZE bytes (budget: $$MAX)"; \
	fi

indent: $(S2T_STAMP)
	@./scripts/indent.sh --write
	python3 scripts/check-ruleset.py --lint --baseline-ref $(RULESET_BASELINE_EFFECTIVE)

corpus: $(S2T_STAMP)
	cargo test --test corpus-evaluation -- --nocapture

.PHONY: all clean distclean check check-size corpus indent install uninstall \
	status hooks uninstall-hooks

# The hooks run the fast half of the gate over the staged content and hold the
# commit message to the rules the log already follows.  A release build installs
# them, because a hook nobody installed is a hook nobody runs; this target is for
# anyone who wants the install on its own, or wants it back after removing it.
hooks:
	@./scripts/install-git-hooks.sh

uninstall-hooks:
	@./scripts/install-git-hooks.sh --uninstall

install: all
	@./scripts/deploy.sh install

uninstall:
	@./scripts/deploy.sh uninstall

status:
	@./scripts/deploy.sh status
