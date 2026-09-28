#!/bin/bash
# verify.sh <impl> [シナリオ名...]
#
# 実装 <impl>（go / rust）で基準シナリオを走らせ、testdata/golden/expected の
# 期待値と比べる（tools/goldencmp）。名前を省略すると全シナリオ。実イメージの
# シナリオは CERULEAN_IMAGE が無ければ飛ばす。出力は tmp/golden-out/<impl>/。
# 1 つでも食い違えば終了コード 1。
#
# 食い違ったときの調べ方（計画書 §5.3）: 同じシナリオを両実装で -trace-hash 付きで
# 走らせ（run.sh の追加の引数）、最初に食い違った行から区間を絞り、最後は
# -trace の PC と命令語で比べる。
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
gdir=$root/testdata/golden
[ $# -ge 1 ] || { echo "usage: verify.sh <go|rust> [scenario...]" >&2; exit 2; }
impl=$1
shift
if [ "$impl" = go ]; then
  export CERULEAN_GO=${CERULEAN_GO:-$root/tmp/golden-bin/cerulean}
  mkdir -p "$(dirname "$CERULEAN_GO")"
  (cd "$root" && go build -o "$CERULEAN_GO" ./cmd/cerulean) || exit 1
fi
cmp=$root/tmp/golden-bin/goldencmp
(cd "$root" && go build -o "$cmp" ./tools/goldencmp) || exit 1
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  for f in "$gdir"/scenarios/*.scenario; do names+=("$(basename "$f" .scenario)"); done
fi
work=$root/tmp/golden-out/$impl
mkdir -p "$work"
fail=0
for n in "${names[@]}"; do
  if grep -q '^image=\$CERULEAN_IMAGE' "$gdir/scenarios/$n.scenario" && [ -z "${CERULEAN_IMAGE:-}" ]; then
    echo "skip $n (CERULEAN_IMAGE is not set)"
    continue
  fi
  start=$(date +%s)
  if ! "$root/tools/golden/run.sh" "$impl" "$n" "$work/$n.jsonl"; then
    echo "FAIL $n (run failed)"
    fail=1
    continue
  fi
  if out=$("$cmp" "$gdir/expected/$n.jsonl" "$work/$n.jsonl"); then
    echo "ok   $n ($(( $(date +%s) - start ))s)"
  else
    echo "FAIL $n"
    echo "$out" | head -40
    fail=1
  fi
done
exit $fail
