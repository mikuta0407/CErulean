package arm

// Thumb 命令の簡易ディスアセンブラ（デバッグトレース用）。
// 分類は exec_thumb.go（ARM ARM DDI 0100 Figure A6-1）と同じ順序にしてあり、
// 実行側で未定義扱いの命令は .hword 表示に落とす。

import (
	"fmt"
	"strings"
)

var thumbALUNames = [16]string{
	"and", "eor", "lsl", "lsr", "asr", "adc", "sbc", "ror",
	"tst", "neg", "cmp", "cmn", "orr", "mul", "bic", "mvn",
}

func lreg(n uint32) string { return fmt.Sprintf("r%d", n&7) }

func hwordFallback(hw uint32) string { return fmt.Sprintf(".hword 0x%04X", hw) }

// thumbRegList は PUSH/POP/LDMIA/STMIA のレジスタリスト表示。
// extra は bit8（PUSH の lr / POP の pc）用の追加レジスタ名（空なら無し）。
func thumbRegList(list uint32, extra string) string {
	var regs []string
	for i := uint32(0); i < 8; i++ {
		if list&(1<<i) != 0 {
			regs = append(regs, lreg(i))
		}
	}
	if extra != "" {
		regs = append(regs, extra)
	}
	return "{" + strings.Join(regs, ",") + "}"
}

// DisasmThumb は hw を PC=pc にある Thumb 命令としてディスアセンブルする。
// next は直後のハーフワード。BL は 2 ハーフワードで 1 命令なので、hw が
// BL プレフィックスで next がサフィックスなら分岐先をまとめて表示する
// （実行は 2 ステップに分かれるため、サフィックス単体も表示できるようにしてある）。
func DisasmThumb(hw, next, pc uint32) string {
	hw &= 0xFFFF
	switch hw >> 13 {
	case 0:
		rd, rs := lreg(hw), lreg(hw>>3)
		op := (hw >> 11) & 3
		if op != 3 {
			return fmt.Sprintf("%ss %s, %s, #%d", shiftNames[op], rd, rs, (hw>>6)&0x1F)
		}
		name := "adds"
		if hw&(1<<9) != 0 {
			name = "subs"
		}
		if hw&(1<<10) != 0 {
			return fmt.Sprintf("%s %s, %s, #%d", name, rd, rs, (hw>>6)&7)
		}
		return fmt.Sprintf("%s %s, %s, %s", name, rd, rs, lreg(hw>>6))
	case 1:
		name := [4]string{"movs", "cmp", "adds", "subs"}[(hw>>11)&3]
		return fmt.Sprintf("%s %s, #0x%X", name, lreg(hw>>8), hw&0xFF)
	case 2:
		return disasmThumb010(hw, pc)
	case 3:
		name := [4]string{"str", "ldr", "strb", "ldrb"}[(hw>>11)&3]
		off := (hw >> 6) & 0x1F
		if hw&(1<<12) == 0 {
			off *= 4 // ワードアクセスは imm5*4
		}
		return fmt.Sprintf("%s %s, [%s, #0x%X]", name, lreg(hw), lreg(hw>>3), off)
	case 4:
		if hw&(1<<12) == 0 {
			name := "strh"
			if hw&(1<<11) != 0 {
				name = "ldrh"
			}
			return fmt.Sprintf("%s %s, [%s, #0x%X]", name, lreg(hw), lreg(hw>>3), ((hw>>6)&0x1F)*2)
		}
		name := "str"
		if hw&(1<<11) != 0 {
			name = "ldr"
		}
		return fmt.Sprintf("%s %s, [sp, #0x%X]", name, lreg(hw>>8), (hw&0xFF)*4)
	case 5:
		return disasmThumb101(hw)
	case 6:
		return disasmThumb110(hw, pc)
	default:
		return disasmThumbBranch(hw, next, pc)
	}
}

