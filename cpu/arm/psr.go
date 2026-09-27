package arm

import "fmt"

// PSR は CPSR/SPSR のビットフィールド。
//
//	31 30 29 28      7 6 5   4:0
//	N  Z  C  V  ...  I F T   mode
type PSR uint32

const (
	FlagN PSR = 1 << 31 // Negative
	FlagZ PSR = 1 << 30 // Zero
	FlagC PSR = 1 << 29 // Carry / not-borrow
	FlagV PSR = 1 << 28 // oVerflow
	FlagI PSR = 1 << 7  // IRQ 禁止
	FlagF PSR = 1 << 6  // FIQ 禁止
	FlagT PSR = 1 << 5  // Thumb state
)

// プロセッサモード（PSR bits 4:0）。
const (
	ModeUsr uint32 = 0x10
	ModeFiq uint32 = 0x11
	ModeIrq uint32 = 0x12
	ModeSvc uint32 = 0x13
	ModeAbt uint32 = 0x17
	ModeUnd uint32 = 0x1B
	ModeSys uint32 = 0x1F
)

func (p PSR) N() bool { return p&FlagN != 0 }
func (p PSR) Z() bool { return p&FlagZ != 0 }
func (p PSR) C() bool { return p&FlagC != 0 }
func (p PSR) V() bool { return p&FlagV != 0 }
func (p PSR) T() bool { return p&FlagT != 0 }

func (p PSR) Mode() uint32 { return uint32(p) & 0x1F }

// set は flag を b に合わせて立てる/落とす。
func (p PSR) set(flag PSR, b bool) PSR {
	if b {
		return p | flag
	}
	return p &^ flag
}

// SetNZ は結果 v から N/Z を更新する（論理演算用: C は呼び出し側でシフタキャリーを入れる）。
func (p PSR) SetNZ(v uint32) PSR {
	p = p.set(FlagN, v&0x80000000 != 0)
	return p.set(FlagZ, v == 0)
}

func (p PSR) String() string {
	flags := []struct {
		f PSR
		c byte
	}{{FlagN, 'N'}, {FlagZ, 'Z'}, {FlagC, 'C'}, {FlagV, 'V'}, {FlagI, 'I'}, {FlagF, 'F'}, {FlagT, 'T'}}
	buf := make([]byte, 0, 16)
	for _, x := range flags {
		if p&x.f != 0 {
			buf = append(buf, x.c)
		} else {
			buf = append(buf, '-')
		}
	}
	return fmt.Sprintf("%s mode=%02X", buf, p.Mode())
}

// condPassed は命令の条件フィールド（bits 31:28）が成立するか。
func condPassed(p PSR, cond uint32) bool {
	switch cond {
	case 0x0: // EQ
		return p.Z()
	case 0x1: // NE
		return !p.Z()
	case 0x2: // CS/HS
		return p.C()
	case 0x3: // CC/LO
		return !p.C()
	case 0x4: // MI
		return p.N()
	case 0x5: // PL
		return !p.N()
	case 0x6: // VS
		return p.V()
	case 0x7: // VC
		return !p.V()
	case 0x8: // HI
		return p.C() && !p.Z()
	case 0x9: // LS
		return !p.C() || p.Z()
	case 0xA: // GE
		return p.N() == p.V()
	case 0xB: // LT
		return p.N() != p.V()
	case 0xC: // GT
		return !p.Z() && p.N() == p.V()
	case 0xD: // LE
		return p.Z() || p.N() != p.V()
	case 0xE: // AL
		return true
	default: // 0xF: ARMv4 では UNPREDICTABLE、v5 では拡張命令空間。
		// decode 側で処理する（ここに来た場合は実行しない）。
		return false
	}
}
