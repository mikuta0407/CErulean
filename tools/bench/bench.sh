#!/bin/bash
# bench.sh [-n 回数] [-steps 命令数] [基準のリビジョン]
#
# Go 版の性能比較: 基準のリビジョン（既定 HEAD）と作業ツリーのビルドを交互に
# n 回（既定 3）ずつ走らせ、リセットから指定の命令数（既定 4 億。実処理の区間）
# の命令/秒の最良値を比べる。この環境の計測は ±4% 程度ばらつくので、交互に
# 走らせて最良値で比べる（計画書 §6.3）。
#
# 基準は git archive で tmp/bench/base に展開してビルドする（作業ツリーには触れない）。
# 実イメージが必要: CERULEAN_IMAGE（既定 tmp/images/PPC_USA.bin）。
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
n=3 steps=400000000
while getopts n:s: opt; do
  case $opt in
    n) n=$OPTARG ;;
    s) steps=$OPTARG ;;
    *) echo "usage: bench.sh [-n runs] [-s steps] [base-rev]" >&2; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
rev=${1:-HEAD}
image=${CERULEAN_IMAGE:-$root/tmp/images/PPC_USA.bin}
w=$root/tmp/bench
rm -rf "$w/base"
mkdir -p "$w/base"
git archive "$rev" | tar -x -C "$w/base"
(cd "$w/base" && go build -o "$w/c_base" ./cmd/cerulean)
go build -o "$w/c_work" ./cmd/cerulean
: > "$w/r_base"
: > "$w/r_work"
for _ in $(seq "$n"); do
  for b in base work; do
    "$w/c_$b" run -history 0 -stats -rtc 2006-01-02T15:04:05 -max-steps "$steps" "$image" 2>&1 >/dev/null |
      grep -o '[0-9.]*M steps/s' | cut -dM -f1 >> "$w/r_$b"
  done
done
for b in base work; do
  echo "$b best=$(sort -n "$w/r_$b" | tail -1)M steps/s  all=$(tr '\n' ' ' < "$w/r_$b")"
done
