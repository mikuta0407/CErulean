package arm

import (
	"bufio"
	"fmt"
	"os"
	"strings"
	"testing"
)

// Go 版と Rust 版の CPU の差分テスト用のベクタ（段階1。docs/rust-migration-plan.md §5.4）。
//
// CERULEAN_ARMVEC_OUT=<ファイル> のときだけ走る。splitmix64 の乱数列から
// ランダムな CPU 状態と命令語を作って 1 命令実行し、結果を 1 行ずつ書く。
// Rust 側（rust/core/src/arm/tests.rs の vector_diff）は同じ乱数列から同じ
// 入力を作り、この出力と突き合わせる。入力の作り方を変えるときは両方を直す。
// 件数は CERULEAN_ARMVEC_N（既定 200000）。

type splitmix struct{ s uint64 }

func (r *splitmix) next() uint64 {
	r.s += 0x9E3779B97F4A7C15
	z := r.s
	z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9
	z = (z ^ (z >> 27)) * 0x94D049BB133111EB
	return z ^ (z >> 31)
}

func (r *splitmix) u32() uint32            { return uint32(r.next() >> 32) }
func (r *splitmix) choose(n uint32) uint32 { return r.u32() % n }

var vecSpecial = []uint32{0, 1, 2, 31, 32, 33, 0x7FFFFFFF, 0x80000000, 0x80000001, 0xFFFFFFFF, 0xFFFFFFFE, 0x100}

// regVal はレジスタの値（メモリ内のアドレス・任意の値・境界値を混ぜる）。
func (r *splitmix) regVal() uint32 {
	switch k := r.choose(8); {
	case k < 4:
		return r.u32() & 0xFFFC
	case k == 4:
		return r.u32() & 0xFFFF
	case k == 5:
		return r.u32()
	case k == 6:
		return vecSpecial[r.choose(uint32(len(vecSpecial)))]
	default:
		return r.choose(64)
	}
}

// vecMemInit はメモリの初期値（Rust と同じ式）。
func vecMemInit(mem []byte) {
	for i := range mem {
		mem[i] = uint8((uint32(i) * 0x9E3779B1) >> 24)
	}
}

type vecCase struct {
	thumb   bool
	word    uint32
	regs    [16]uint32
	cpsr    uint32
	spsr    [numBanks]uint32
	r8usr   [5]uint32
	r8fiq   [5]uint32
	r13     [numBanks]uint32
	r14     [numBanks]uint32
	irq     bool
	fiq     bool
	vecBase uint32
	bad     int64 // -1 = なし
}

func genVecCase(r *splitmix) vecCase {
	var v vecCase
	v.thumb = r.choose(10) < 3
	modes := []uint32{ModeUsr, ModeFiq, ModeIrq, ModeSvc, ModeAbt, ModeUnd, ModeSys}
	mode := modes[r.choose(7)]
	v.cpsr = r.u32()&0xF0000000 | r.choose(4)<<6 | mode
	if v.thumb {
		v.cpsr |= uint32(FlagT)
	}
	for i := range v.regs {
		v.regs[i] = r.regVal()
	}
	if v.thumb {
		v.regs[15] = 0x1000 + r.choose(0x6000)*2
	} else {
		v.regs[15] = 0x1000 + r.choose(0x3000)*4
	}
	for i := range v.spsr {
		v.spsr[i] = r.u32()
	}
	for i := range v.r8usr {
		v.r8usr[i] = r.regVal()
	}
	for i := range v.r8fiq {
		v.r8fiq[i] = r.regVal()
	}
	for i := range v.r13 {
		v.r13[i] = r.regVal()
	}
	for i := range v.r14 {
		v.r14[i] = r.regVal()
	}
	v.irq = r.choose(8) == 0
	v.fiq = r.choose(16) == 0
	if r.choose(2) != 0 {
		v.vecBase = 0xFFFF0000
	}
	v.bad = -1
	if r.choose(4) == 0 {
		v.bad = int64(r.u32() & 0xFFFC)
	}
	if v.thumb {
		v.word = r.u32() & 0xFFFF
	} else {
		v.word = r.u32()
		if r.choose(10) < 6 {
			v.word = v.word&0x0FFFFFFF | 0xE0000000
		}
	}
	return v
}

