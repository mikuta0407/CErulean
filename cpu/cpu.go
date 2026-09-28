// Package cpu は CPU コアの境界 interface を定義する。
//
// ここには実装を置かない。コア実装（cpu/arm）は将来別実装（Rust コアの
// バインディング等）に差し替えられるよう、他のパッケージはこの interface
// だけに依存すること。
package cpu

import "fmt"

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
// error の使い分け:
//   - *AbortError: MMU 起因のフォルト。CPU がデータアボート/プリフェッチ
//     アボート例外としてゲストに配送する。
//   - それ以外（bus.BusError 等）: エミュレータ側の不備（スタブ未実装など）
//     とみなし、エミュレーションを停止させる。
//
// リトルエンディアン固定（WinCE/ARM は LE）。
type Memory interface {
	Read8(addr uint32) (uint8, error)
	Read16(addr uint32) (uint16, error)
	Read32(addr uint32) (uint32, error)
	Write8(addr uint32, v uint8) error
	Write16(addr uint32, v uint16) error
	Write32(addr uint32, v uint32) error
}

// InstructionFetcher は Memory 実装が命令フェッチをデータリードと区別
// したい場合に追加実装する任意 interface。CPU コアは実装があれば命令
// フェッチに Fetch32 を使い、なければ Read32 に落ちる。
//
// MMU がこれを使う理由: 実機（ARM920T）では MMU 有効化の MCR の時点で
// 後続 2 命令がパイプラインにフェッチ済みで、WinCE のブートコードは
// 「MCR の直後の命令は物理アドレスのまま実行される」ことに依存している。
// フェッチ経路を区別できると、この 2 命令分だけ旧変換状態を使う近似で
// 再現できる。
type InstructionFetcher interface {
	Fetch32(addr uint32) (uint32, error)
}

// AbortError は MMU の変換・保護チェックで発生したフォルト。
// CPU はこれを ARM のアボート例外に変換する（cpu/arm の Step 参照）。
// Status/Domain は FSR（フォルトステータスレジスタ）にそのまま入る値
// （ARM ARM DDI 0100 のフォルトステータス符号）。
type AbortError struct {
	VA     uint32 // フォルトを起こした仮想アドレス
	Status uint8  // FSR[3:0]（例: 0101=セクション変換フォルト）
	Domain uint8  // FSR[7:4] に入るドメイン番号
	Write  bool   // 書き込みアクセスだったか
}

func (e *AbortError) Error() string {
	kind := "read"
	if e.Write {
		kind = "write"
	}
	return fmt.Sprintf("abort: %s at VA=%08X (status=%X domain=%X)", kind, e.VA, e.Status, e.Domain)
}

// Prober は Memory 実装が「状態を変えずに読める場合だけ読む」手段を
// 提供する任意 interface。CPU のアイドルループ検出（arm.Core.PollLoop）が、
// ループの命令とロード先を実際のアクセスと同じ経路で確かめるのに使う。
//
// ok=true を返すのは、同じアクセスを実際に行っても Memory 側の状態
// （ソフト TLB・フェッチ猶予・監視の表示など）が一切変わらない場合だけ。
// そうでなければ（TLB ミス・MMIO・監視中など）ok=false を返す。
type Prober interface {
	Probe32(addr uint32, fetch bool) (v uint32, ok bool)
}
