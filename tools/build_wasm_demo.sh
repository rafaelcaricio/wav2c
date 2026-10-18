#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
cargo build --release --target wasm32-unknown-unknown --lib --manifest-path "$ROOT/Cargo.toml"
cp "$ROOT/target/wasm32-unknown-unknown/release/av2codec.wasm" "$ROOT/web/multiview_wasm/av2codec.wasm"
echo "wrote $ROOT/web/multiview_wasm/av2codec.wasm"
