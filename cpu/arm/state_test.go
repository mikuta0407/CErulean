package arm

import (
	"testing"

	"github.com/mikuta0407/cerulean/snapshot/snapshottest"
)

func TestCoreStateFields(t *testing.T) {
	snapshottest.CheckFields(t, Core{},
		[]string{"regs", "cpsr", "spsr", "bankR8Usr", "bankR8Fiq", "bankR13", "bankR14", "irq", "fiq"},
		// spinHint は実行ループが毎命令消費する一時値、hist* はデバッグ用の記録。
		[]string{"mem", "cp15", "fetch32", "prober", "spinHint", "hist", "histPos", "histN"})
}

func TestCoreStateRoundTrip(t *testing.T) {
	src := &Core{}
	for i := range src.regs {
		src.regs[i] = uint32(i) * 0x01010101
	}
	src.cpsr = PSR(ModeIrq) | FlagT | FlagI
	for i := range src.spsr {
		src.spsr[i] = PSR(0x10 + i)
	}
	for i := range src.bankR8Usr {
		src.bankR8Usr[i] = 0x100 + uint32(i)
		src.bankR8Fiq[i] = 0x200 + uint32(i)
	}
	for i := range src.bankR13 {
		src.bankR13[i] = 0x300 + uint32(i)
		src.bankR14[i] = 0x400 + uint32(i)
	}
	src.irq = true
	dst := &Core{}
	snapshottest.RoundTrip(t, src, dst)
	if dst.regs != src.regs || dst.cpsr != src.cpsr || dst.spsr != src.spsr ||
		dst.bankR8Fiq != src.bankR8Fiq || dst.bankR14 != src.bankR14 || !dst.irq || dst.fiq {
		t.Errorf("state mismatch after round trip")
	}
}
