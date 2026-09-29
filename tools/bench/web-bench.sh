#!/bin/bash
# web-bench.sh [-n 回数] [-s 命令数] [-j 閾値,まとめる数] [基準のリビジョン]
#
# bench.sh の wasm（Node）版: 基準のリビジョン（既定 HEAD）と作業ツリーの web クレートを
# release でビルドし、交互に n 回（既定 3）ずつリセットから指定の命令数（既定 4 億）まで
# 走らせて、命令/秒の最良値を比べる。-j で作業ツリー側だけ JIT を有効にする（段階5。
# 基準に JIT がない版も比べられるように）。-J なら両方で JIT を有効にする。
# 実イメージが必要: CERULEAN_IMAGE（既定 tmp/images/PPC_USA.bin）。
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
n=3 steps=400000000 jit="" jit_base=""
while getopts n:s:j:J: opt; do
  case $opt in
    n) n=$OPTARG ;;
    s) steps=$OPTARG ;;
    j) jit=$OPTARG ;;
    J) jit=$OPTARG jit_base=$OPTARG ;;
    *) echo "usage: web-bench.sh [-n runs] [-s steps] [-j|-J threshold,batch] [base-rev]" >&2; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
rev=${1:-HEAD}
w=$root/tmp/bench
rm -rf "$w/web-base"
mkdir -p "$w/web-base"
git archive "$rev" rust | tar -x -C "$w/web-base"
build() { # <rust ディレクトリ> <target ディレクトリ> <出力>
  (cd "$1" && CARGO_TARGET_DIR=$2 cargo build --release -q -p cerulean-web --target wasm32-unknown-unknown)
  rm -rf "$3"
  wasm-bindgen --target nodejs --out-dir "$3" "$2/wasm32-unknown-unknown/release/cerulean_web.wasm"
}
build "$w/web-base/rust" "$w/target-base" "$w/pkg_base"
build rust "$root/rust/target" "$w/pkg_work"
: > "$w/wr_base"
: > "$w/wr_work"
for _ in $(seq "$n"); do
  node tools/bench/wasm-run.mjs "$w/pkg_base" "$steps" $jit_base | grep -o '^[0-9.]*' >> "$w/wr_base"
  node tools/bench/wasm-run.mjs "$w/pkg_work" "$steps" $jit | grep -o '^[0-9.]*' >> "$w/wr_work"
done
for b in base work; do
  echo "$b best=$(sort -n "$w/wr_$b" | tail -1)M steps/s  all=$(tr '\n' ' ' < "$w/wr_$b")"
done
