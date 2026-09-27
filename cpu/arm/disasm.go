package arm

// 簡易ディスアセンブラ（デバッグトレース用）。
// 実装済みの命令だけを人間可読にする。網羅性・厳密性より「トレースを
// 目で追える」ことが目的なので、稀な形式は素直に .word 表示に落とす。

import (
	"fmt"
	"strings"
)

var condNames = [16]string{
	"eq", "ne", "cs", "cc", "mi", "pl", "vs", "vc",
	"hi", "ls", "ge", "lt", "gt", "le", "", "nv",
}

var dpNames = [16]string{
	"and", "eor", "sub", "rsb", "add", "adc", "sbc", "rsc",
	"tst", "teq", "cmp", "cmn", "orr", "mov", "bic", "mvn",
}

var shiftNames = [4]string{"lsl", "lsr", "asr", "ror"}

func reg(n uint32) string {
	switch n & 15 {
	case 13:
		return "sp"
	case 14:
		return "lr"
	case 15:
		return "pc"
	}
	return fmt.Sprintf("r%d", n&15)
}

// Disasm は word を PC=pc にある命令としてディスアセンブルする。
// pc は分岐先の絶対アドレス表示に使う。
func Disasm(word, pc uint32) string {
	cond := condNames[word>>28]
	if word>>28 == 0xF {
		return wordFallback(word)
	}
	switch (word >> 25) & 7 {
	case 0, 1:
		return disasmDP(word, cond)
	case 2, 3:
		if (word>>25)&7 == 3 && word&0x10 != 0 {
			return wordFallback(word) // v4 では未定義（decode.go と同じ扱い）
		}
		return disasmLdst(word, cond)
	case 4:
		return disasmLdmStm(word, cond)
	case 5:
		return disasmBranch(word, pc, cond)
	case 7:
		if word&(1<<24) != 0 {
			return fmt.Sprintf("swi%s 0x%06X", cond, word&0xFFFFFF)
		}
		if word&0x10 != 0 {
			return disasmMcrMrc(word, cond)
		}
	}
	return wordFallback(word)
}

func wordFallback(word uint32) string {
	return fmt.Sprintf(".word 0x%08X", word)
}

// shifterOperand は データ処理の第2オペランド表示。
func shifterOperand(word uint32) string {
	if word&(1<<25) != 0 {
		return fmt.Sprintf("#0x%X", ror(word&0xFF, ((word>>8)&0xF)*2))
	}
	rm := reg(word & 0xF)
	shiftType := (word >> 5) & 3
	if word&(1<<4) != 0 {
		return fmt.Sprintf("%s, %s %s", rm, shiftNames[shiftType], reg((word>>8)&0xF))
	}
	amount := (word >> 7) & 0x1F
	if amount == 0 {
		switch shiftType {
		case 0:
			return rm
		case 3:
			return rm + ", rrx"
		default: // lsr/asr #32
			return fmt.Sprintf("%s, %s #32", rm, shiftNames[shiftType])
		}
	}
	return fmt.Sprintf("%s, %s #%d", rm, shiftNames[shiftType], amount)
}

func disasmDP(word uint32, cond string) string {
	// データ処理空間に同居する命令を先に判別（decode.go と同じ順序）。
	if word&0x0FFFFFF0 == 0x012FFF10 {
		return fmt.Sprintf("bx%s %s", cond, reg(word&0xF))
	}
	if word&(1<<25) == 0 && word&0x90 == 0x90 {
		if (word>>5)&3 == 0 {
			return disasmMulSwp(word, cond)
		}
		return disasmLdstMisc(word, cond)
	}
	op := (word >> 21) & 0xF
	if op >= 8 && op <= 11 && word&(1<<20) == 0 {
		return disasmPSR(word, cond)
	}

	s := ""
	if word&(1<<20) != 0 {
		s = "s"
	}
	rn, rd := reg((word>>16)&0xF), reg((word>>12)&0xF)
	opnd := shifterOperand(word)
	switch op {
	case opMOV, opMVN:
		return fmt.Sprintf("%s%s%s %s, %s", dpNames[op], cond, s, rd, opnd)
	case opTST, opTEQ, opCMP, opCMN:
		return fmt.Sprintf("%s%s %s, %s", dpNames[op], cond, rn, opnd)
	default:
		return fmt.Sprintf("%s%s%s %s, %s, %s", dpNames[op], cond, s, rd, rn, opnd)
	}
}

func disasmPSR(word uint32, cond string) string {
	psr := "cpsr"
	if word&(1<<22) != 0 {
		psr = "spsr"
	}
	if word&(1<<21) == 0 { // MRS
		return fmt.Sprintf("mrs%s %s, %s", cond, reg((word>>12)&0xF), psr)
	}
	fields := ""
	for i, ch := range "cxsf" {
		if word&(1<<(16+i)) != 0 {
			fields += string(ch)
		}
	}
	if word&(1<<25) != 0 {
		return fmt.Sprintf("msr%s %s_%s, #0x%X", cond, psr, fields, ror(word&0xFF, ((word>>8)&0xF)*2))
	}
	return fmt.Sprintf("msr%s %s_%s, %s", cond, psr, fields, reg(word&0xF))
}

