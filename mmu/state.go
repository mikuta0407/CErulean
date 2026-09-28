package mmu

import "github.com/mikuta0407/cerulean/snapshot"

// スナップショット（snapshot.Stateful）。
//
// ソフト TLB も保存する（ユーザー確認済み 2026-09）。TLB は「c8 で無効化
// されるまで古い変換が残る」という観測可能な状態を持つので、空で復元すると
// 通し実行と結果が食い違い得るため。ram（ページ実体のスライス）はポインタ
// なので保存せず、復元時に pa から引き直す。permR/permW は priv から
// 導出されるので保存しない。

var _ snapshot.Stateful = (*MMU)(nil)

func (m *MMU) StateVersion() uint16 { return 1 }

func (m *MMU) SaveState(e *snapshot.Encoder) {
	e.U32(m.ctrl)
	e.U32(m.ttb)
	e.U32(m.dacr)
	e.U32(m.fsr)
	e.U32(m.far)
	e.U32(m.pid)
	e.Bool(m.priv)
	e.I64(int64(m.fetchGrace))
	e.U32(m.graceNext)
	e.U32(m.prevCtrl)
	e.U32s(m.regs[:])
	e.U64(uint64(len(m.tlb)))
	for i := range m.tlb {
		t := &m.tlb[i]
		e.U32(t.tag)
		e.U32(t.pa)
		e.U8(t.perm)
	}
}

func (m *MMU) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	m.ctrl = d.U32()
	m.ttb = d.U32()
	m.dacr = d.U32()
	m.fsr = d.U32()
	m.far = d.U32()
	m.pid = d.U32()
	m.priv = d.Bool()
	m.fetchGrace = int(d.I64())
	m.graceNext = d.U32()
	m.prevCtrl = d.U32()
	d.U32sInto(m.regs[:])
	if n := d.U64(); n != uint64(len(m.tlb)) {
		d.Fail("mmu: TLB size %d does not match %d", n, len(m.tlb))
		return
	}
	pager, _ := m.phys.(RAMPager)
	for i := range m.tlb {
		t := &m.tlb[i]
		t.tag = d.U32()
		t.pa = d.U32()
		t.perm = d.U8()
		t.ram = nil
		if t.tag&tlbValid != 0 && pager != nil {
			t.ram = pager.RAMPage(t.pa)
		}
	}
	m.updatePermMask()
	m.resetCode() // デコードキャッシュは保存しない（CPU 側も空で復元する）
}
