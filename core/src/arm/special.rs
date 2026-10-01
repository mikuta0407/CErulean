//! 頻出する命令形の特化（性能対策）。
//!
//! デコード時に、よく実行される形の命令には専用の実行関数を選び、即値（回転済みの
//! 即値・符号つきのオフセット・分岐先の差分）を取り出しておく。汎用の実行関数
//! （exec_data_proc など）は実行のたびに命令語を分解し、16 種類の演算の match と
//! フラグ処理の分岐を通るため。デコード結果はデコードキャッシュに残るので、
//! 取り出しは物理ページ上の命令ごとに 1 回で済む。
//!
//! 特化した形は IR（ir.rs）の専用の Op になり、レジスタ番号と即値を Instr に持つ。
//! ここの関数はその Op の実行（ir::exec から展開される）。演算の種類は const
//! ジェネリクスで渡す。
//!
//! 対象は WM5 の実行で多い形（2026-09 に実行命令の分布を計測: LDR 即値 18%、
//! CMP 即値 8%、MOV 即値・レジスタ 各 7〜8%、条件分岐 9% など）。
//! 意味は汎用の実装と完全に同じでなければならない（tests.rs の
//! specialized_matches_generic がランダムな命令語と状態で突き合わせる）。
//! 境界的な形（Rd/Rn=PC、ライトバック、シフトつきオペランド、ADC/SBC/RSB/RSC 等）は
//! 特化せず汎用に回す。

use super::exec_arm::*;
use super::ir::{Instr, Op};
use super::*;

/// 加減算の結果からフラグ 4 つを立てる。
#[inline(always)]
fn nzcv(c: &mut Cpu, (res, cf, vf): (u32, bool, bool)) -> u32 {
    c.cpsr = set_flag(set_flag(set_nz(c.cpsr, res), FLAG_C, cf), FLAG_V, vf);
    res
}

#[inline(always)]
fn rd(w: u32) -> u32 {
    (w >> 12) & 0xF
}

#[inline(always)]
fn rn(w: u32) -> u32 {
    (w >> 16) & 0xF
}

#[inline(always)]
fn rm(w: u32) -> u32 {
    w & 0xF
}

/// word に特化した Op があれば返す（なければ None）。条件フィールドは実行
/// ループが判定するので、ここでは見ない（0xF は対象外）。
pub(crate) fn specialize(word: u32) -> Option<Instr> {
    if word >> 28 == 0xF {
        return None;
    }
    match (word >> 25) & 7 {
        1 => spec_data_proc_imm(word),
        0 if word & 0x0FFFFFF0 == 0x012FFF10 => spec_bx(word),
        0 if word & 0x90 == 0x90 => spec_ldst_misc_imm(word),
        0 if word & 0x10 == 0 => spec_data_proc_reg(word), // レジスタ・即値シフト
        2 => spec_ldst_imm(word),
        5 => Some(spec_branch(word)),
        _ => None,
    }
}

// ---- データ処理（即値オペランド）----

/// S=0 の即値演算: Rd = Rn op imm（MOV/MVN は imm そのもの。MVN は反転済み）。
#[inline(always)]
pub(super) fn dp_imm<const OP: u32>(c: &mut Cpu, rd: usize, rn: usize, imm: u32) -> ExecResult {
    let n = c.regs[rn];
    c.regs[rd] = match OP {
        OP_MOV => imm,
        OP_ADD => n.wrapping_add(imm),
        OP_SUB => n.wrapping_sub(imm),
        OP_AND => n & imm,
        OP_ORR => n | imm,
        OP_EOR => n ^ imm,
        _ => n & !imm, // BIC
    };
    Ok(())
}

/// S=1 の即値の加減算・比較（CMP/CMN は Rd に書かない）。
#[inline(always)]
pub(super) fn dp_imm_arith<const OP: u32>(
    c: &mut Cpu,
    rd: usize,
    rn: usize,
    imm: u32,
) -> ExecResult {
    let n = c.regs[rn];
    let r = match OP {
        OP_CMP | OP_SUB => add_with_carry(n, !imm, 1),
        _ => add_with_carry(n, imm, 0), // CMN/ADD
    };
    let res = nzcv(c, r);
    if OP == OP_SUB || OP == OP_ADD {
        c.regs[rd] = res;
    }
    Ok(())
}

