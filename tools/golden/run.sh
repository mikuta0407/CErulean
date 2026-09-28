#!/bin/bash
# run.sh <impl> <シナリオ名> <出力.jsonl> [追加の引数...]
#
# testdata/golden/scenarios/<シナリオ名>.scenario の定義どおりに実装 <impl> を
# リセットから走らせ、結果の JSON Lines（testdata/golden/README.md）を書く。
# 追加の引数はその実装の CLI にそのまま渡す（例: go なら -trace-hash 1000000）。
#
# impl:
#   go    Go 版。$CERULEAN_GO（既定: リポジトリ直下の ./cerulean。無ければビルド）
#   rust  Rust 版（段階1 で CLI ができたら対応する）
#
# 実イメージのシナリオは環境変数 CERULEAN_IMAGE（PPC_USA.bin のパス）が必要。
# UART1 の出力は <出力>.uart に、CLI の標準エラーは <出力>.err に書く。
set -euo pipefail
if [ $# -lt 3 ]; then
  echo "usage: run.sh <go|rust> <scenario> <out.jsonl> [args...]" >&2
  exit 2
fi
impl=$1 name=$2 out=$3
shift 3
root=$(cd "$(dirname "$0")/../.." && pwd)
gdir=$root/testdata/golden
def=$gdir/scenarios/$name.scenario
[ -f "$def" ] || { echo "run.sh: no scenario $def" >&2; exit 2; }

# シナリオの定義（キー=値）を読む。
image='' image_sha256='' rtc='' max_steps='' script='' checkpoints=''
while IFS= read -r line || [ -n "$line" ]; do
  line=${line%%#*}
  [[ $line =~ ^[[:space:]]*$ ]] && continue
  k=${line%%=*} v=${line#*=}
  v=$(echo "$v" | sed 's/[[:space:]]*$//')
  case $k in
    image|image_sha256|rtc|max_steps|script|checkpoints) printf -v "$k" '%s' "$v" ;;
    *) echo "run.sh: $def: unknown key $k" >&2; exit 2 ;;
  esac
done < "$def"

if [ "$image" = '$CERULEAN_IMAGE' ]; then
  if [ -z "${CERULEAN_IMAGE:-}" ]; then
    echo "run.sh: $name needs CERULEAN_IMAGE (path to PPC_USA.bin)" >&2
    exit 3
  fi
  image=$CERULEAN_IMAGE
else
  image=$gdir/$image
fi
if [ -n "$image_sha256" ]; then
  got=$(sha256sum "$image" | cut -d' ' -f1)
  if [ "$got" != "$image_sha256" ]; then
    echo "run.sh: $image: sha256 $got, want $image_sha256" >&2
    exit 3
  fi
fi

case $impl in
  go)
    bin=${CERULEAN_GO:-$root/cerulean}
    if [ ! -x "$bin" ]; then
      (cd "$root" && go build -o "$bin" ./cmd/cerulean)
    fi
    args=(run -history 0 -rtc "$rtc" -max-steps "$max_steps" -result "$out")
    [ -n "$script" ] && args+=(-script "$gdir/scenarios/$script")
    for c in $checkpoints; do args+=(-checkpoint "$c"); done
    "$bin" "${args[@]}" "$@" "$image" > "$out.uart" 2> "$out.err" || {
      echo "run.sh: $name: go exited with $? (see $out.err)" >&2
      exit 1
    }
    ;;
  rust)
    # TODO(段階1): Rust の CLI ができたら、同じ定義から引数を組み立てる。
    echo "run.sh: rust implementation is not available yet" >&2
    exit 2
    ;;
  *)
    echo "run.sh: unknown implementation $impl" >&2
    exit 2
    ;;
esac
