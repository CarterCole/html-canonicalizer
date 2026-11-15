#!/bin/sh
# Build the in-page canonicalizer: the Rust core as WebAssembly, its JS glue, and the bundle.
#   web/pkg/   wasm-bindgen output (canon_core.js + canon_core_bg.wasm)
#   web/dist/  canon.js (ES module) + canon_core_bg.wasm, ready to serve
# Needs: rustup target wasm32-unknown-unknown, and a wasm-bindgen CLI whose version matches
# the wasm-bindgen crate pinned in crates/wasm/Cargo.toml.
set -eu
cd "$(dirname "$0")/.."
want=$(sed -n 's/^wasm-bindgen = "=\(.*\)"/\1/p' crates/wasm/Cargo.toml)
have=$(wasm-bindgen --version | awk '{print $2}')
if [ "$want" != "$have" ]; then
  echo "wasm-bindgen CLI is $have but the crate pins $want: cargo install wasm-bindgen-cli --version $want" >&2
  exit 1
fi
cargo build -p canon-wasm --target wasm32-unknown-unknown --profile wasm
wasm-bindgen --target web --out-dir web/pkg --out-name canon_core target/wasm32-unknown-unknown/wasm/canon_wasm.wasm
node web/bundle.mjs
