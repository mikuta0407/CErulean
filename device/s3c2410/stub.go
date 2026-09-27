package s3c2410

import "github.com/mikuta0407/cerulean/bus"

// Stub は「書かれた値を保持して読み返すだけ」の汎用 MMIO ブロック。
// GPIO・クロック等、当面は読み書きが通ればブートが進む周辺機器に使う。
// 実動作（ステータスビットの変化、割り込み等）が必要になった機器は
// 専用実装に置き換える。
//
// レジスタ数は少ない（ブロックあたり数十個）ので map で疎に保持する。
type Stub struct {
	name string
	regs map[uint32]uint32 // ワードアラインしたオフセット → 値
}

var _ bus.Device = (*Stub)(nil)

// NewStub は名前付きの値保持スタブを作る。init はリセット値
// （ワードアラインオフセット → 値。nil 可）。
func NewStub(name string, init map[uint32]uint32) *Stub {
	regs := make(map[uint32]uint32, len(init))
	for k, v := range init {
		regs[k&^3] = v
	}
	return &Stub{name: name, regs: regs}
}

// Read はワード単位で保持した値から、アクセスサイズ分を切り出して返す。
// 未書き込みのレジスタは 0。
func (s *Stub) Read(off uint32, size int) uint32 {
	w := s.regs[off&^3]
	switch size {
	case 1:
		return (w >> ((off & 3) * 8)) & 0xFF
	case 2:
		return (w >> ((off & 2) * 8)) & 0xFFFF
	default:
		return w
	}
}

// Write は保持ワードの該当バイト/ハーフワードだけを書き換える。
func (s *Stub) Write(off uint32, size int, v uint32) {
	a := off &^ 3
	w := s.regs[a]
	switch size {
	case 1:
		shift := (off & 3) * 8
		w = w&^(0xFF<<shift) | (v&0xFF)<<shift
	case 2:
		shift := (off & 2) * 8
		w = w&^(0xFFFF<<shift) | (v&0xFFFF)<<shift
	default:
		w = v
	}
	s.regs[a] = w
}
