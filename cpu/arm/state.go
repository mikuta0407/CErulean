package arm

import "github.com/mikuta0407/cerulean/snapshot"

// スナップショット（snapshot.Stateful）。
// mem・cp15・fetch32 は配線なので保存しない。特権状態は MMU 側が自分の
// 状態として保存するので、復元時に SetPrivileged で通知し直す必要もない。

var _ snapshot.Stateful = (*Core)(nil)

func (c *Core) StateVersion() uint16 { return 1 }

func (c *Core) SaveState(e *snapshot.Encoder) {
	e.U32s(c.regs[:])
	e.U32(uint32(c.cpsr))
	for _, p := range c.spsr {
		e.U32(uint32(p))
	}
	e.U32s(c.bankR8Usr[:])
	e.U32s(c.bankR8Fiq[:])
	e.U32s(c.bankR13[:])
	e.U32s(c.bankR14[:])
	e.Bool(c.irq)
	e.Bool(c.fiq)
}

func (c *Core) LoadState(d *snapshot.Decoder) {
	if !d.CheckVersion(1) {
		return
	}
	d.U32sInto(c.regs[:])
	c.cpsr = PSR(d.U32())
	for i := range c.spsr {
		c.spsr[i] = PSR(d.U32())
	}
	d.U32sInto(c.bankR8Usr[:])
	d.U32sInto(c.bankR8Fiq[:])
	d.U32sInto(c.bankR13[:])
	d.U32sInto(c.bankR14[:])
	c.irq = d.Bool()
	c.fiq = d.Bool()
}
