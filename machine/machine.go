// Package machine は SoC と周辺機器の組み合わせ（ボード構成）を定義する。
package machine

import (
	"github.com/mikuta0407/cerulean/cpu"
	"github.com/mikuta0407/cerulean/loader"
)

// Machine は 1 台のエミュレート対象マシン。構成（SoC の種類、RAM 量、
// アドレス変換規則）はこの interface の実装ごとに閉じる。
type Machine interface {
	Name() string
	CPU() cpu.CPU
	// LoadImage はローダーの中間表現を RAM に配置し、リセット後に
	// エントリポイントから実行される状態にする。イメージ内アドレス
	// （CE 仮想アドレス）から物理アドレスへの変換はここで行う。
	LoadImage(img *loader.Image) error
	// Reset は CPU と周辺機器をリセットする（LoadImage の後に呼ぶ）。
	Reset()
	// Step は 1 命令ぶんエミュレーションを進める。
	// TODO: タイマー等を実装したら、ここでデバイスの時間も進める。
	Step() error
}
