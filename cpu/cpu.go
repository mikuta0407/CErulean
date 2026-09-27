// Package cpu は CPU コアの境界 interface を定義する。
//
// ここには実装を置かない。コア実装（cpu/arm）は将来別実装（Rust コアの
// バインディング等）に差し替えられるよう、他のパッケージはこの interface
// だけに依存すること。
package cpu

// CPU は 1 コアのプロセッサ。
//
// Step 単位の実行にしているのは、iOS で JIT が使えずインタプリタが前提のため。
// 将来ブロック単位実行を入れる場合も「エラーが出るまで進める」という
// この境界は維持できる（Step 相当をまとめて回すだけ）。
type CPU interface {
	// Reset は電源投入相当のリセットを行い、pc から実行を開始する状態にする。
	Reset(pc uint32)
	// Step は 1 命令実行する。未実装命令・バスエラー等はエラーで返り、
	// CPU はその命令の実行前の状態を保つ（呼び出し側が PC 等を表示できる）。
	Step() error
	// PC は現在のプログラムカウンタ（次に実行する命令のアドレス）。
	PC() uint32
	// Reg は現在のモードから見える汎用レジスタ r0..r15 の値。デバッグ/テスト用。
	Reg(n int) uint32
	// SetIRQ / SetFIQ は割り込み線のレベルを設定する（マイルストーン1では未使用）。
	SetIRQ(asserted bool)
	SetFIQ(asserted bool)
}

// Memory は CPU から見えるメモリ空間（MMU を含む）。アドレスは CPU が発行
// したままの値で、変換は Memory 実装（mmu パッケージ）の責務。
//
// error はアボート（存在しないアドレスへのアクセス等）を表す。当面は
// エミュレーションを停止させ、将来はデータアボート例外に変換する。
// リトルエンディアン固定（WinCE/ARM は LE）。
type Memory interface {
	Read8(addr uint32) (uint8, error)
	Read16(addr uint32) (uint16, error)
	Read32(addr uint32) (uint32, error)
	Write8(addr uint32, v uint8) error
	Write16(addr uint32, v uint16) error
	Write32(addr uint32, v uint32) error
}
