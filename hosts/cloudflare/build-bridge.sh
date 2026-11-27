#!/usr/bin/env bash
set -euo pipefail

host="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd -- "$host/../.." && pwd)"
out="${1:-$host/test/generated}"
target_dir="${CARGO_TARGET_DIR:-$root/target}"

# Darwin's system ar can silently produce an empty SQLite archive from Wasm
# objects. Use an archiver that understands Wasm, including on local builds.
export AR_wasm32_unknown_unknown="${AR_wasm32_unknown_unknown:-llvm-ar}"
export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
command -v "$AR_wasm32_unknown_unknown" >/dev/null

cargo rustc --manifest-path "$root/Cargo.toml" --locked -p ion-core \
  --target-dir "$target_dir" --target wasm32-unknown-unknown \
  --features cloudflare-journal --lib --crate-type cdylib
wasm-bindgen --target web --no-typescript --out-dir "$out" \
  "$target_dir/wasm32-unknown-unknown/debug/ion_core.wasm"
