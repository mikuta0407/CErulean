#!/bin/bash
# armvec.sh [件数]: Go 版と Rust 版の CPU の差分テスト（1 命令の実行結果の突き合わせ）。
#
# Go 側（cpu/arm/vector_test.go）が splitmix64 の乱数列からランダムな CPU 状態と
# 命令語を作って 1 命令ずつ実行し、結果を tmp/armvec/go.txt に書く。Rust 側
# （rust/core/src/arm/tests.rs の vector_diff）は同じ乱数列から同じ入力を作り、
# 全レジスタ・退避領域・PSR・FSR/FAR・メモリの変化・停止の種類を比べる。
# Rust はデバッグビルドで走らせる（ゲストの演算のオーバーフローを panic で見つけるため）。
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
n=${1:-200000}
mkdir -p "$root/tmp/armvec"
out=$root/tmp/armvec/go.txt
(cd "$root" && CERULEAN_ARMVEC_OUT=$out CERULEAN_ARMVEC_N=$n go test ./cpu/arm/ -run TestGenVectors -count=1)
(cd "$root/rust" && CERULEAN_ARMVEC=$out cargo test -p cerulean-core vector_diff -- --nocapture 2>&1 |
  grep -E 'case|go:|rust:|vector_diff|test result|panicked')