/// S=1 の即値の論理演算: N/Z は結果、C はシフタキャリー（回転ありなら即値の
/// bit31、なしなら現在の C = 変えない）、V は不変。SETC は「回転あり」。
#[inline(always)]
pub(super) fn dp_imm_logic<const OP: u32, const SETC: bool>(
    c: &mut Cpu,
    rd: usize,
    rn: usize,
    imm: u32,
) -> ExecResult {
    let n = c.regs[rn];
    let res = match OP {
        OP_TST | OP_AND => n & imm,
        OP_TEQ => n ^ imm,
        OP_ORR => n | imm,
        _ => imm, // MOV
    };
    let mut p = set_nz(c.cpsr, res);
    if SETC {
        p = set_flag(p, FLAG_C, imm & 0x80000000 != 0);
    }
    c.cpsr = p;
    if OP != OP_TST && OP != OP_TEQ {
        c.regs[rd] = res;
    }
    Ok(())
}

/// 即値オペランドのデータ処理（Rd・Rn が PC でない形）。
fn spec_data_proc_imm(w: u32) -> Option<Instr> {
    let op = (w >> 21) & 0xF;
    let s = w & (1 << 20) != 0;
    let is_test = (OP_TST..=OP_CMN).contains(&op);
    if (is_test && !s) || rn(w) == 15 || (rd(w) == 15 && !is_test) {
        return None; // MRS/MSR の空間・PC を使う形
    }
    let rot = ((w >> 8) & 0xF) * 2;
    let imm = ror(w & 0xFF, rot);
    if !s {
        let o = match op {
            OP_MOV => Op::MovImm,
            OP_MVN => return Some(Instr::special(Op::MovImm, w, !imm)),
            OP_ADD => Op::AddImm,
            OP_SUB => Op::SubImm,
            OP_AND => Op::AndImm,
            OP_ORR => Op::OrrImm,
            OP_EOR => Op::EorImm,
            OP_BIC => Op::BicImm,
            _ => return None,
        };
        return Some(Instr::special(o, w, imm));
    }
    let setc = rot != 0;
    let o = match (op, setc) {
        (OP_CMP, _) => Op::CmpImm,
        (OP_CMN, _) => Op::CmnImm,
        (OP_SUB, _) => Op::SubsImm,
        (OP_ADD, _) => Op::AddsImm,
        (OP_TST, false) => Op::TstImm,
        (OP_TST, true) => Op::TstImmC,
        (OP_TEQ, false) => Op::TeqImm,
        (OP_TEQ, true) => Op::TeqImmC,
        (OP_AND, false) => Op::AndsImm,
        (OP_AND, true) => Op::AndsImmC,
        (OP_ORR, false) => Op::OrrsImm,
        (OP_ORR, true) => Op::OrrsImmC,
        (OP_MOV, false) => Op::MovsImm,
        (OP_MOV, true) => Op::MovsImmC,
        _ => return None,
    };
    Some(Instr::special(o, w, imm))
}

// ---- データ処理（レジスタオペランド、シフトなし。imm = Rm の番号）----

/// S=0: Rd = Rn op Rm。
#[inline(always)]
pub(super) fn dp_reg<const OP: u32>(c: &mut Cpu, rd: usize, rn: usize, rm: u32) -> ExecResult {
    let (n, m) = (c.regs[rn], c.regs[rm as usize]);
    c.regs[rd] = match OP {
        OP_MOV => m,
        OP_ADD => n.wrapping_add(m),
        OP_SUB => n.wrapping_sub(m),
        OP_AND => n & m,
        OP_ORR => n | m,
        OP_EOR => n ^ m,
        _ => n & !m, // BIC
    };
    Ok(())
}