func disasmMulSwp(word uint32, cond string) string {
	s := ""
	if word&(1<<20) != 0 {
		s = "s"
	}
	rd, rn := (word>>16)&0xF, (word>>12)&0xF
	rs, rm := (word>>8)&0xF, word&0xF
	switch {
	case word&(1<<24) != 0: // SWP/SWPB
		b := ""
		if word&(1<<22) != 0 {
			b = "b"
		}
		return fmt.Sprintf("swp%s%s %s, %s, [%s]", cond, b, reg(rn), reg(rm), reg(rd))
	case word&(1<<23) != 0: // ロング乗算
		name := "umull"
		switch (word >> 21) & 3 {
		case 1:
			name = "umlal"
		case 2:
			name = "smull"
		case 3:
			name = "smlal"
		}
		return fmt.Sprintf("%s%s%s %s, %s, %s, %s", name, cond, s, reg(rn), reg(rd), reg(rm), reg(rs))
	case word&(1<<21) != 0:
		return fmt.Sprintf("mla%s%s %s, %s, %s, %s", cond, s, reg(rd), reg(rm), reg(rs), reg(rn))
	default:
		return fmt.Sprintf("mul%s%s %s, %s, %s", cond, s, reg(rd), reg(rm), reg(rs))
	}
}

// addrMode は LDR/STR 系のアドレス表示 "[rn, off]{!}" を組み立てる。
func addrMode(word uint32, offStr string) string {
	rn := reg((word >> 16) & 0xF)
	pre := word&(1<<24) != 0
	sign := ""
	if word&(1<<23) == 0 {
		sign = "-"
	}
	wb := ""
	if pre && word&(1<<21) != 0 {
		wb = "!"
	}
	if offStr == "" || offStr == "#0x0" || offStr == "#0" {
		if pre {
			return fmt.Sprintf("[%s]%s", rn, wb)
		}
		return fmt.Sprintf("[%s]", rn)
	}
	if pre {
		return fmt.Sprintf("[%s, %s%s]%s", rn, sign, offStr, wb)
	}
	return fmt.Sprintf("[%s], %s%s", rn, sign, offStr)
}

func disasmLdst(word uint32, cond string) string {
	name := "str"
	if word&(1<<20) != 0 {
		name = "ldr"
	}
	if word&(1<<22) != 0 {
		name += "b"
	}
	var offStr string
	if word&(1<<25) != 0 {
		// レジスタオフセット。ビット25 の意味がデータ処理と逆（1=レジスタ）なので、
		// クリアして shifterOperand のレジスタ形式表示を流用する。
		offStr = shifterOperand(word &^ (1 << 25))
	} else if off := word & 0xFFF; off != 0 {
		offStr = fmt.Sprintf("#0x%X", off)
	}
	return fmt.Sprintf("%s%s %s, %s", name, cond, reg((word>>12)&0xF), addrMode(word, offStr))
}

func disasmLdstMisc(word uint32, cond string) string {
	var name string
	switch load, sh := word&(1<<20) != 0, (word>>5)&3; {
	case sh == 1 && load:
		name = "ldrh"
	case sh == 1:
		name = "strh"
	case sh == 2 && load:
		name = "ldrsb"
	case sh == 3 && load:
		name = "ldrsh"
	default:
		return wordFallback(word) // LDRD/STRD（v5TE）等は未実装
	}
	var offStr string
	if word&(1<<22) != 0 {
		if off := ((word >> 4) & 0xF0) | (word & 0xF); off != 0 {
			offStr = fmt.Sprintf("#0x%X", off)
		}
	} else {
		offStr = reg(word & 0xF)
	}
	return fmt.Sprintf("%s%s %s, %s", name, cond, reg((word>>12)&0xF), addrMode(word, offStr))
}

func disasmLdmStm(word uint32, cond string) string {
	name := "stm"
	if word&(1<<20) != 0 {
		name = "ldm"
	}
	suffix := [4]string{"da", "ia", "db", "ib"}[(word>>23)&3]
	wb, s := "", ""
	if word&(1<<21) != 0 {
		wb = "!"
	}
	if word&(1<<22) != 0 {
		s = "^"
	}
	var regs []string
	for i := uint32(0); i < 16; i++ {
		if word&(1<<i) != 0 {
			regs = append(regs, reg(i))
		}
	}
	return fmt.Sprintf("%s%s%s %s%s, {%s}%s", name, suffix, cond, reg((word>>16)&0xF), wb, strings.Join(regs, ","), s)
}

func disasmBranch(word, pc uint32, cond string) string {
	name := "b"
	if word&(1<<24) != 0 {
		name = "bl"
	}
	offset := int32(word<<8) >> 6
	return fmt.Sprintf("%s%s 0x%08X", name, cond, uint32(int32(pc+8)+offset))
}

func disasmMcrMrc(word uint32, cond string) string {
	name := "mcr"
	if word&(1<<20) != 0 {
		name = "mrc"
	}
	return fmt.Sprintf("%s%s p%d, %d, %s, c%d, c%d, %d",
		name, cond, (word>>8)&0xF, (word>>21)&7, reg((word>>12)&0xF), (word>>16)&0xF, word&0xF, (word>>5)&7)
}
