#!/bin/bash
# regen.sh [シナリオ名...]
#
# 基準の期待値 testdata/golden/expected/<名前>.jsonl を Rust 版（ネイティブの
# インタプリタ）で作り直す。名前を省略すると全シナリオ。実イメージのシナリオは
# CERULEAN_IMAGE（WM6 のシナリオは CERULEAN_IMAGE_WM6）が必要で、無ければ飛ばす。
#
# 期待値を変えるのはコアの動作を意図して変えたときだけ（理由をコミットに残す。
# 計画書 §1）。段階1 までは Go 版で作った値で、2026-09-28 に Rust 版と一致を確認して
# 基準を Rust 版に切り替えた。
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
gdir=$root/testdata/golden
(cd "$root/rust" && cargo build --release -q -p cerulean-cli)
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  for f in "$gdir"/scenarios/*.scenario; do names+=("$(basename "$f" .scenario)"); done
fi
work=$root/tmp/golden-out/regen
mkdir -p "$work" "$gdir/expected"
for n in "${names[@]}"; do
  if grep -q '^image=\$CERULEAN_IMAGE_WM6' "$gdir/scenarios/$n.scenario"; then
    if [ -z "${CERULEAN_IMAGE_WM6:-}" ]; then
      echo "skip $n (CERULEAN_IMAGE_WM6 is not set)"
      continue
    fi
  elif grep -q '^image=\$CERULEAN_IMAGE' "$gdir/scenarios/$n.scenario" && [ -z "${CERULEAN_IMAGE:-}" ]; then
    echo "skip $n (CERULEAN_IMAGE is not set)"
    continue
  fi
  start=$(date +%s)
  "$root/tools/golden/run.sh" "$n" "$work/$n.jsonl"
  cp "$work/$n.jsonl" "$gdir/expected/$n.jsonl"
  echo "wrote expected/$n.jsonl ($(( $(date +%s) - start ))s)"
done
