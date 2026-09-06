#!/bin/sh

# Every build shape this tree ships, linted in the profile it ships in.
#
# Dead code is not a property of the source alone: it depends on which features
# are on and whether debug_assertions is. The browser-wasm shape once
# accumulated warnings nothing gated, and index_guard.rs once carried three
# items that only a release build could see were dead. Each lane below is a cell
# of that grid that something actually ships.
#
# The Windows CI leg cannot run make, so it calls this script instead of
# restating the list. That is the point: a lane added here reaches both.

set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$ROOT" || exit 1

# --lib rather than --all-targets on the release lanes: tests and benches are
# never built in release, and linting them there costs more than half again the
# lane's time for coverage of a shape nothing runs. --bins is in because the CLI
# and the MCP server ship from src/main.rs, which --lib does not see.
lanes="
--all-targets
--lib --no-default-features
--lib --no-default-features --features browser-wasm
--lib --bins --release
--lib --release --no-default-features --features browser-wasm
"

echo "$lanes" | while IFS= read -r lane; do
    [ -n "$lane" ] || continue
    echo "  CLIPPY  $lane"
    # Word splitting is what carries the lane's flags, so it is deliberate here.
    # shellcheck disable=SC2086
    cargo clippy $lane -- -D warnings
done