/// S=1: CMP/SUB/ADD はフラグ 4 つ、MOVS/TST は N/Z のみ（シフトなしのシフタキャリーは
/// 現在の C = 変えない。V 不変）。
#[inline(always)]
pub(super) fn dp_reg_s<const OP: u32>(c: &mut Cpu, rd: usize, rn: usize, rm: u32) -> ExecResult {
    let (n, m) = (c.regs[rn], c.regs[rm as usize]);
    match OP {
        OP_CMP => {
            nzcv(c, add_with_carry(n, !m, 1));
        }
        OP_SUB => c.regs[rd] = nzcv(c, add_with_carry(n, !m, 1)),
        OP_ADD => c.regs[rd] = nzcv(c, add_with_carry(n, m, 0)),
        OP_MOV => {
            c.cpsr = set_nz(c.cpsr, m);
            c.regs[rd] = m;
        }
        _ => c.cpsr = set_nz(c.cpsr, n & m), // TST
    }
    Ok(())
}

/// S=0 の即値シフトつきレジスタオペランド: Rd = Rn op (Rm shift #amount)。
/// imm = Rm | 種類 << 4 | シフト量 << 8。シフト量は 1〜31 だけ（0 は LSR/ASR #32・
/// RRX という別の意味になるので汎用に回す）。S=0 なのでシフタキャリーは要らない。
#[inline(always)]
pub(super) fn dp_shift<const OP: u32>(c: &mut Cpu, rd: usize, rn: usize, imm: u32) -> ExecResult {
    let m = c.regs[(imm & 0xF) as usize];
    let amount = imm >> 8; // 1〜31
    let m = match (imm >> 4) & 3 {
        0 => m << amount,
        1 => m >> amount,
        2 => ((m as i32) >> amount) as u32,
        _ => m.rotate_right(amount),
    };
    let n = c.regs[rn];
    c.regs[rd] = match OP {
        OP_MOV => m,
        OP_MVN => !m,
        OP_ADD => n.wrapping_add(m),
        OP_SUB => n.wrapping_sub(m),
        OP_RSB => m.wrapping_sub(n),
        OP_AND => n & m,
        OP_ORR => n | m,
        OP_EOR => n ^ m,
        _ => n & !m, // BIC
    };
    Ok(())
}

/// レジスタオペランド（シフトなし・即値シフト）のデータ処理（Rd・Rn・Rm が PC で
/// ない形。例外として MOV pc, Rm は分岐として特化する）。
fn spec_data_proc_reg(w: u32) -> Option<Instr> {
    let op = (w >> 21) & 0xF;
    let s = w & (1 << 20) != 0;
    let is_test = (OP_TST..=OP_CMN).contains(&op);
    if (is_test && !s) || rm(w) == 15 || rn(w) == 15 {
        return None; // MRS/MSR・BX などの空間・PC を使う形
    }
    if w & 0xFF0 != 0 {
        // 即値シフト（bit4=0 は呼び出し側で確認済み）
        let amount = (w >> 7) & 0x1F;
        if s || rd(w) == 15 || amount == 0 {
            return None;
        }
        let o = match op {
            OP_MOV => Op::MovShift,
            OP_MVN => Op::MvnShift,
            OP_ADD => Op::AddShift,
            OP_SUB => Op::SubShift,
            OP_RSB => Op::RsbShift,
            OP_AND => Op::AndShift,
            OP_ORR => Op::OrrShift,
            OP_EOR => Op::EorShift,
            OP_BIC => Op::BicShift,
            _ => return None,
        };
        return Some(Instr::special(
            o,
            w,
            rm(w) | ((w >> 5) & 3) << 4 | amount << 8,
        ));
    }
    if rd(w) == 15 {
        // MOV pc, Rm（S=0。関数からの復帰）。ARMv4T の r15 書き込みは bit1:0 を無視。
        return if !s && op == OP_MOV {
            Some(Instr::special(Op::MovPcReg, w, rm(w)))
        } else {
            None
        };
    }
    let o = match (s, op) {
        (false, OP_MOV) => Op::MovReg,
        (false, OP_ADD) => Op::AddReg,
        (false, OP_SUB) => Op::SubReg,
        (false, OP_AND) => Op::AndReg,
        (false, OP_ORR) => Op::OrrReg,
        (false, OP_EOR) => Op::EorReg,
        (false, OP_BIC) => Op::BicReg,
        (true, OP_CMP) => Op::CmpReg,
        (true, OP_SUB) => Op::SubsReg,
        (true, OP_ADD) => Op::AddsReg,
        (true, OP_MOV) => Op::MovsReg,
        (true, OP_TST) => Op::TstReg,
        _ => return None,
    };
    Some(Instr::special(o, w, rm(w)))
}

