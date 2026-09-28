//! 簡易ディスアセンブラ（デバッグトレース用。Go の disasm.go・disasm_thumb.go）。
//! 実装済みの命令だけを人間可読にする。網羅性・厳密性より「トレースを目で
//! 追える」ことが目的なので、稀な形式は素直に .word 表示に落とす。
//! 一致確認では比べない（トレースの比較は PC と命令語で行う。計画書 §5.3）。

use super::exec_arm::ror;

const COND_NAMES: [&str; 16] = [
    "eq", "ne", "cs", "cc", "mi", "pl", "vs", "vc", "hi", "ls", "ge", "lt", "gt", "le", "", "nv",
];
const DP_NAMES: [&str; 16] = [
    "and", "eor", "sub", "rsb", "add", "adc", "sbc", "rsc", "tst", "teq", "cmp", "cmn", "orr",
    "mov", "bic", "mvn",
];
const SHIFT_NAMES: [&str; 4] = ["lsl", "lsr", "asr", "ror"];
const THUMB_ALU_NAMES: [&str; 16] = [
    "and", "eor", "lsl", "lsr", "asr", "adc", "sbc", "ror", "tst", "neg", "cmp", "cmn", "orr",
    "mul", "bic", "mvn",
];

fn reg(n: u32) -> String {
    match n & 15 {
        13 => "sp".into(),
        14 => "lr".into(),
        15 => "pc".into(),
        n => format!("r{n}"),
    }
}

fn word_fallback(word: u32) -> String {
    format!(".word 0x{word:08X}")
}

/// word を PC=pc にある ARM 命令としてディスアセンブルする。pc は分岐先の
/// 絶対アドレス表示に使う。
pub fn disasm(word: u32, pc: u32) -> String {
    let cond = COND_NAMES[(word >> 28) as usize];
    if word >> 28 == 0xF {
        return word_fallback(word);
    }
    match (word >> 25) & 7 {
        0 | 1 => disasm_dp(word, cond),
        2 | 3 => {
            if (word >> 25) & 7 == 3 && word & 0x10 != 0 {
                return word_fallback(word); // v4 では未定義（decode と同じ扱い）
            }
            disasm_ldst(word, cond)
        }
        4 => disasm_ldm_stm(word, cond),
        5 => {
            let name = if word & (1 << 24) != 0 { "bl" } else { "b" };
            let offset = ((word << 8) as i32) >> 6;
            format!(
                "{name}{cond} 0x{:08X}",
                pc.wrapping_add(8).wrapping_add(offset as u32)
            )
        }
        7 if word & (1 << 24) != 0 => format!("swi{cond} 0x{:06X}", word & 0xFFFFFF),
        7 if word & 0x10 != 0 => {
            let name = if word & (1 << 20) != 0 { "mrc" } else { "mcr" };
            format!(
                "{name}{cond} p{}, {}, {}, c{}, c{}, {}",
                (word >> 8) & 0xF,
                (word >> 21) & 7,
                reg(word >> 12),
                (word >> 16) & 0xF,
                word & 0xF,
                (word >> 5) & 7
            )
        }
        _ => word_fallback(word),
    }
}

/// データ処理の第2オペランド表示。
fn shifter_operand(word: u32) -> String {
    if word & (1 << 25) != 0 {
        return format!("#0x{:X}", ror(word & 0xFF, ((word >> 8) & 0xF) * 2));
    }
    let rm = reg(word);
    let st = ((word >> 5) & 3) as usize;
    if word & (1 << 4) != 0 {
        return format!("{rm}, {} {}", SHIFT_NAMES[st], reg(word >> 8));
    }
    match ((word >> 7) & 0x1F, st) {
        (0, 0) => rm,
        (0, 3) => format!("{rm}, rrx"),
        (0, _) => format!("{rm}, {} #32", SHIFT_NAMES[st]), // lsr/asr #32
        (a, _) => format!("{rm}, {} #{a}", SHIFT_NAMES[st]),
    }
}

