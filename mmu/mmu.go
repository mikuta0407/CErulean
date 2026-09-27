// Package mmu は ARM920T の MMU（CP15 System Control Coprocessor）を実装する。
//
// マイルストーン1では変換なしのパススルーで、CP15 は「ID レジスタを返し、
// 書き込みは記録するだけ」のスタブ。WinCE カーネルは起動早々に MMU を
// 有効化するため、ページテーブルウォークの実装が次の大きな課題になる。
// TODO(マイルストーン2以降): 変換テーブルウォーク、ドメイン、アクセス権限、
// アボート、high vectors（V ビット）。
package mmu

import (
	"fmt"

	"github.com/mikuta0407/cerulean/cpu"
)

// MMU は CPU とバスの間に入る仮想→物理変換層。cpu.Memory と
// arm.Coprocessor（CP15）の両方を実装する。
type MMU struct {
	phys cpu.Memory // 物理アドレス空間（bus）

	// CP15 レジスタ（スタブ）。crn ごとに最後に書かれた値を保持する。
	regs [16]uint32
}

var _ cpu.Memory = (*MMU)(nil)

func New(phys cpu.Memory) *MMU {
	return &MMU{phys: phys}
}

// Enabled は MMU（CP15 c1 の M ビット）が有効化されているか。
func (m *MMU) Enabled() bool { return m.regs[1]&1 != 0 }

// ---- cpu.Memory: 今はパススルー ----

func (m *MMU) translate(va uint32) (uint32, error) {
	if m.Enabled() {
		// TODO: 変換テーブルウォークを実装するまでは、MMU が有効化されたら
		// 止めて気づけるようにする（黙って素通しすると原因不明のクラッシュになる）。
		return 0, fmt.Errorf("mmu: address translation not implemented (MMU was enabled by CP15; VA=%08X)", va)
	}
	return va, nil
}

func (m *MMU) Read8(a uint32) (uint8, error) {
	pa, err := m.translate(a)
	if err != nil {
		return 0, err
	}
	return m.phys.Read8(pa)
}
func (m *MMU) Read16(a uint32) (uint16, error) {
	pa, err := m.translate(a)
	if err != nil {
		return 0, err
	}
	return m.phys.Read16(pa)
}
func (m *MMU) Read32(a uint32) (uint32, error) {
	pa, err := m.translate(a)
	if err != nil {
		return 0, err
	}
	return m.phys.Read32(pa)
}
func (m *MMU) Write8(a uint32, v uint8) error {
	pa, err := m.translate(a)
	if err != nil {
		return err
	}
	return m.phys.Write8(pa, v)
}
func (m *MMU) Write16(a uint32, v uint16) error {
	pa, err := m.translate(a)
	if err != nil {
		return err
	}
	return m.phys.Write16(pa, v)
}
func (m *MMU) Write32(a uint32, v uint32) error {
	pa, err := m.translate(a)
	if err != nil {
		return err
	}
	return m.phys.Write32(pa, v)
}

// ---- CP15（arm.Coprocessor を満たす。arm への import は不要）----

// ARM920T の Main ID レジスタ値（ARM920T TRM）。
// 0x41 = ARM Ltd, 920 = part number, rev は適当に 0。
const arm920MainID = 0x41129200

func (m *MMU) Read(opc1, crn, crm, opc2 uint8) (uint32, error) {
	if crn == 0 {
		// c0: ID レジスタ。opc2=0 が Main ID。
		// TODO: opc2=1 はキャッシュタイプレジスタ。必要になったら正しい値を返す。
		return arm920MainID, nil
	}
	return m.regs[crn&15], nil
}

func (m *MMU) Write(opc1, crn, crm, opc2 uint8, v uint32) error {
	// 書き込みは保持のみ（キャッシュ操作 c7、TLB 操作 c8 などは何もしなくてよい）。
	// c1 の M ビットが立つと translate 側で停止する。
	m.regs[crn&15] = v
	return nil
}
