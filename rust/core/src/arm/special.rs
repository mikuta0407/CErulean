//! 頻出する命令形の特化（性能対策。ユーザー確認済み 2026-09。Go の special.go）。
//!
//! デコード時に、よく実行される形の命令には専用の実行関数を選び、即値（回転済みの
//! 即値・符号つきのオフセット・分岐先の差分）を取り出しておく。汎用の実行関数
//! （exec_data_proc など）は実行のたびに命令語を分解し、16 種類の演算の match と
//! フラグ処理の分岐を通るため。デコード結果はデコードキャッシュに残るので、
//! 取り出しは物理ページ上の命令ごとに 1 回で済む。
//!
//! Go ではクロージャでフィールドを取り込んでいた。Rust の関数ポインタは値を持てない
//! ので、演算の種類は const ジェネリクス（関数ごとに別の実体になる）、即値は
//! デコード済み命令の imm で渡す。レジスタ番号は命令語から取り出す（シフトと
//! マスクだけなので十分安い）。
//!
//! 対象は WM5 の実行で多い形（2026-09 に実行命令の分布を計測: LDR 即値 18%、
//! CMP 即値 8%、MOV 即値・レジスタ 各 7〜8%、条件分岐 9% など）。
//! 意味は汎用の実装と完全に同じでなければならない（tests.rs の
//! specialized_matches_generic がランダムな命令語と状態で突き合わせる）。
//! 境界的な形（Rd/Rn=PC、ライトバック、シフトつきオペランド、ADC/SBC/RSB/RSC 等）は
//! 特化せず汎用に回す。

use super::code::Instr;
use super::exec_arm::*;
use super::*;

#[inline(always)]
fn rd(w: u32) -> usize {
    ((w >> 12) & 0xF) as usize
}

#[inline(always)]
fn rn(w: u32) -> usize {
    ((w >> 16) & 0xF) as usize
}

#[inline(always)]
fn rm(w: u32) -> usize {
    (w & 0xF) as usize
}

/// 加減算の結果からフラグ 4 つを立てる。
#[inline(always)]
fn nzcv(c: &mut Cpu, (res, cf, vf): (u32, bool, bool)) -> u32 {
    c.cpsr = set_flag(set_flag(set_nz(c.cpsr, res), FLAG_C, cf), FLAG_V, vf);
    res
}

/// word に専用の実行関数があれば返す（なければ None）。条件フィールドは実行
/// ループが判定するので、ここでは見ない（0xF は対象外）。
pub(crate) fn specialize<S: System>(word: u32) -> Option<Instr<S>> {
    if word >> 28 == 0xF {
        return None;
    }
    match (word >> 25) & 7 {
        1 => spec_data_proc_imm(word),
        0 if word & 0xFF0 == 0 => spec_data_proc_reg(word), // レジスタ・シフトなし（LSL #0）
        2 => spec_ldst_imm(word),
        5 => Some(spec_branch(word)),
        _ => None,
    }
}

fn instr<S>(exec: ExecFn<S>, word: u32, imm: u32) -> Option<Instr<S>> {
    Some(Instr { exec, word, imm })
}

// ---- データ処理（即値オペランド）----

