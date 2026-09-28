#!/bin/bash
# check.sh: コミット前のローカルの確認（CI は置かない方針。2026-09 ユーザー確認）。
#   - Rust: fmt・clippy・テスト（ネイティブと wasm32-wasip1）
#   - web: wasm32-unknown-unknown でビルドし、wasm-bindgen の出力を Node で読み込む
# CERULEAN_IMAGE があれば実イメージのテストも走る（数十秒）。
# 必要なツール: rustup（rust/rust-toolchain.toml の版）、wasm-bindgen-cli
# （rust/Cargo.toml の wasm-bindgen と同じ版）、Node.js。
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

echo "== rust"
cd "$root/rust"
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p cerulean-core --target wasm32-wasip1

echo "== web (wasm32-unknown-unknown)"
cargo build -p cerulean-web --target wasm32-unknown-unknown --release
want=$(sed -n 's/^wasm-bindgen = "=\(.*\)"/\1/p' Cargo.toml)
got=$(wasm-bindgen --version | cut -d' ' -f2)
[ "$want" = "$got" ] || { echo "wasm-bindgen-cli $got, but Cargo.toml pins $want"; exit 1; }
out=$root/tmp/web-check
rm -rf "$out"
wasm-bindgen --target nodejs --out-dir "$out" target/wasm32-unknown-unknown/release/cerulean_web.wasm
node web/tests/node-smoke.mjs "$out"
echo "check: ok"
