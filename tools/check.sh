#!/bin/bash
# check.sh: コミット前のローカルの確認（CI は置かない方針。2026-09 ユーザー確認）。
#   - Rust: fmt・clippy・テスト（ネイティブと wasm32-wasip1）
#   - web: wasm32-unknown-unknown でビルドし、wasm-bindgen の出力を Node で読み込む。
#     JIT とインタプリタの差分テスト（web/tests/jit-diff.mjs。約 20 秒）も Node で走らせる
# CERULEAN_IMAGE があれば実イメージのテストも走る（数十秒）。
# 必要なツール: rustup（rust-toolchain.toml の版）、wasm-bindgen-cli
# （Cargo.toml の wasm-bindgen と同じ版）、Node.js。
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

echo "== rust"
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p cerulean-core --target wasm32-wasip1

echo "== web (wasm32-unknown-unknown)"
"$root/tools/web-build.sh"
out=$root/web/pkg-node
node web/tests/node-smoke.mjs "$out"
node web/tests/jit-diff.mjs "$out"
echo "== embedded Web assets"
cargo clippy -p cerulean-cli --all-targets --features embedded-web -- -D warnings
cargo test -p cerulean-cli --features embedded-web
echo "check: ok"