func TestGenVectors(t *testing.T) {
	out := os.Getenv("CERULEAN_ARMVEC_OUT")
	if out == "" {
		t.Skip("CERULEAN_ARMVEC_OUT not set")
	}
	n := 200000
	if s := os.Getenv("CERULEAN_ARMVEC_N"); s != "" {
		fmt.Sscan(s, &n)
	}
	f, err := os.Create(out)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	w := bufio.NewWriter(f)
	defer w.Flush()

	rng := &splitmix{s: 0x43455255} // "CERU"
	base := make([]byte, 64*1024)
	vecMemInit(base)
	for i := 0; i < n; i++ {
		v := genVecCase(rng)
		tm := testMem{data: make([]byte, len(base))}
		copy(tm.data, base)
		cp := &fakeCP15{vecBase: v.vecBase}
		var mem interface {
			Read8(uint32) (uint8, error)
			Read16(uint32) (uint16, error)
			Read32(uint32) (uint32, error)
			Write8(uint32, uint8) error
			Write16(uint32, uint16) error
			Write32(uint32, uint32) error
		} = &tm
		am := &abortMem{testMem: tm, badAddr: uint32(v.bad)}
		if v.bad >= 0 {
			mem = am
		}
		c := New(mem, cp)
		c.Reset(v.regs[15])
		c.regs = v.regs
		c.cpsr = PSR(v.cpsr)
		c.spsr = [numBanks]PSR{}
		for j := range v.spsr {
			c.spsr[j] = PSR(v.spsr[j])
		}
		c.bankR8Usr, c.bankR8Fiq, c.bankR13, c.bankR14 = v.r8usr, v.r8fiq, v.r13, v.r14
		c.irq, c.fiq = v.irq, v.fiq
		data := tm.data
		if v.bad >= 0 {
			data = am.data
		}
		pc := v.regs[15]
		if v.thumb {
			data[pc], data[pc+1] = uint8(v.word), uint8(v.word>>8)
		} else {
			data[pc], data[pc+1], data[pc+2], data[pc+3] = uint8(v.word), uint8(v.word>>8), uint8(v.word>>16), uint8(v.word>>24)
		}
		before := append([]byte(nil), data...)
		stepErr := c.Step()

		var b strings.Builder
		fmt.Fprintf(&b, "%d ", i)
		switch e := stepErr.(type) {
		case nil:
			b.WriteString("ok")
		case *UndefinedError:
			fmt.Fprintf(&b, "undef %08X %08X %v", e.PC, e.Word, e.Arch)
		default:
			b.WriteString("stop")
		}
		fmt.Fprintf(&b, " |")
		for _, x := range c.regs {
			fmt.Fprintf(&b, " %08X", x)
		}
		fmt.Fprintf(&b, " | %08X |", uint32(c.cpsr))
		for _, x := range c.spsr {
			fmt.Fprintf(&b, " %08X", uint32(x))
		}
		b.WriteString(" |")
		for _, arr := range [][]uint32{c.bankR8Usr[:], c.bankR8Fiq[:], c.bankR13[:], c.bankR14[:]} {
			for _, x := range arr {
				fmt.Fprintf(&b, " %08X", x)
			}
		}
		fmt.Fprintf(&b, " | %08X %08X %v |", cp.fsr, cp.far, cp.priv)
		for a := 0; a < len(data); a += 4 {
			if string(data[a:a+4]) != string(before[a:a+4]) {
				fmt.Fprintf(&b, " %04X=%02X%02X%02X%02X", a, data[a+3], data[a+2], data[a+1], data[a])
			}
		}
		b.WriteByte('\n')
		w.WriteString(b.String())
	}
}
