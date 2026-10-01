#!/bin/bash
# verify.sh [シナリオ名...]
#
# Rust 版（release）で基準シナリオを走らせ、testdata/golden/expected の期待値と比べる
# （cerulean goldencmp）。名前を省略すると全シナリオ。実イメージのシナリオは
# image=$<環境変数名> の環境変数（CERULEAN_IMAGE など）が無ければ飛ばす。出力は tmp/golden-out/。1 つでも食い違えば終了コード 1。
# CERULEAN_BIN で別のビルドを、GOLDEN_RUNNER=wasm で wasm（Node）を指定できる
# （wasm は tools/web-build.sh でビルドし直してから走らせる）。wasm では CERULEAN_JIT=1
# （または「閾値,まとめる数」）で JIT を有効にできる（tools/golden/run-wasm.mjs）。
#
# 食い違ったときの調べ方（計画書 §5.3）: 基準のビルドと調べるビルドで同じシナリオを
# --trace-hash 付きで走らせ（run.sh の追加の引数）、最初に食い違った行から区間を
# 絞り、最後は --trace の PC と命令語で比べる。
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
gdir=$root/testdata/golden
(cd "$root" && cargo build --release -q -p cerulean-cli) || exit 1
if [ "${GOLDEN_RUNNER:-native}" = wasm ]; then
  "$root/tools/web-build.sh" > /dev/null || exit 1
fi
cmp=$root/target/release/cerulean
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  for f in "$gdir"/scenarios/*.scenario; do names+=("$(basename "$f" .scenario)"); done
fi
work=$root/tmp/golden-out/${GOLDEN_RUNNER:-native}
mkdir -p "$work"
fail=0
for n in "${names[@]}"; do
  var=$(sed -n 's/^image=\$\([A-Z0-9_]*\).*/\1/p' "$gdir/scenarios/$n.scenario")
  if [ -n "$var" ] && [ -z "${!var:-}" ]; then
    echo "skip $n ($var is not set)"
    continue
  fi
  start=$(date +%s)
  if ! "$root/tools/golden/run.sh" "$n" "$work/$n.jsonl"; then
    echo "FAIL $n (run failed)"
    fail=1
    continue
  fi
  if out=$("$cmp" goldencmp "$gdir/expected/$n.jsonl" "$work/$n.jsonl"); then
    echo "ok   $n ($(( $(date +%s) - start ))s)"
  else
    echo "FAIL $n"
    echo "$out" | head -40
    fail=1
  fi
done
exit $fail
