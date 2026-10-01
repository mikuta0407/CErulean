//! デコード済み ARM 命令の中間表現（IR。段階4 の性能対策。計画書の段階4）。
//!
//! 1 命令を 8 バイトの [`Instr`]（演算の種類・条件・レジスタ番号・即値）にし、
//! 実行は 1 つの match（[`exec`]）で振り分ける。関数ポインタの表（16 バイト）に
//! 比べて、デコード表が半分になりキャッシュミスが減る・呼び出しの出入りが消える・
//! 頻出形の処理がループの中に展開される。
//!
//! 頻出する形（special.rs）は、デコード時にフィールドを取り出した専用の Op にする。
//! それ以外は「汎用」の Op で、imm に命令語をそのまま入れ、実行時に汎用の実行関数
//! （exec_arm.rs）が分解する。1 IR 命令 = 1 ARM 命令なので、命令数の対応は自明
//! （段階5 の JIT も同じ単位で数える）。
//!
//! 意味は汎用の実装と完全に同じでなければならない（tests.rs の
//! specialized_matches_generic がランダムな命令語と状態で突き合わせる）。

use super::exec_arm::*;
use super::special::*;
use super::*;

/// IR の演算の種類。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Op {
    // ---- 汎用（imm = 命令語）----
    DataProc,
    Ldst,
    LdstMisc,
    LdmStm,
    MulSwp,
    Mrs,
    Msr,
    Branch,
    Bx,
    Swi,
    McrMrc,
    /// cond=1111 の拡張空間（ARMv5+。未実装で停止）
    UnimplV5,
    /// MRS に似た即値形式（未定義。停止）
    UnimplMrsImm,
    /// bits[27:25]=011 かつ bit4=1（アーキテクチャ上の未定義。ゲストに配送）
    UndefLdstReg,
    /// LDC/STC（コプロセッサなし。ゲストに配送）
    UndefLdcStc,
    /// CDP（コプロセッサなし。ゲストに配送）
    UndefCdp,

    // ---- 特化（rd・rn はレジスタ番号、imm は取り出し済みの値。special.rs）----
    /// Rd = imm（MVN は反転済みの imm）
    MovImm,
    AddImm,
    SubImm,
    AndImm,
    OrrImm,
    EorImm,
    BicImm,
    CmpImm,
    CmnImm,
    SubsImm,
    AddsImm,
    /// 論理演算の S=1。末尾 C は「即値の回転あり」（C フラグに即値の bit31 を入れる）
    TstImm,
    TstImmC,
    TeqImm,
    TeqImmC,
    AndsImm,
    AndsImmC,
    OrrsImm,
    OrrsImmC,
    MovsImm,
    MovsImmC,
    /// レジスタオペランド（シフトなし）。imm = Rm の番号
    MovReg,
    AddReg,
    SubReg,
    AndReg,
    OrrReg,
    EorReg,
    BicReg,
    CmpReg,
    SubsReg,
    AddsReg,
    MovsReg,
    TstReg,
    /// S=0 の即値シフトつきレジスタオペランド（imm = Rm | 種類 << 4 | シフト量 << 8）
    MovShift,
    MvnShift,
    AddShift,
    SubShift,
    RsbShift,
    AndShift,
    OrrShift,
    EorShift,
    BicShift,
    /// MOV pc, Rm（imm = Rm の番号）
    MovPcReg,
    /// BX Rm（rn = Rm の番号）
    BxReg,
    /// ハーフワード・符号付き転送の即値オフセット（imm = 符号つきのオフセット）
    LdrhImm,
    LdrsbImm,
    LdrshImm,
    StrhImm,
    /// ライトバックつきの即値オフセット（Pre: プリインデックス W=1、Post:
    /// ポストインデックス。imm = 符号つきのオフセット）
    LdrPre,
    LdrPost,
    LdrbPre,
    LdrbPost,
    StrPre,
    StrPost,
    StrbPre,
    StrbPost,
    /// 即値オフセット（imm = 符号つきのオフセット。Rn=PC なら PC+8 基準に補正済み）
    LdrImm,
    LdrbImm,
    StrImm,
    StrbImm,
    /// imm = 分岐先の差分（regs[15]=PC+4 に足す）
    B,
    Bl,
    /// 3 命令ループ先頭への後方分岐（idle.rs）
    BSpin,
}

/// デコード済みの ARM 命令（8 バイト）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instr {
    pub op: Op,
    /// 条件フィールド（命令語の bits[31:28]）
    pub cond: u8,
    pub rd: u8,
    pub rn: u8,
    /// 汎用: 命令語。特化: Op ごとの値
    pub imm: u32,
}

