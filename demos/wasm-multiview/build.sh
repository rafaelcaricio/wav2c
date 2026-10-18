#!/usr/bin/env bash
set -euo pipefail
DEMO_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$DEMO_DIR/../.." && pwd)
rustup target add wasm32-unknown-unknown >/dev/null 2>&1 || true
cargo build --release --target wasm32-unknown-unknown -p av2codec-wasm --manifest-path "$ROOT/Cargo.toml"
cp "$ROOT/target/wasm32-unknown-unknown/release/av2codec_wasm.wasm" "$DEMO_DIR/av2codec.wasm"
echo "wrote $DEMO_DIR/av2codec.wasm"
