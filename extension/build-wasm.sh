#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "wasm-pack is required. Install it from https://rustwasm.github.io/wasm-pack/installer/" >&2
  exit 1
fi

if [ ! -f src/engine/s2t_data.rs ]; then
  python3 scripts/gen-s2t-tables.py
  rustfmt src/engine/s2t_data.rs
fi

wasm-pack build . \
  --target web \
  --out-dir extension/dist \
  --out-name zhtw_mcp_wasm \
  --no-opt \
  --no-default-features \
  --features browser-wasm