fn disasm_dp(word: u32, cond: &str) -> String {
    // データ処理空間に同居する命令を先に判別（decode と同じ順序）。
    if word & 0x0FFFFFF0 == 0x012FFF10 {
        return format!("bx{cond} {}", reg(word));
    }
    if word & (1 << 25) == 0 && word & 0x90 == 0x90 {
        if (word >> 5) & 3 == 0 {
            return disasm_mul_swp(word, cond);
        }
        return disasm_ldst_misc(word, cond);
    }
    let op = ((word >> 21) & 0xF) as usize;
    if (8..=11).contains(&op) && word & (1 << 20) == 0 {
        return disasm_psr(word, cond);
    }
    let s = if word & (1 << 20) != 0 { "s" } else { "" };
    let (rn, rd, opnd) = (reg(word >> 16), reg(word >> 12), shifter_operand(word));
    match op {
        13 | 15 => format!("{}{cond}{s} {rd}, {opnd}", DP_NAMES[op]),
        8..=11 => format!("{}{cond} {rn}, {opnd}", DP_NAMES[op]),
        _ => format!("{}{cond}{s} {rd}, {rn}, {opnd}", DP_NAMES[op]),
    }
}

fn disasm_psr(word: u32, cond: &str) -> String {
    let psr = if word & (1 << 22) != 0 {
        "spsr"
    } else {
        "cpsr"
    };
    if word & (1 << 21) == 0 {
        return format!("mrs{cond} {}, {psr}", reg(word >> 12));
    }
    let fields: String = "cxsf"
        .chars()
        .enumerate()
        .filter(|(i, _)| word & (1 << (16 + i)) != 0)
        .map(|(_, c)| c)
        .collect();
    if word & (1 << 25) != 0 {
        return format!(
            "msr{cond} {psr}_{fields}, #0x{:X}",
            ror(word & 0xFF, ((word >> 8) & 0xF) * 2)
        );
    }
    format!("msr{cond} {psr}_{fields}, {}", reg(word))
}

fn disasm_mul_swp(word: u32, cond: &str) -> String {
    let s = if word & (1 << 20) != 0 { "s" } else { "" };
    let (rd, rn, rs, rm) = (reg(word >> 16), reg(word >> 12), reg(word >> 8), reg(word));
    if word & (1 << 24) != 0 {
        let b = if word & (1 << 22) != 0 { "b" } else { "" };
        return format!("swp{cond}{b} {rn}, {rm}, [{rd}]");
    }
    if word & (1 << 23) != 0 {
        let name = ["umull", "umlal", "smull", "smlal"][((word >> 21) & 3) as usize];
        return format!("{name}{cond}{s} {rn}, {rd}, {rm}, {rs}");
    }
    if word & (1 << 21) != 0 {
        return format!("mla{cond}{s} {rd}, {rm}, {rs}, {rn}");
    }
    format!("mul{cond}{s} {rd}, {rm}, {rs}")
}

/// LDR/STR 系のアドレス表示 "[rn, off]{!}" を組み立てる。
fn addr_mode(word: u32, off: &str) -> String {
    let rn = reg(word >> 16);
    let pre = word & (1 << 24) != 0;
    let sign = if word & (1 << 23) == 0 { "-" } else { "" };
    let wb = if pre && word & (1 << 21) != 0 {
        "!"
    } else {
        ""
    };
    if off.is_empty() || off == "#0x0" || off == "#0" {
        return if pre {
            format!("[{rn}]{wb}")
        } else {
            format!("[{rn}]")
        };
    }
    if pre {
        format!("[{rn}, {sign}{off}]{wb}")
    } else {
        format!("[{rn}], {sign}{off}")
    }
}

fn disasm_ldst(word: u32, cond: &str) -> String {
    let mut name = if word & (1 << 20) != 0 { "ldr" } else { "str" }.to_string();
    if word & (1 << 22) != 0 {
        name.push('b');
    }
    let off = if word & (1 << 25) != 0 {
        // レジスタオフセット。ビット25 の意味がデータ処理と逆（1=レジスタ）なので、
        // クリアして shifter_operand のレジスタ形式表示を流用する。
        shifter_operand(word & !(1 << 25))
    } else if word & 0xFFF != 0 {
        format!("#0x{:X}", word & 0xFFF)
    } else {
        String::new()
    };
    format!(
        "{name}{cond} {}, {}",
        reg(word >> 12),
        addr_mode(word, &off)
    )
}

fn disasm_ldst_misc(word: u32, cond: &str) -> String {
    let name = match (word & (1 << 20) != 0, (word >> 5) & 3) {
        (true, 1) => "ldrh",
        (false, 1) => "strh",
        (true, 2) => "ldrsb",
        (true, 3) => "ldrsh",
        _ => return word_fallback(word), // LDRD/STRD（v5TE）等は未実装
    };
    let off = if word & (1 << 22) != 0 {
        let o = ((word >> 4) & 0xF0) | (word & 0xF);
        if o != 0 {
            format!("#0x{o:X}")
        } else {
            String::new()
        }
    } else {
        reg(word)
    };
    format!(
        "{name}{cond} {}, {}",
        reg(word >> 12),
        addr_mode(word, &off)
    )
}

