package arm

// ArchRegs は全モードのレジスタを「そのモードから見た値」で並べたもの。
// Go 版と Rust 版の一致確認（testdata/golden/README の CPU 状態のダンプ）に
// 使う。Core の内部の持ち方（現在モードの値は regs に、他のモードの値は
// 退避領域に置く）に依存しない形にするため、現在モードの分は regs から、
// それ以外は退避領域から取る（退避領域のうち現在モードの枠は古い値なので
// 使わない）。
type ArchRegs struct {
	R    [16]uint32 // 現在モードから見える r0〜r15（r15 は次に実行する命令のアドレス）
	Usr  [7]uint32  // usr/sys の r8〜r14
	Fiq  [7]uint32  // fiq の r8〜r14
	Irq  [2]uint32  // irq の r13, r14
	Svc  [2]uint32  // svc の r13, r14
	Abt  [2]uint32  // abt の r13, r14
	Und  [2]uint32  // und の r13, r14
	CPSR uint32
	SPSR [5]uint32 // fiq, irq, svc, abt, und の順
}

// ArchRegs は現在の全モードのレジスタを返す（状態は変えない）。
// CPSR のモードが不正な値のときは usr として扱う（curBank と同じ規則。
// その場合 regs は直前の正しいモードの値のままだが、WinCE では起きない）。
func (c *Core) ArchRegs() ArchRegs {
	var a ArchRegs
	a.R = c.regs
	cur := c.curBank()

	// r8〜r12 は FIQ とそれ以外の 2 組。
	if cur == bankFiq {
		copy(a.Fiq[:5], c.regs[8:13])
		copy(a.Usr[:5], c.bankR8Usr[:])
	} else {
		copy(a.Usr[:5], c.regs[8:13])
		copy(a.Fiq[:5], c.bankR8Fiq[:])
	}
	// r13/r14 はモードごと。
	r1314 := func(b int) [2]uint32 {
		if b == cur {
			return [2]uint32{c.regs[13], c.regs[14]}
		}
		return [2]uint32{c.bankR13[b], c.bankR14[b]}
	}
	u := r1314(bankUsr)
	a.Usr[5], a.Usr[6] = u[0], u[1]
	f := r1314(bankFiq)
	a.Fiq[5], a.Fiq[6] = f[0], f[1]
	a.Irq = r1314(bankIrq)
	a.Svc = r1314(bankSvc)
	a.Abt = r1314(bankAbt)
	a.Und = r1314(bankUnd)

	a.CPSR = uint32(c.cpsr)
	for i, b := range []int{bankFiq, bankIrq, bankSvc, bankAbt, bankUnd} {
		a.SPSR[i] = uint32(c.spsr[b])
	}
	return a
}
