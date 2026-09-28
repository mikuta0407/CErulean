// Package machine は SoC と周辺機器の組み合わせ（ボード構成）を定義する。
package machine

import (
	"image"
	"io"

	"github.com/mikuta0407/cerulean/cpu"
	"github.com/mikuta0407/cerulean/loader"
)

// Machine は 1 台のエミュレート対象マシン。構成（SoC の種類、RAM 量、
// アドレス変換規則）はこの interface の実装ごとに閉じる。
//
// フロントエンド（CLI・ブラウザ型・将来の gomobile）はこの interface と
// emu パッケージだけで操作できる。実行は命令数を単位とした決定論的な
// もので、実時間との同期はフロントエンドの責務（コアは壁時計を見ない）。
type Machine interface {
	Name() string
	CPU() cpu.CPU
	// LoadImage はローダーの中間表現を RAM に配置し、リセット後に
	// エントリポイントから実行される状態にする。イメージ内アドレス
	// （CE 仮想アドレス）から物理アドレスへの変換はここで行う。
	LoadImage(img *loader.Image) error
	// Reset は CPU と周辺機器をリセットする（LoadImage の後に呼ぶ）。
	Reset()
	// Step は 1 命令ぶんエミュレーションを進める（デバイスの時間も進む）。
	Step() error
	// RunUntil は Steps() が limit に達するかエラーまで実行する。
	// 1 命令ずつ Step した場合と状態は完全に一致する（アイドル区間を
	// まとめて進める等の最適化は、状態を変えない範囲で実装が行う）。
	RunUntil(limit uint64) error
	// Steps はリセットからの実行命令数（= 仮想時間の基準）。
	Steps() uint64
	// InstructionsPerSecond は仮想時間 1 秒あたりの命令数。
	InstructionsPerSecond() uint64

	// 入力。呼び出した時点（命令境界）で適用される。
	TouchScreenSize() (w, h int)
	TouchDown(x, y int) error
	TouchMove(x, y int) error
	TouchUp()
	KeyNames() []string
	KeyDown(name string) error
	KeyUp(name string) error

	// Frame は現在の画面（LCD の表示内容）。表示が無効なら error。
	Frame() (*image.RGBA, error)

	// SaveSnapshot / LoadSnapshot は全状態の保存と復元。imageID は元
	// イメージの識別子（照合は呼び出し側）。LoadSnapshot は New 直後の
	// マシンに対して呼ぶ。
	SaveSnapshot(w io.Writer, imageID string) error
	LoadSnapshot(r io.Reader) (imageID string, err error)
}