impl Instr {
    /// 汎用の Op（imm = 命令語）。
    pub(crate) fn generic(op: Op, word: u32) -> Instr {
        Instr {
            op,
            cond: (word >> 28) as u8,
            rd: 0,
            rn: 0,
            imm: word,
        }
    }

    /// 特化した Op（レジスタ番号は命令語の Rd・Rn の位置から取る）。
    pub(crate) fn special(op: Op, word: u32, imm: u32) -> Instr {
        Instr {
            op,
            cond: (word >> 28) as u8,
            rd: ((word >> 12) & 0xF) as u8,
            rn: ((word >> 16) & 0xF) as u8,
            imm,
        }
    }

    /// エラー表示用の命令語。未定義命令のエラーは汎用の Op からしか出ない
    /// （特化した Op のエラーはメモリのアボート・バスエラーだけ）。
    #[inline(always)]
    pub(crate) fn word(&self) -> u32 {
        if (self.op as u8) <= Op::UndefCdp as u8 {
            self.imm
        } else {
            0
        }
    }
}

/// ARM 命令語 1 個をデコードする（命令語だけの純関数）。頻出する形には
/// 特化した Op を、それ以外には汎用の Op を選ぶ。
pub fn decode_instr(word: u32) -> Instr {
    specialize(word).unwrap_or_else(|| Instr::generic(decode(word), word))
}

