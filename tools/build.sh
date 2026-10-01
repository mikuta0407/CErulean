#!/bin/bash
# wasm を生成してから Web 資材入りの単体配布用 cerulean をビルドする。
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
"$root/tools/web-build.sh"
cd "$root"
cargo build --locked --release -p cerulean-cli --features embedded-web