// ---- BX ----

/// BX Rm（Rm が PC でない形。rn = Rm の番号）。bit0 で ARM/Thumb を切り替える。
fn spec_bx(w: u32) -> Option<Instr> {
    if rm(w) == 15 {
        return None;
    }
    let mut i = Instr::special(Op::BxReg, w, 0);
    i.rn = rm(w) as u8;
    Some(i)
}

// ---- LDRH/STRH/LDRSB/LDRSH 即値オフセット ----

/// ハーフワード・符号付き転送の即値オフセット（プリインデックス・ライトバックなし。
/// Rd・Rn が PC でない形。imm = 符号つきのオフセット）。
fn spec_ldst_misc_imm(w: u32) -> Option<Instr> {
    // P=1・I=1（即値）・W=0
    if w & (1 << 24) == 0 || w & (1 << 22) == 0 || w & (1 << 21) != 0 {
        return None;
    }
    if rd(w) == 15 || rn(w) == 15 {
        return None;
    }
    let mut off = ((w >> 4) & 0xF0) | (w & 0xF);
    if w & (1 << 23) == 0 {
        off = off.wrapping_neg();
    }
    let o = match (w & (1 << 20) != 0, (w >> 5) & 3) {
        (true, 1) => Op::LdrhImm,
        (true, 2) => Op::LdrsbImm,
        (true, 3) => Op::LdrshImm,
        (false, 1) => Op::StrhImm,
        _ => return None, // sh=00 は乗算側。L=0 の SB/SH は LDRD/STRD（v5TE）
    };
    Some(Instr::special(o, w, off))
}

/// LDRH/LDRSB/LDRSH（非アラインのハーフワードは UNPREDICTABLE。汎用と同じくアラインする）。
#[inline(always)]
pub(super) fn ldr_half<S: System, const SH: u32>(
    c: &mut Cpu,
    sys: &mut S,
    rd: usize,
    rn: usize,
    off: u32,
) -> ExecResult {
    let addr = c.regs[rn].wrapping_add(off);
    c.regs[rd] = match SH {
        1 => sys.read(addr & !1, 2)?,
        2 => sys.read(addr, 1)? as u8 as i8 as i32 as u32,
        _ => sys.read(addr & !1, 2)? as u16 as i16 as i32 as u32,
    };
    Ok(())
}

#[inline(always)]
pub(super) fn strh_imm<S: System>(
    c: &mut Cpu,
    sys: &mut S,
    rd: usize,
    rn: usize,
    off: u32,
) -> ExecResult {
    let addr = c.regs[rn].wrapping_add(off);
    Ok(sys.write(addr & !1, 2, c.regs[rd] & 0xFFFF)?)
}

// ---- LDR/STR 即値オフセット（imm = 符号つきのオフセット。2 の補数）----

/// LDR（非アラインは ARMv4 の回転）。
#[inline(always)]
pub(super) fn ldr_imm<S: System>(
    c: &mut Cpu,
    sys: &mut S,
    rd: usize,
    rn: usize,
    off: u32,
) -> ExecResult {
    let addr = c.regs[rn].wrapping_add(off);
    c.regs[rd] = ror(sys.read(addr & !3, 4)?, 8 * (addr & 3));
    Ok(())
}

#[inline(always)]
pub(super) fn ldrb_imm<S: System>(
    c: &mut Cpu,
    sys: &mut S,
    rd: usize,
    rn: usize,
    off: u32,
) -> ExecResult {
    c.regs[rd] = sys.read(c.regs[rn].wrapping_add(off), 1)?;
    Ok(())
}

#[inline(always)]
pub(super) fn str_imm<S: System>(
    c: &mut Cpu,
    sys: &mut S,
    rd: usize,
    rn: usize,
    off: u32,
) -> ExecResult {
    Ok(sys.write(c.regs[rn].wrapping_add(off) & !3, 4, c.regs[rd])?)
}