/// IR 命令を 1 個実行する（条件は呼び出し側が判定済み。regs[15] = PC+4）。
#[inline(always)]
pub(crate) fn exec<S: System>(c: &mut Cpu, sys: &mut S, i: Instr) -> ExecResult {
    let (rd, rn, imm) = (i.rd as usize, i.rn as usize, i.imm);
    match i.op {
        Op::DataProc => exec_data_proc(c, sys, imm),
        Op::Ldst => exec_ldst(c, sys, imm),
        Op::LdstMisc => exec_ldst_misc(c, sys, imm),
        Op::LdmStm => exec_ldm_stm(c, sys, imm),
        Op::MulSwp => exec_mul_swp(c, sys, imm),
        Op::Mrs => exec_mrs(c, sys, imm),
        Op::Msr => exec_msr(c, sys, imm),
        Op::Branch => exec_branch(c, sys, imm),
        Op::Bx => exec_bx(c, sys, imm),
        Op::Swi => exec_swi(c, sys),
        Op::McrMrc => exec_mcr_mrc(c, sys, imm),
        Op::UnimplV5 => Err(unimpl!("cond=1111 extension space (ARMv5+)")),
        Op::UnimplMrsImm => Err(unimpl!("undefined (MRS-like encoding with immediate)")),
        Op::UndefLdstReg => Err(Exec::Undef(&Undef {
            reason: "architecturally undefined space (011 with bit4)",
            arch: true,
        })),
        Op::UndefLdcStc => Err(Exec::Undef(&Undef {
            reason: "LDC/STC (no coprocessor)",
            arch: true,
        })),
        Op::UndefCdp => Err(Exec::Undef(&Undef {
            reason: "CDP (no coprocessor)",
            arch: true,
        })),

        Op::MovImm => dp_imm::<OP_MOV>(c, rd, rn, imm),
        Op::AddImm => dp_imm::<OP_ADD>(c, rd, rn, imm),
        Op::SubImm => dp_imm::<OP_SUB>(c, rd, rn, imm),
        Op::AndImm => dp_imm::<OP_AND>(c, rd, rn, imm),
        Op::OrrImm => dp_imm::<OP_ORR>(c, rd, rn, imm),
        Op::EorImm => dp_imm::<OP_EOR>(c, rd, rn, imm),
        Op::BicImm => dp_imm::<OP_BIC>(c, rd, rn, imm),
        Op::CmpImm => dp_imm_arith::<OP_CMP>(c, rd, rn, imm),
        Op::CmnImm => dp_imm_arith::<OP_CMN>(c, rd, rn, imm),
        Op::SubsImm => dp_imm_arith::<OP_SUB>(c, rd, rn, imm),
        Op::AddsImm => dp_imm_arith::<OP_ADD>(c, rd, rn, imm),
        Op::TstImm => dp_imm_logic::<OP_TST, false>(c, rd, rn, imm),
        Op::TstImmC => dp_imm_logic::<OP_TST, true>(c, rd, rn, imm),
        Op::TeqImm => dp_imm_logic::<OP_TEQ, false>(c, rd, rn, imm),
        Op::TeqImmC => dp_imm_logic::<OP_TEQ, true>(c, rd, rn, imm),
        Op::AndsImm => dp_imm_logic::<OP_AND, false>(c, rd, rn, imm),
        Op::AndsImmC => dp_imm_logic::<OP_AND, true>(c, rd, rn, imm),
        Op::OrrsImm => dp_imm_logic::<OP_ORR, false>(c, rd, rn, imm),
        Op::OrrsImmC => dp_imm_logic::<OP_ORR, true>(c, rd, rn, imm),
        Op::MovsImm => dp_imm_logic::<OP_MOV, false>(c, rd, rn, imm),
        Op::MovsImmC => dp_imm_logic::<OP_MOV, true>(c, rd, rn, imm),
        Op::MovReg => dp_reg::<OP_MOV>(c, rd, rn, imm),
        Op::AddReg => dp_reg::<OP_ADD>(c, rd, rn, imm),
        Op::SubReg => dp_reg::<OP_SUB>(c, rd, rn, imm),
        Op::AndReg => dp_reg::<OP_AND>(c, rd, rn, imm),
        Op::OrrReg => dp_reg::<OP_ORR>(c, rd, rn, imm),
        Op::EorReg => dp_reg::<OP_EOR>(c, rd, rn, imm),
        Op::BicReg => dp_reg::<OP_BIC>(c, rd, rn, imm),
        Op::CmpReg => dp_reg_s::<OP_CMP>(c, rd, rn, imm),
        Op::SubsReg => dp_reg_s::<OP_SUB>(c, rd, rn, imm),
        Op::AddsReg => dp_reg_s::<OP_ADD>(c, rd, rn, imm),
        Op::MovsReg => dp_reg_s::<OP_MOV>(c, rd, rn, imm),
        Op::TstReg => dp_reg_s::<OP_TST>(c, rd, rn, imm),
        Op::MovShift => dp_shift::<OP_MOV>(c, rd, rn, imm),
        Op::MvnShift => dp_shift::<OP_MVN>(c, rd, rn, imm),
        Op::AddShift => dp_shift::<OP_ADD>(c, rd, rn, imm),
        Op::SubShift => dp_shift::<OP_SUB>(c, rd, rn, imm),
        Op::RsbShift => dp_shift::<OP_RSB>(c, rd, rn, imm),
        Op::AndShift => dp_shift::<OP_AND>(c, rd, rn, imm),
        Op::OrrShift => dp_shift::<OP_ORR>(c, rd, rn, imm),
        Op::EorShift => dp_shift::<OP_EOR>(c, rd, rn, imm),
        Op::BicShift => dp_shift::<OP_BIC>(c, rd, rn, imm),
        Op::MovPcReg => {
            c.regs[15] = c.regs[imm as usize] & !3;
            Ok(())
        }
        Op::BxReg => {
            let target = c.regs[rn];
            if target & 1 != 0 {
                c.cpsr |= FLAG_T;
                c.regs[15] = target & !1;
            } else {
                c.cpsr &= !FLAG_T;
                c.regs[15] = target & !3;
            }
            Ok(())
        }
        Op::LdrhImm => ldr_half::<S, 1>(c, sys, rd, rn, imm),
        Op::LdrsbImm => ldr_half::<S, 2>(c, sys, rd, rn, imm),
        Op::LdrshImm => ldr_half::<S, 3>(c, sys, rd, rn, imm),
        Op::StrhImm => strh_imm(c, sys, rd, rn, imm),
        Op::LdrPre => ldst_wb::<S, true, false, true>(c, sys, rd, rn, imm),
        Op::LdrPost => ldst_wb::<S, true, false, false>(c, sys, rd, rn, imm),
        Op::LdrbPre => ldst_wb::<S, true, true, true>(c, sys, rd, rn, imm),
        Op::LdrbPost => ldst_wb::<S, true, true, false>(c, sys, rd, rn, imm),
        Op::StrPre => ldst_wb::<S, false, false, true>(c, sys, rd, rn, imm),
        Op::StrPost => ldst_wb::<S, false, false, false>(c, sys, rd, rn, imm),
        Op::StrbPre => ldst_wb::<S, false, true, true>(c, sys, rd, rn, imm),
        Op::StrbPost => ldst_wb::<S, false, true, false>(c, sys, rd, rn, imm),
        Op::LdrImm => ldr_imm(c, sys, rd, rn, imm),
        Op::LdrbImm => ldrb_imm(c, sys, rd, rn, imm),
        Op::StrImm => str_imm(c, sys, rd, rn, imm),
        Op::StrbImm => strb_imm(c, sys, rd, rn, imm),
        Op::B => {
            c.regs[15] = c.regs[15].wrapping_add(imm);
            Ok(())
        }
        Op::Bl => {
            c.regs[14] = c.regs[15];
            c.regs[15] = c.regs[15].wrapping_add(imm);
            Ok(())
        }
        Op::BSpin => {
            // 印を付けて run を打ち切る（machine がアイドルスキップを試す）。
            c.regs[15] = c.regs[15].wrapping_add(imm);
            c.spin_hint = true;
            sys.run_ctl().budget = 0;
            Ok(())
        }
    }
}
