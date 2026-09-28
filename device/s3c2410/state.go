package s3c2410

import (
	"sort"
	"time"

	"github.com/mikuta0407/cerulean/snapshot"
)

// 周辺機器のスナップショット（snapshot.Stateful）。
// コールバック（割り込み通知）・出力先・名前・forced ビット等は New 時の
// 配線・構成なので保存しない（各 _test の CheckFields で分類を明示している）。

var (
	_ snapshot.Stateful = (*Stub)(nil)
	_ snapshot.Stateful = (*INTC)(nil)
	_ snapshot.Stateful = (*PWMTimer)(nil)
	_ snapshot.Stateful = (*LCD)(nil)
	_ snapshot.Stateful = (*RTC)(nil)
	_ snapshot.Stateful = (*UART)(nil)
	_ snapshot.Stateful = (*DMAStub)(nil)
)

// ---- Stub ----

func (s *Stub) StateVersion() uint16 { return 1 }

// SaveState は保持値をオフセット順に書く（map の反復順に依存しないように
// して、同じ状態からは常に同じバイト列を作る）。
func (s *Stub) SaveState(e *snapshot.Encoder) {
	offs := make([]uint32, 0, len(s.regs))
	for off := range s.regs {
		offs = append(offs, off)
	}
	sort.Slice(offs, func(i, j int) bool { return offs[i] < offs[j] })
	e.U64(uint64(len(offs)))
	for _, off := range offs {
		e.U32(off)
		e.U32(s.regs[off])
	}
}

func (s *Stub) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	n := d.U64()
	if n > 0x10000 {
		d.Fail("stub %s: too many registers (%d)", s.name, n)
		return
	}
	s.regs = make(map[uint32]uint32, n)
	for i := uint64(0); i < n && d.Err() == nil; i++ {
		off := d.U32()
		s.regs[off] = d.U32()
	}
}

// ---- INTC ----

func (ic *INTC) StateVersion() uint16 { return 1 }

func (ic *INTC) SaveState(e *snapshot.Encoder) {
	for _, v := range []uint32{ic.srcpnd, ic.intmod, ic.intmsk, ic.priority,
		ic.intpnd, ic.intoffset, ic.subsrcpnd, ic.intsubmsk} {
		e.U32(v)
	}
}

// LoadState は値を戻すだけで recompute しない。CPU の IRQ/FIQ 線の
// レベルは CPU 側の状態として保存・復元されるため。
func (ic *INTC) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	for _, p := range []*uint32{&ic.srcpnd, &ic.intmod, &ic.intmsk, &ic.priority,
		&ic.intpnd, &ic.intoffset, &ic.subsrcpnd, &ic.intsubmsk} {
		*p = d.U32()
	}
}

// ---- PWMTimer ----

func (t *PWMTimer) StateVersion() uint16 { return 1 }

func (t *PWMTimer) SaveState(e *snapshot.Encoder) {
	e.U32(t.tcfg0)
	e.U32(t.tcfg1)
	e.U32(t.tcon)
	e.U32s(t.tcntb[:])
	e.U32s(t.tcmpb[:])
	for n := range t.cnt {
		e.I64(t.cnt[n])
		e.Bool(t.running[n])
	}
}

func (t *PWMTimer) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	t.tcfg0 = d.U32()
	t.tcfg1 = d.U32()
	t.tcon = d.U32()
	d.U32sInto(t.tcntb[:])
	d.U32sInto(t.tcmpb[:])
	for n := range t.cnt {
		t.cnt[n] = d.I64()
		t.running[n] = d.Bool()
	}
}

// ---- LCD ----

func (l *LCD) StateVersion() uint16 { return 1 }

func (l *LCD) SaveState(e *snapshot.Encoder) {
	e.U32s(l.regs[:])
	e.U32s(l.palette[:])
}

func (l *LCD) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	d.U32sInto(l.regs[:])
	d.U32sInto(l.palette[:])
}

// ---- RTC ----

func (r *RTC) StateVersion() uint16 { return 1 }

// base は UTC として扱う壁時計の値なので Unix 秒で保存する（秒未満は
// SetTime/Write で常に 0）。
func (r *RTC) SaveState(e *snapshot.Encoder) {
	e.I64(r.base.Unix())
	e.I64(r.elapsed)
	e.U32(r.rtccon)
	r.other.SaveState(e)
}

func (r *RTC) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	r.base = time.Unix(d.I64(), 0).UTC()
	r.elapsed = d.I64()
	r.rtccon = d.U32()
	r.other.LoadState(d) // 同じチャンク内なので版数も同じ（1）
}

// ---- UART ----

func (u *UART) StateVersion() uint16 { return 1 }

func (u *UART) SaveState(e *snapshot.Encoder) {
	for _, v := range []uint32{u.ulcon, u.ucon, u.ufcon, u.umcon, u.ubrdiv} {
		e.U32(v)
	}
}

func (u *UART) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	for _, p := range []*uint32{&u.ulcon, &u.ucon, &u.ufcon, &u.umcon, &u.ubrdiv} {
		*p = d.U32()
	}
}

// ---- DMAStub ----

func (dm *DMAStub) StateVersion() uint16 { return 1 }

func (dm *DMAStub) SaveState(e *snapshot.Encoder) { dm.stub.SaveState(e) }

func (dm *DMAStub) LoadState(d *snapshot.Decoder) { dm.stub.LoadState(d) }

// ---- ADC ----

var _ snapshot.Stateful = (*ADC)(nil)

func (a *ADC) StateVersion() uint16 { return 1 }

func (a *ADC) SaveState(e *snapshot.Encoder) {
	for _, v := range []uint32{a.adccon, a.adctsc, a.adcdly, a.dat0, a.dat1, a.rawX, a.rawY} {
		e.U32(v)
	}
	e.I64(a.converting)
	e.Bool(a.ecflg)
	e.Bool(a.penDown)
}

func (a *ADC) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	for _, p := range []*uint32{&a.adccon, &a.adctsc, &a.adcdly, &a.dat0, &a.dat1, &a.rawX, &a.rawY} {
		*p = d.U32()
	}
	a.converting = d.I64()
	a.ecflg = d.Bool()
	a.penDown = d.Bool()
}