#[inline(always)]
pub(super) fn strb_imm<S: System>(
    c: &mut Cpu,
    sys: &mut S,
    rd: usize,
    rn: usize,
    off: u32,
) -> ExecResult {
    Ok(sys.write(c.regs[rn].wrapping_add(off), 1, c.regs[rd] & 0xFF)?)
}

/// ライトバックつきの LDR/STR/LDRB/STRB 即値オフセット（PRE: プリインデックスで
/// W=1、!PRE: ポストインデックスで W=0）。汎用と同じ順序: アクセスがアボートしたら
/// ベースは書き換えない。ロードで Rd=Rn ならロード値が勝つ。ストアはベースを
/// 書き換える前の Rd を書く。
#[inline(always)]
pub(super) fn ldst_wb<S: System, const LOAD: bool, const BYTE: bool, const PRE: bool>(
    c: &mut Cpu,
    sys: &mut S,
    rd: usize,
    rn: usize,
    off: u32,
) -> ExecResult {
    let base = c.regs[rn];
    let indexed = base.wrapping_add(off);
    let addr = if PRE { indexed } else { base };
    if LOAD {
        let val = if BYTE {
            sys.read(addr, 1)?
        } else {
            ror(sys.read(addr & !3, 4)?, 8 * (addr & 3))
        };
        c.regs[rn] = indexed;
        c.regs[rd] = val;
    } else {
        let val = c.regs[rd];
        if BYTE {
            sys.write(addr, 1, val & 0xFF)?;
        } else {
            sys.write(addr & !3, 4, val)?;
        }
        c.regs[rn] = indexed;
    }
    Ok(())
}

/// LDR/STR/LDRB/STRB の即値オフセット。ロードの Rd=PC と、ストアの Rd=PC は
/// 汎用に回す。Rn=PC（リテラルプールの読み出し）は PC+8 を基準にする。
/// ライトバックつき（プリインデックス W=1・ポストインデックス W=0）は Rn が PC で
/// ない形だけ。ポストインデックスの W=1（LDRT/STRT）は汎用に回す。
fn spec_ldst_imm(w: u32) -> Option<Instr> {
    if rd(w) == 15 {
        return None;
    }
    let mut off = w & 0xFFF;
    if w & (1 << 23) == 0 {
        off = off.wrapping_neg(); // 2 の補数で加算すれば減算になる
    }
    let (load, byte) = (w & (1 << 20) != 0, w & (1 << 22) != 0);
    match (w & (1 << 24) != 0, w & (1 << 21) != 0) {
        (true, false) => {}                             // ライトバックなし（下）
        (_, true) if w & (1 << 24) == 0 => return None, // LDRT/STRT
        (pre, _) => {
            if rn(w) == 15 {
                return None;
            }
            let o = match (load, byte, pre) {
                (true, false, true) => Op::LdrPre,
                (true, false, false) => Op::LdrPost,
                (true, true, true) => Op::LdrbPre,
                (true, true, false) => Op::LdrbPost,
                (false, false, true) => Op::StrPre,
                (false, false, false) => Op::StrPost,
                (false, true, true) => Op::StrbPre,
                (false, true, false) => Op::StrbPost,
            };
            return Some(Instr::special(o, w, off));
        }
    }
    if rn(w) == 15 {
        off = off.wrapping_add(4); // regs[15] は実行中 PC+4 なので、PC+8 基準にする
    }
    let o = match (load, byte) {
        (true, false) => Op::LdrImm,
        (true, true) => Op::LdrbImm,
        (false, false) => Op::StrImm,
        (false, true) => Op::StrbImm,
    };
    Some(Instr::special(o, w, off))
}

// ---- B/BL ----

/// B/BL（オフセットを取り出し済み。PC+8 基準なので regs[15]=PC+4 に +4 を足す）。
/// L=0 で 2 命令前へ戻る分岐は 3 命令のポーリングループの可能性（idle.rs）。
fn spec_branch(w: u32) -> Instr {
    let off = ((((w << 8) as i32) >> 6) as u32).wrapping_add(4);
    let o = if w & (1 << 24) != 0 {
        Op::Bl
    } else if w & 0x01FFFFFF == 0x00FFFFFC {
        Op::BSpin
    } else {
        Op::B
    };
    Instr::special(o, w, off)
}
