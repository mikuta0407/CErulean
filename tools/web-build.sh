#!/bin/bash
# web-build.sh: web クレートを wasm32-unknown-unknown の release でビルドし、
# wasm-bindgen の出力を 2 か所に置く（どちらも非コミット）:
#   rust/web/pkg-node/   Node 用（--target nodejs。tools/golden/run-wasm.mjs が使う）
#   rust/web/www/pkg/    ブラウザ用（--target web。rust/web/www の計測ページが使う）
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root/rust"
cargo build -q -p cerulean-web --target wasm32-unknown-unknown --release
wasm=target/wasm32-unknown-unknown/release/cerulean_web.wasm
rm -rf web/pkg-node web/www/pkg
wasm-bindgen --target nodejs --out-dir web/pkg-node "$wasm"
wasm-bindgen --target web --out-dir web/www/pkg "$wasm"
ls -la web/www/pkg/*.wasm