fn disasm_ldm_stm(word: u32, cond: &str) -> String {
    let name = if word & (1 << 20) != 0 { "ldm" } else { "stm" };
    let suffix = ["da", "ia", "db", "ib"][((word >> 23) & 3) as usize];
    let wb = if word & (1 << 21) != 0 { "!" } else { "" };
    let s = if word & (1 << 22) != 0 { "^" } else { "" };
    let regs: Vec<String> = (0..16).filter(|i| word & (1 << i) != 0).map(reg).collect();
    format!(
        "{name}{suffix}{cond} {}{wb}, {{{}}}{s}",
        reg(word >> 16),
        regs.join(",")
    )
}

// ---- Thumb ----

fn lreg(n: u32) -> String {
    format!("r{}", n & 7)
}

fn hword_fallback(hw: u32) -> String {
    format!(".hword 0x{hw:04X}")
}

/// PUSH/POP/LDMIA/STMIA のレジスタリスト表示。extra は bit8（PUSH の lr /
/// POP の pc）用の追加レジスタ名（空なら無し）。
fn thumb_reg_list(list: u32, extra: &str) -> String {
    let mut regs: Vec<String> = (0..8).filter(|i| list & (1 << i) != 0).map(lreg).collect();
    if !extra.is_empty() {
        regs.push(extra.into());
    }
    format!("{{{}}}", regs.join(","))
}

/// hw を PC=pc にある Thumb 命令としてディスアセンブルする。next は直後の
/// ハーフワード。BL は 2 ハーフワードで 1 命令なので、hw が BL プレフィックスで
/// next がサフィックスなら分岐先をまとめて表示する。
pub fn disasm_thumb(hw: u32, next: u32, pc: u32) -> String {
    let hw = hw & 0xFFFF;
    match hw >> 13 {
        0 => {
            let (rd, rs) = (lreg(hw), lreg(hw >> 3));
            let op = (hw >> 11) & 3;
            if op != 3 {
                return format!(
                    "{}s {rd}, {rs}, #{}",
                    SHIFT_NAMES[op as usize],
                    (hw >> 6) & 0x1F
                );
            }
            let name = if hw & (1 << 9) != 0 { "subs" } else { "adds" };
            if hw & (1 << 10) != 0 {
                return format!("{name} {rd}, {rs}, #{}", (hw >> 6) & 7);
            }
            format!("{name} {rd}, {rs}, {}", lreg(hw >> 6))
        }
        1 => {
            let name = ["movs", "cmp", "adds", "subs"][((hw >> 11) & 3) as usize];
            format!("{name} {}, #0x{:X}", lreg(hw >> 8), hw & 0xFF)
        }
        2 => disasm_thumb010(hw, pc),
        3 => {
            let name = ["str", "ldr", "strb", "ldrb"][((hw >> 11) & 3) as usize];
            let mut off = (hw >> 6) & 0x1F;
            if hw & (1 << 12) == 0 {
                off *= 4; // ワードアクセスは imm5*4
            }
            format!("{name} {}, [{}, #0x{off:X}]", lreg(hw), lreg(hw >> 3))
        }
        4 => {
            if hw & (1 << 12) == 0 {
                let name = if hw & (1 << 11) != 0 { "ldrh" } else { "strh" };
                return format!(
                    "{name} {}, [{}, #0x{:X}]",
                    lreg(hw),
                    lreg(hw >> 3),
                    ((hw >> 6) & 0x1F) * 2
                );
            }
            let name = if hw & (1 << 11) != 0 { "ldr" } else { "str" };
            format!("{name} {}, [sp, #0x{:X}]", lreg(hw >> 8), (hw & 0xFF) * 4)
        }
        5 => disasm_thumb101(hw),
        6 => disasm_thumb110(hw, pc),
        _ => disasm_thumb_branch(hw, next, pc),
    }
}

