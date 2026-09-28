#!/bin/bash
# compare.sh [-n 回数] [-s 命令数]
#
# Go 版と Rust 版（release）の速度比較: リセットから指定の命令数（既定 4 億。
# 実処理の区間）を交互に n 回（既定 3）ずつ走らせ、命令/秒の最良値を比べる
# （この環境の計測は ±4% 程度ばらつくので交互に走らせて最良値で比べる。計画書 §6.3）。
# 実イメージが必要: CERULEAN_IMAGE（既定 tmp/images/PPC_USA.bin）。
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
n=3 steps=400000000
while getopts n:s: opt; do
  case $opt in
    n) n=$OPTARG ;;
    s) steps=$OPTARG ;;
    *) echo "usage: compare.sh [-n runs] [-s steps]" >&2; exit 2 ;;
  esac
done
image=${CERULEAN_IMAGE:-$root/tmp/images/PPC_USA.bin}
w=$root/tmp/bench
mkdir -p "$w"
go build -o "$w/c_go" ./cmd/cerulean
(cd rust && cargo build --release -q -p cerulean-cli)
: > "$w/r_go"
: > "$w/r_rust"
for _ in $(seq "$n"); do
  "$w/c_go" run -history 0 -stats -rtc 2006-01-02T15:04:05 -max-steps "$steps" "$image" 2>&1 >/dev/null |
    grep -o '[0-9.]*M steps/s' | cut -dM -f1 >> "$w/r_go"
  rust/target/release/cerulean run --history 0 --stats --quiet-uart --rtc 2006-01-02T15:04:05 --max-steps "$steps" "$image" 2>&1 |
    grep -o '[0-9.]*M steps/s' | cut -dM -f1 >> "$w/r_rust"
done
for b in go rust; do
  echo "$b best=$(sort -n "$w/r_$b" | tail -1)M steps/s  all=$(tr '\n' ' ' < "$w/r_$b")"
done
