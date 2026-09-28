package arm

import "testing"

// ArchRegs が現在モードに関係なく同じ値を返すこと（一致確認のダンプは
// 内部の退避の持ち方に依存してはならないため）。
func TestArchRegsIndependentOfCurrentMode(t *testing.T) {
	c, _ := newTestCore()
	// 各モードに入り、そのモードで見える r8〜r14 に固有の値を書く。
	modes := []uint32{ModeUsr, ModeFiq, ModeIrq, ModeSvc, ModeAbt, ModeUnd}
	for _, mode := range modes {
		c.SetCPSR(PSR(mode) | FlagI | FlagF)
		for r := 8; r <= 14; r++ {
			if r < 13 && mode != ModeUsr && mode != ModeFiq {
				continue // r8〜r12 は FIQ 以外で共有（usr で書いた値を残す）
			}
			c.SetReg(r, mode<<8|uint32(r))
		}
		if mode != ModeUsr {
			c.spsr[c.curBank()] = PSR(0x1000 | mode)
		}
	}
	for r := 0; r < 8; r++ {
		c.SetReg(r, 0xA0+uint32(r))
	}

	want := func(mode uint32, r int) uint32 {
		if r < 13 && mode != ModeFiq {
			mode = ModeUsr // r8〜r12 は FIQ 以外で共有
		}
		return mode<<8 | uint32(r)
	}
	var first ArchRegs
	for i, mode := range append(modes, ModeSys) {
		c.SetCPSR(PSR(mode) | FlagI | FlagF)
		a := c.ArchRegs()
		for r := 8; r <= 14; r++ {
			if a.Usr[r-8] != want(ModeUsr, r) || a.Fiq[r-8] != want(ModeFiq, r) {
				t.Fatalf("mode %02X: usr/fiq r%d = %X/%X", mode, r, a.Usr[r-8], a.Fiq[r-8])
			}
		}
		for j, b := range []struct {
			m uint32
			v [2]uint32
		}{{ModeIrq, a.Irq}, {ModeSvc, a.Svc}, {ModeAbt, a.Abt}, {ModeUnd, a.Und}} {
			if b.v != [2]uint32{want(b.m, 13), want(b.m, 14)} {
				t.Fatalf("mode %02X: bank %d r13/r14 = %X", mode, j, b.v)
			}
		}
		for j, m := range []uint32{ModeFiq, ModeIrq, ModeSvc, ModeAbt, ModeUnd} {
			if a.SPSR[j] != 0x1000|m {
				t.Fatalf("mode %02X: SPSR[%d] = %X", mode, j, a.SPSR[j])
			}
		}
		// 現在モードから見える r8〜r14 も ArchRegs.R に入っている。
		cur := mode
		if cur == ModeSys {
			cur = ModeUsr
		}
		for r := 8; r <= 14; r++ {
			if a.R[r] != want(cur, r) {
				t.Fatalf("mode %02X: R[%d] = %X", mode, r, a.R[r])
			}
		}
		// モード固有の部分（R・CPSR）以外は全モードで同じ。
		a.R, a.CPSR = ArchRegs{}.R, 0
		if i == 0 {
			first = a
		} else if a != first {
			t.Fatalf("mode %02X: banked view differs from usr view", mode)
		}
	}
}