fn disasm_thumb010(hw: u32, pc: u32) -> String {
    if hw >> 10 == 0x10 {
        // Format 4 ALU（tst/cmp/cmn 以外はフラグ更新付き）
        let op = (hw >> 6) & 0xF;
        let s = if op != 0x8 && op != 0xA && op != 0xB {
            "s"
        } else {
            ""
        };
        return format!(
            "{}{s} {}, {}",
            THUMB_ALU_NAMES[op as usize],
            lreg(hw),
            lreg(hw >> 3)
        );
    }
    if hw >> 10 == 0x11 {
        // Format 5 Hi レジスタ・BX
        let rd = (hw & 7) | ((hw >> 4) & 8);
        let rs = (hw >> 3) & 0xF;
        return match (hw >> 8) & 3 {
            0 => format!("add {}, {}", reg(rd), reg(rs)),
            1 => format!("cmp {}, {}", reg(rd), reg(rs)),
            2 => format!("mov {}, {}", reg(rd), reg(rs)),
            _ => format!("bx {}", reg(rs)),
        };
    }
    if hw >> 11 == 0x9 {
        // Format 6 PC 相対 LDR（PC は命令+4 をワードアライン）
        let addr = (pc.wrapping_add(4) & !3).wrapping_add((hw & 0xFF) * 4);
        return format!(
            "ldr {}, [pc, #0x{:X}] ; =0x{addr:08X}",
            lreg(hw >> 8),
            (hw & 0xFF) * 4
        );
    }
    // Format 7/8 レジスタオフセット
    let name = [
        "str", "strh", "strb", "ldrsb", "ldr", "ldrh", "ldrb", "ldrsh",
    ][((hw >> 9) & 7) as usize];
    format!(
        "{name} {}, [{}, {}]",
        lreg(hw),
        lreg(hw >> 3),
        lreg(hw >> 6)
    )
}

fn disasm_thumb101(hw: u32) -> String {
    if hw & (1 << 12) == 0 {
        // Format 12: ADD Rd, PC/SP, #imm8*4
        let base = if hw & (1 << 11) != 0 { "sp" } else { "pc" };
        return format!("add {}, {base}, #0x{:X}", lreg(hw >> 8), (hw & 0xFF) * 4);
    }
    if (hw >> 8) & 0xF == 0 {
        // Format 13: ADD SP, #±imm7*4
        let name = if hw & (1 << 7) != 0 { "sub" } else { "add" };
        return format!("{name} sp, #0x{:X}", (hw & 0x7F) * 4);
    }
    if (hw >> 9) & 3 == 2 {
        // Format 14: PUSH/POP
        if hw & (1 << 11) != 0 {
            return format!(
                "pop {}",
                thumb_reg_list(hw & 0xFF, if hw & (1 << 8) != 0 { "pc" } else { "" })
            );
        }
        return format!(
            "push {}",
            thumb_reg_list(hw & 0xFF, if hw & (1 << 8) != 0 { "lr" } else { "" })
        );
    }
    hword_fallback(hw) // v4T では未定義（BKPT 等は v5）
}

fn disasm_thumb110(hw: u32, pc: u32) -> String {
    if hw & (1 << 12) == 0 {
        // Format 15: LDMIA/STMIA Rn!, {rlist}
        let name = if hw & (1 << 11) != 0 {
            "ldmia"
        } else {
            "stmia"
        };
        return format!(
            "{name} {}!, {}",
            lreg(hw >> 8),
            thumb_reg_list(hw & 0xFF, "")
        );
    }
    match (hw >> 8) & 0xF {
        0xF => format!("swi 0x{:02X}", hw & 0xFF),
        0xE => hword_fallback(hw), // 未定義
        cond => {
            // Format 16: 条件分岐（PC+4 基準、±imm8*2）
            let off = ((hw << 24) as i32) >> 23;
            format!(
                "b{} 0x{:08X}",
                COND_NAMES[cond as usize],
                pc.wrapping_add(4).wrapping_add(off as u32)
            )
        }
    }
}