/// S=0 の即値演算: Rd = Rn op imm（MOV/MVN は imm そのもの。MVN は反転済み）。
fn dp_imm<S: System, const OP: u32>(c: &mut Cpu, _: &mut S, w: u32, imm: u32) -> ExecResult {
    let n = c.regs[rn(w)];
    c.regs[rd(w)] = match OP {
        OP_MOV | OP_MVN => imm,
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
fn dp_imm_arith<S: System, const OP: u32>(c: &mut Cpu, _: &mut S, w: u32, imm: u32) -> ExecResult {
    let n = c.regs[rn(w)];
    let r = match OP {
        OP_CMP | OP_SUB => add_with_carry(n, !imm, 1),
        _ => add_with_carry(n, imm, 0), // CMN/ADD
    };
    let res = nzcv(c, r);
    if OP == OP_SUB || OP == OP_ADD {
        c.regs[rd(w)] = res;
    }
    Ok(())
}

/// S=1 の即値の論理演算: N/Z は結果、C はシフタキャリー（回転ありなら即値の
/// bit31、なしなら現在の C = 変えない）、V は不変。SETC は「回転あり」。
fn dp_imm_logic<S: System, const OP: u32, const SETC: bool>(
    c: &mut Cpu,
    _: &mut S,
    w: u32,
    imm: u32,
) -> ExecResult {
    let n = c.regs[rn(w)];
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
        c.regs[rd(w)] = res;
    }
    Ok(())
}

/// 即値オペランドのデータ処理（Rd・Rn が PC でない形）。
fn spec_data_proc_imm<S: System>(w: u32) -> Option<Instr<S>> {
    let op = (w >> 21) & 0xF;
    let s = w & (1 << 20) != 0;
    let is_test = (OP_TST..=OP_CMN).contains(&op);
    if (is_test && !s) || rn(w) == 15 || (rd(w) == 15 && !is_test) {
        return None; // MRS/MSR の空間・PC を使う形
    }
    let rot = ((w >> 8) & 0xF) * 2;
    let imm = ror(w & 0xFF, rot);
    if !s {
        let f: ExecFn<S> = match op {
            OP_MOV => dp_imm::<S, OP_MOV>,
            OP_MVN => return instr(dp_imm::<S, OP_MVN>, w, !imm),
            OP_ADD => dp_imm::<S, OP_ADD>,
            OP_SUB => dp_imm::<S, OP_SUB>,
            OP_AND => dp_imm::<S, OP_AND>,
            OP_ORR => dp_imm::<S, OP_ORR>,
            OP_EOR => dp_imm::<S, OP_EOR>,
            OP_BIC => dp_imm::<S, OP_BIC>,
            _ => return None,
        };
        return instr(f, w, imm);
    }
    let setc = rot != 0;
    let f: ExecFn<S> = match (op, setc) {
        (OP_CMP, _) => dp_imm_arith::<S, OP_CMP>,
        (OP_CMN, _) => dp_imm_arith::<S, OP_CMN>,
        (OP_SUB, _) => dp_imm_arith::<S, OP_SUB>,
        (OP_ADD, _) => dp_imm_arith::<S, OP_ADD>,
        (OP_TST, false) => dp_imm_logic::<S, OP_TST, false>,
        (OP_TST, true) => dp_imm_logic::<S, OP_TST, true>,
        (OP_TEQ, false) => dp_imm_logic::<S, OP_TEQ, false>,
        (OP_TEQ, true) => dp_imm_logic::<S, OP_TEQ, true>,
        (OP_AND, false) => dp_imm_logic::<S, OP_AND, false>,
        (OP_AND, true) => dp_imm_logic::<S, OP_AND, true>,
        (OP_ORR, false) => dp_imm_logic::<S, OP_ORR, false>,
        (OP_ORR, true) => dp_imm_logic::<S, OP_ORR, true>,
        (OP_MOV, false) => dp_imm_logic::<S, OP_MOV, false>,
        (OP_MOV, true) => dp_imm_logic::<S, OP_MOV, true>,
        _ => return None,
    };
    instr(f, w, imm)
}

// ---- データ処理（レジスタオペランド、シフトなし）----

/// S=0: Rd = Rn op Rm。
fn dp_reg<S: System, const OP: u32>(c: &mut Cpu, _: &mut S, w: u32, _: u32) -> ExecResult {
    let (n, m) = (c.regs[rn(w)], c.regs[rm(w)]);
    c.regs[rd(w)] = match OP {
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
fn dp_reg_s<S: System, const OP: u32>(c: &mut Cpu, _: &mut S, w: u32, _: u32) -> ExecResult {
    let (n, m) = (c.regs[rn(w)], c.regs[rm(w)]);
    match OP {
        OP_CMP => {
            nzcv(c, add_with_carry(n, !m, 1));
        }
        OP_SUB => c.regs[rd(w)] = nzcv(c, add_with_carry(n, !m, 1)),
        OP_ADD => c.regs[rd(w)] = nzcv(c, add_with_carry(n, m, 0)),
        OP_MOV => {
            c.cpsr = set_nz(c.cpsr, m);
            c.regs[rd(w)] = m;
        }
        _ => c.cpsr = set_nz(c.cpsr, n & m), // TST
    }
    Ok(())
}

/// レジスタオペランド（シフトなし）のデータ処理（Rd・Rn・Rm が PC でない形）。
fn spec_data_proc_reg<S: System>(w: u32) -> Option<Instr<S>> {
    let op = (w >> 21) & 0xF;
    let s = w & (1 << 20) != 0;
    let is_test = (OP_TST..=OP_CMN).contains(&op);
    if (is_test && !s) || rm(w) == 15 || rn(w) == 15 || (rd(w) == 15 && !is_test) {
        return None; // MRS/MSR・BX などの空間・PC を使う形
    }
    let f: ExecFn<S> = match (s, op) {
        (false, OP_MOV) => dp_reg::<S, OP_MOV>,
        (false, OP_ADD) => dp_reg::<S, OP_ADD>,
        (false, OP_SUB) => dp_reg::<S, OP_SUB>,
        (false, OP_AND) => dp_reg::<S, OP_AND>,
        (false, OP_ORR) => dp_reg::<S, OP_ORR>,
        (false, OP_EOR) => dp_reg::<S, OP_EOR>,
        (false, OP_BIC) => dp_reg::<S, OP_BIC>,
        (true, OP_CMP) => dp_reg_s::<S, OP_CMP>,
        (true, OP_SUB) => dp_reg_s::<S, OP_SUB>,
        (true, OP_ADD) => dp_reg_s::<S, OP_ADD>,
        (true, OP_MOV) => dp_reg_s::<S, OP_MOV>,
        (true, OP_TST) => dp_reg_s::<S, OP_TST>,
        _ => return None,
    };
    instr(f, w, 0)
}

// ---- LDR/STR 即値オフセット ----

/// LDR（非アラインは ARMv4 の回転）。imm は符号つきのオフセット（2 の補数）。
fn ldr_imm<S: System>(c: &mut Cpu, sys: &mut S, w: u32, off: u32) -> ExecResult {
    let addr = c.regs[rn(w)].wrapping_add(off);
    c.regs[rd(w)] = ror(sys.read(addr & !3, 4)?, 8 * (addr & 3));
    Ok(())
}

fn ldrb_imm<S: System>(c: &mut Cpu, sys: &mut S, w: u32, off: u32) -> ExecResult {
    c.regs[rd(w)] = sys.read(c.regs[rn(w)].wrapping_add(off), 1)?;
    Ok(())
}

fn str_imm<S: System>(c: &mut Cpu, sys: &mut S, w: u32, off: u32) -> ExecResult {
    Ok(sys.write(c.regs[rn(w)].wrapping_add(off) & !3, 4, c.regs[rd(w)])?)
}

fn strb_imm<S: System>(c: &mut Cpu, sys: &mut S, w: u32, off: u32) -> ExecResult {
    Ok(sys.write(c.regs[rn(w)].wrapping_add(off), 1, c.regs[rd(w)] & 0xFF)?)
}

/// LDR/STR/LDRB/STRB の即値オフセット（プリインデックス・ライトバックなし）。
/// ロードの Rd=PC と、ストアの Rd=PC は汎用に回す。Rn=PC（リテラルプールの
/// 読み出し）は PC+8 を基準にする。
fn spec_ldst_imm<S: System>(w: u32) -> Option<Instr<S>> {
    if w & (1 << 24) == 0 || w & (1 << 21) != 0 || rd(w) == 15 {
        return None; // ポストインデックス・ライトバック
    }
    let mut off = w & 0xFFF;
    if w & (1 << 23) == 0 {
        off = off.wrapping_neg(); // 2 の補数で加算すれば減算になる
    }
    if rn(w) == 15 {
        off = off.wrapping_add(4); // regs[15] は実行中 PC+4 なので、PC+8 基準にする
    }
    let f: ExecFn<S> = match (w & (1 << 20) != 0, w & (1 << 22) != 0) {
        (true, false) => ldr_imm::<S>,
        (true, true) => ldrb_imm::<S>,
        (false, false) => str_imm::<S>,
        (false, true) => strb_imm::<S>,
    };
    instr(f, w, off)
}

// ---- B/BL ----

fn b<S: System>(c: &mut Cpu, _: &mut S, _: u32, off: u32) -> ExecResult {
    c.regs[15] = c.regs[15].wrapping_add(off);
    Ok(())
}

fn bl<S: System>(c: &mut Cpu, _: &mut S, _: u32, off: u32) -> ExecResult {
    c.regs[14] = c.regs[15];
    c.regs[15] = c.regs[15].wrapping_add(off);
    Ok(())
}

/// 3 命令ループ先頭への後方分岐（idle.rs）。印を付けて run を打ち切る。
fn b_spin<S: System>(c: &mut Cpu, sys: &mut S, _: u32, off: u32) -> ExecResult {
    c.regs[15] = c.regs[15].wrapping_add(off);
    c.spin_hint = true;
    sys.run_ctl().budget = 0;
    Ok(())
}

/// B/BL（オフセットを取り出し済み。PC+8 基準なので regs[15]=PC+4 に +4 を足す）。
fn spec_branch<S: System>(w: u32) -> Instr<S> {
    let off = ((((w << 8) as i32) >> 6) as u32).wrapping_add(4);
    let f: ExecFn<S> = if w & (1 << 24) != 0 {
        bl::<S>
    } else if w & 0x01FFFFFF == 0x00FFFFFC {
        b_spin::<S>
    } else {
        b::<S>
    };
    Instr {
        exec: f,
        word: w,
        imm: off,
    }
}