func disasmThumb010(hw, pc uint32) string {
	switch {
	case hw>>10 == 0x10: // Format 4 ALU
		op := (hw >> 6) & 0xF
		name := thumbALUNames[op]
		if op != 0x8 && op != 0xA && op != 0xB { // tst/cmp/cmn 以外はフラグ更新付き
			name += "s"
		}
		return fmt.Sprintf("%s %s, %s", name, lreg(hw), lreg(hw>>3))
	case hw>>10 == 0x11: // Format 5 Hi レジスタ・BX
		rd := (hw & 7) | ((hw >> 4) & 8)
		rs := (hw >> 3) & 0xF
		switch (hw >> 8) & 3 {
		case 0:
			return fmt.Sprintf("add %s, %s", reg(rd), reg(rs))
		case 1:
			return fmt.Sprintf("cmp %s, %s", reg(rd), reg(rs))
		case 2:
			return fmt.Sprintf("mov %s, %s", reg(rd), reg(rs))
		default:
			return fmt.Sprintf("bx %s", reg(rs))
		}
	case hw>>11 == 0x9: // Format 6 PC 相対 LDR（PC は命令+4 をワードアライン）
		addr := (pc+4)&^3 + (hw&0xFF)*4
		return fmt.Sprintf("ldr %s, [pc, #0x%X] ; =0x%08X", lreg(hw>>8), (hw&0xFF)*4, addr)
	default: // Format 7/8 レジスタオフセット
		name := [8]string{"str", "strh", "strb", "ldrsb", "ldr", "ldrh", "ldrb", "ldrsh"}[(hw>>9)&7]
		return fmt.Sprintf("%s %s, [%s, %s]", name, lreg(hw), lreg(hw>>3), lreg(hw>>6))
	}
}

func disasmThumb101(hw uint32) string {
	if hw&(1<<12) == 0 { // Format 12: ADD Rd, PC/SP, #imm8*4
		base := "pc"
		if hw&(1<<11) != 0 {
			base = "sp"
		}
		return fmt.Sprintf("add %s, %s, #0x%X", lreg(hw>>8), base, (hw&0xFF)*4)
	}
	switch {
	case (hw>>8)&0xF == 0: // Format 13: ADD SP, #±imm7*4
		if hw&(1<<7) != 0 {
			return fmt.Sprintf("sub sp, #0x%X", (hw&0x7F)*4)
		}
		return fmt.Sprintf("add sp, #0x%X", (hw&0x7F)*4)
	case (hw>>9)&3 == 2: // Format 14: PUSH/POP
		if hw&(1<<11) != 0 {
			extra := ""
			if hw&(1<<8) != 0 {
				extra = "pc"
			}
			return "pop " + thumbRegList(hw&0xFF, extra)
		}
		extra := ""
		if hw&(1<<8) != 0 {
			extra = "lr"
		}
		return "push " + thumbRegList(hw&0xFF, extra)
	}
	return hwordFallback(hw) // v4T では未定義（BKPT 等は v5）
}

func disasmThumb110(hw, pc uint32) string {
	if hw&(1<<12) == 0 { // Format 15: LDMIA/STMIA Rn!, {rlist}
		name := "stmia"
		if hw&(1<<11) != 0 {
			name = "ldmia"
		}
		return fmt.Sprintf("%s %s!, %s", name, lreg(hw>>8), thumbRegList(hw&0xFF, ""))
	}
	switch cond := (hw >> 8) & 0xF; cond {
	case 0xF:
		return fmt.Sprintf("swi 0x%02X", hw&0xFF)
	case 0xE:
		return hwordFallback(hw) // 未定義
	default: // Format 16: 条件分岐（PC+4 基準、±imm8*2）
		off := int32(hw<<24) >> 23
		return fmt.Sprintf("b%s 0x%08X", condNames[cond], uint32(int32(pc+4)+off))
	}
}

func disasmThumbBranch(hw, next, pc uint32) string {
	switch (hw >> 11) & 3 {
	case 0: // B ±imm11*2
		off := int32(hw<<21) >> 20
		return fmt.Sprintf("b 0x%08X", uint32(int32(pc+4)+off))
	case 2: // BL プレフィックス
		hi := int32(hw<<21) >> 9 // signext(imm11)<<12
		if (next>>11)&0x1F == 0x1F {
			target := uint32(int32(pc+4)+hi) + (next&0x7FF)<<1
			return fmt.Sprintf("bl 0x%08X", target)
		}
		return fmt.Sprintf("bl.prefix lr=0x%08X", uint32(int32(pc+4)+hi))
	case 3: // BL サフィックス（分岐先は LR 依存なので相対値のみ）
		return fmt.Sprintf("bl.suffix lr+0x%X", (hw&0x7FF)<<1)
	default: // BLX サフィックス（v5。未実装）
		return hwordFallback(hw)
	}
}