fn disasm_thumb_branch(hw: u32, next: u32, pc: u32) -> String {
    match (hw >> 11) & 3 {
        0 => {
            // B ±imm11*2
            let off = ((hw << 21) as i32) >> 20;
            format!("b 0x{:08X}", pc.wrapping_add(4).wrapping_add(off as u32))
        }
        2 => {
            // BL プレフィックス
            let hi = pc
                .wrapping_add(4)
                .wrapping_add((((hw << 21) as i32) >> 9) as u32);
            if (next >> 11) & 0x1F == 0x1F {
                return format!("bl 0x{:08X}", hi.wrapping_add((next & 0x7FF) << 1));
            }
            format!("bl.prefix lr=0x{hi:08X}")
        }
        // BL サフィックス（分岐先は LR 依存なので相対値のみ）
        3 => format!("bl.suffix lr+0x{:X}", (hw & 0x7FF) << 1),
        _ => hword_fallback(hw), // BLX サフィックス（v5。未実装）
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm() {
        // 期待文字列は Go 版と同じ（GNU as 風だが厳密互換は目的でない）。
        for (word, pc, want) in [
            (0xE3A0D901, 0, "mov sp, #0x4000"),
            (0xE1A01002, 0, "mov r1, r2"),
            (0xE0813282, 0, "add r3, r1, r2, lsl #5"),
            (0xE2522001, 0, "subs r2, r2, #0x1"),
            (0xE3530000, 0, "cmp r3, #0x0"),
            (0x0A000003, 0x1000, "beq 0x00001014"),
            (0xEB000000, 0x1000, "bl 0x00001008"),
            (0xE12FFF11, 0, "bx r1"),
            (0xE5912004, 0, "ldr r2, [r1, #0x4]"),
            (0xE5A12004, 0, "str r2, [r1, #0x4]!"),
            (0xE4912004, 0, "ldr r2, [r1], #0x4"),
            (0xE5D12000, 0, "ldrb r2, [r1]"),
            (0xE7912002, 0, "ldr r2, [r1, r2]"),
            (0xE1D120B4, 0, "ldrh r2, [r1, #0x4]"),
            (0xE8BD8010, 0, "ldmia sp!, {r4,pc}"),
            (0xE92D4010, 0, "stmdb sp!, {r4,lr}"),
            (0xE10F1000, 0, "mrs r1, cpsr"),
            (0xE129F001, 0, "msr cpsr_cf, r1"),
            (0xE0030291, 0, "mul r3, r1, r2"),
            (0xE0854392, 0, "umull r4, r5, r2, r3"),
            (0xE1013092, 0, "swp r3, r2, [r1]"),
            (0xEE110F10, 0, "mrc p15, 0, r0, c1, c0, 0"),
            (0xEE010F10, 0, "mcr p15, 0, r0, c1, c0, 0"),
            (0xEF000010, 0, "swi 0x000010"),
            (0xE7F000F0, 0, ".word 0xE7F000F0"),
        ] {
            assert_eq!(disasm(word, pc), want, "{word:08X}");
        }
    }

    #[test]
    fn thumb() {
        for (hw, next, pc, want) in [
            (0x2005, 0, 0, "movs r0, #0x5"),
            (0x1888, 0, 0, "adds r0, r1, r2"),
            (0x1E48, 0, 0, "subs r0, r1, #1"),
            (0x0048, 0, 0, "lsls r0, r1, #1"),
            (0x4288, 0, 0, "cmp r0, r1"),
            (0x4008, 0, 0, "ands r0, r1"),
            (0x4770, 0, 0, "bx lr"),
            (0x46C0, 0, 0, "mov r8, r8"),
            (0x4801, 0, 0x1002, "ldr r0, [pc, #0x4] ; =0x00001008"),
            (0x5888, 0, 0, "ldr r0, [r1, r2]"),
            (0x6848, 0, 0, "ldr r0, [r1, #0x4]"),
            (0x7848, 0, 0, "ldrb r0, [r1, #0x1]"),
            (0x8848, 0, 0, "ldrh r0, [r1, #0x2]"),
            (0x9801, 0, 0, "ldr r0, [sp, #0x4]"),
            (0xA801, 0, 0, "add r0, sp, #0x4"),
            (0xB082, 0, 0, "sub sp, #0x8"),
            (0xB510, 0, 0, "push {r4,lr}"),
            (0xBD10, 0, 0, "pop {r4,pc}"),
            (0xC103, 0, 0, "stmia r1!, {r0,r1}"),
            (0xD0FE, 0, 0x1000, "beq 0x00001000"),
            (0xDF01, 0, 0, "swi 0x01"),
            (0xDE00, 0, 0, ".hword 0xDE00"),
            (0xE7FE, 0, 0x1000, "b 0x00001000"),
            (0xF000, 0xF802, 0x1000, "bl 0x00001008"),
            (0xF802, 0, 0x1002, "bl.suffix lr+0x4"),
        ] {
            assert_eq!(disasm_thumb(hw, next, pc), want, "{hw:04X}");
        }
    }
}
