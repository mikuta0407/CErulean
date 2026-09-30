#!/bin/bash
# run.sh <シナリオ名> <出力.jsonl> [追加の引数...]
#
# testdata/golden/scenarios/<シナリオ名>.scenario の定義どおりに Rust 版（release）を
# リセットから走らせ、結果の JSON Lines（testdata/golden/README.md）を書く。
# 追加の引数は CLI にそのまま渡す（例: --trace-hash 1000000）。
# CLI は $CERULEAN_BIN（既定: rust/target/release/cerulean。無ければビルド）。
# GOLDEN_RUNNER=wasm なら web クレートの wasm を Node で走らせる（run-wasm.mjs）。
#
# 実イメージのシナリオは環境変数 CERULEAN_IMAGE（PPC_USA.bin のパス）が必要。
# WM6 のシナリオ（wm6-*）は CERULEAN_IMAGE_WM6（WM6 の JPN 版（Professional Images の msi）の PPC_JPN.bin）。
# UART1 の出力は <出力>.uart に、CLI の標準エラーは <出力>.err に書く。
set -euo pipefail
if [ $# -lt 2 ]; then
  echo "usage: run.sh <scenario> <out.jsonl> [args...]" >&2
  exit 2
fi
name=$1 out=$2
shift 2
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
elif [ "$image" = '$CERULEAN_IMAGE_WM6' ]; then
  if [ -z "${CERULEAN_IMAGE_WM6:-}" ]; then
    echo "run.sh: $name needs CERULEAN_IMAGE_WM6 (path to the WM6 Professional JPN PPC_JPN.bin)" >&2
    exit 3
  fi
  image=$CERULEAN_IMAGE_WM6
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

if [ "${GOLDEN_RUNNER:-native}" = wasm ]; then
  # wasm（Node）で走らせる（tools/web-build.sh の出力を使う）。
  [ -f "$root/rust/web/pkg-node/cerulean_web.js" ] || "$root/tools/web-build.sh" >/dev/null
  node "$root/tools/golden/run-wasm.mjs" "$name" "$out" > "$out.uart" 2> "$out.err" || {
    echo "run.sh: $name: wasm runner exited with $? (see $out.err)" >&2
    exit 1
  }
  exit 0
fi
bin=${CERULEAN_BIN:-$root/rust/target/release/cerulean}
if [ ! -x "$bin" ]; then
  (cd "$root/rust" && cargo build --release -q -p cerulean-cli)
fi
args=(run --history 0 --quiet-uart --rtc "$rtc" --max-steps "$max_steps" --result "$out")
[ -n "$script" ] && args+=(--script "$gdir/scenarios/$script")
for c in $checkpoints; do args+=(--checkpoint "$c"); done
"$bin" "${args[@]}" "$@" "$image" > "$out.uart" 2> "$out.err" || {
  echo "run.sh: $name: exited with $? (see $out.err)" >&2
  exit 1
}
