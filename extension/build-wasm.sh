#!/bin/sh
set -eu

cd "$(dirname "$0")/.."

if ! command -v wasm-pack >/dev/null 2>&1; then
  echo "wasm-pack is required. Install it with: cargo install wasm-pack" >&2
  exit 1
fi

rustup_path_dir=
cleanup() {
  if [ -n "$rustup_path_dir" ]; then
    rm -rf "$rustup_path_dir"
  fi
}
trap cleanup EXIT

make_temp_dir() {
  mktemp -d 2>/dev/null || mktemp -d -t zhtw-mcp-wasm
}

if command -v rustup >/dev/null 2>&1; then
  rustup_rustc="$(rustup which rustc 2>/dev/null || true)"
  rustup_cargo="$(rustup which cargo 2>/dev/null || true)"
  if [ -n "$rustup_rustc" ] && [ -n "$rustup_cargo" ]; then
    rustup target add wasm32-unknown-unknown
    rustup_path_dir="$(make_temp_dir)"
    ln -s "$rustup_rustc" "$rustup_path_dir/rustc"
    ln -s "$rustup_cargo" "$rustup_path_dir/cargo"
    PATH="$rustup_path_dir:$PATH"
    export PATH
  fi
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
