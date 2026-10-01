#!/bin/bash
# regen.sh [シナリオ名...]
#
# 基準の期待値 testdata/golden/expected/<名前>.jsonl を Rust 版（ネイティブの
# インタプリタ）で作り直す。名前を省略すると全シナリオ。実イメージのシナリオは
# image=$<環境変数名> の環境変数（CERULEAN_IMAGE など）が必要で、無ければ飛ばす。
#
# 期待値を変えるのはコアの動作を意図して変えたときだけ（理由をコミットに残す。
# testdata/golden/README.md）。
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
gdir=$root/testdata/golden
(cd "$root" && cargo build --release -q -p cerulean-cli)
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  for f in "$gdir"/scenarios/*.scenario; do names+=("$(basename "$f" .scenario)"); done
fi
work=$root/tmp/golden-out/regen
mkdir -p "$work" "$gdir/expected"
for n in "${names[@]}"; do
  var=$(sed -n 's/^image=\$\([A-Z0-9_]*\).*/\1/p' "$gdir/scenarios/$n.scenario")
  if [ -n "$var" ] && [ -z "${!var:-}" ]; then
    echo "skip $n ($var is not set)"
    continue
  fi
  start=$(date +%s)
  "$root/tools/golden/run.sh" "$n" "$work/$n.jsonl"
  cp "$work/$n.jsonl" "$gdir/expected/$n.jsonl"
  echo "wrote expected/$n.jsonl ($(( $(date +%s) - start ))s)"
done
