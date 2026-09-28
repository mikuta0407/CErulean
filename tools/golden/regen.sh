#!/bin/bash
# regen.sh [シナリオ名...]
#
# 基準の期待値 testdata/golden/expected/<名前>.jsonl を Go 版で作り直す
# （段階1 の完了後は Rust 版で作り直す。docs/rust-migration-plan.md §5.2）。
# 名前を省略すると全シナリオ。実イメージのシナリオは CERULEAN_IMAGE が必要で、
# 無ければ飛ばす。Go の版（go version）も表示する（基準を作り直すときに
# Go の版が変わっていないことを確かめるため。計画書 §8）。
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
gdir=$root/testdata/golden
impl=${GOLDEN_IMPL:-go}
go version
if [ "$impl" = go ]; then
  export CERULEAN_GO=${CERULEAN_GO:-$root/tmp/golden-bin/cerulean}
  mkdir -p "$(dirname "$CERULEAN_GO")"
  (cd "$root" && go build -o "$CERULEAN_GO" ./cmd/cerulean)
fi
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  for f in "$gdir"/scenarios/*.scenario; do names+=("$(basename "$f" .scenario)"); done
fi
work=$root/tmp/golden-out/regen
mkdir -p "$work" "$gdir/expected"
for n in "${names[@]}"; do
  if grep -q '^image=\$CERULEAN_IMAGE' "$gdir/scenarios/$n.scenario" && [ -z "${CERULEAN_IMAGE:-}" ]; then
    echo "skip $n (CERULEAN_IMAGE is not set)"
    continue
  fi
  start=$(date +%s)
  "$root/tools/golden/run.sh" "$impl" "$n" "$work/$n.jsonl"
  cp "$work/$n.jsonl" "$gdir/expected/$n.jsonl"
  echo "wrote expected/$n.jsonl ($(( $(date +%s) - start ))s)"
done
