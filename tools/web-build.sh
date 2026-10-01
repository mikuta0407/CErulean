#!/bin/bash
# web-build.sh: web クレートを wasm32-unknown-unknown の release でビルドし、
# wasm-bindgen の出力を 2 か所に置く（どちらも非コミット）:
#   web/pkg-node/   Node 用（--target nodejs。tools/golden/run-wasm.mjs が使う）
#   web/www/pkg/    ブラウザ用（--target web。web/www の計測ページが使う）
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
want=$(sed -n 's/^wasm-bindgen = "=\(.*\)"/\1/p' Cargo.toml)
got=$(wasm-bindgen --version | cut -d' ' -f2)
[ "$want" = "$got" ] || { echo "wasm-bindgen-cli $got, but Cargo.toml pins $want" >&2; exit 1; }
cargo build --locked -q -p cerulean-web --target wasm32-unknown-unknown --release
wasm=target/wasm32-unknown-unknown/release/cerulean_web.wasm
rm -rf web/pkg-node web/www/pkg
wasm-bindgen --target nodejs --out-dir web/pkg-node "$wasm"
wasm-bindgen --target web --out-dir web/www/pkg "$wasm"
# Service Worker が inline_js の依存もオフライン用に保存できるよう、実行資材の一覧を作る。
node --input-type=module <<'JS'
import { readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
const root = "web/www/pkg";
function files(dir, prefix = "") {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    const name = prefix + e.name;
    if (e.isDirectory()) return files(join(dir, e.name), name + "/");
    return e.isFile() && /\.(js|wasm)$/.test(e.name) ? [name] : [];
  });
}
writeFileSync(join(root, "assets.json"), JSON.stringify(files(root).sort()) + "\n");
JS
ls -la web/www/pkg/*.wasm
