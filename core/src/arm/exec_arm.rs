//! ARM 命令のデコードと実行。
//!
//! ゲストの演算は wrapping で書き、可変のシフト量は必ず場合分けする。
//! 32 ビット以上のシフトでもゲストの規則に沿った値とキャリーを返す。

use super::ir::Op;
use super::*;

/// ARM 命令語 1 個の汎用の Op を選ぶ（特化しない場合。ir.rs）。未実装・未定義の
/// 命令も「実行するとエラーを返す Op」として返す（デコード自体は失敗しない）。
/// 命令語だけの純関数なので、結果を物理アドレスをキーにキャッシュしても正しさが保たれる。
///
/// ARM ARM Figure A3-1 の命令クラス分けに従う。分岐の順序が重要:
/// 「データ処理レジスタ形式」の空間には bit7/bit4 や S ビットの組み合わせで
/// 乗算・MRS/MSR・BX などが埋め込まれている。
pub(crate) fn decode(word: u32) -> Op {
    if word >> 28 == 0xF {
        // ARMv4 では UNPREDICTABLE、v5 以降は BLX 等の拡張空間。
        // （実行ループの条件判定で NOP として飛ばされるのでここには来ない。cond_passed 参照）
        // TODO(v5TE): PXA27x 対応時に BLX(1) 等を実装する。
        return Op::UnimplV5;
    }
    match (word >> 25) & 7 {
        0 => {
            // データ処理（レジスタ形式）とその同居命令
            // BX: 0001 0010 1111 1111 1111 0001
            if word & 0x0FFFFFF0 == 0x012FFF10 {
                return Op::Bx;
            }
            if word & 0x90 == 0x90 {
                // bit7=1 かつ bit4=1: データ処理ではない
                if (word >> 5) & 3 == 0 {
                    // bits[7:4] = 1001: 乗算（MUL/MLA/UMULL...）または SWP
                    return Op::MulSwp;
                }
                // bits[7:4] = 1011/1101/1111: ハーフワード・符号付き転送
                return Op::LdstMisc;
            }
            let op = (word >> 21) & 0xF;
            if (8..=11).contains(&op) && word & (1 << 20) == 0 {
                // TST/TEQ/CMP/CMN の S=0 は MRS/MSR の空間
                return if op & 1 == 0 { Op::Mrs } else { Op::Msr };
            }
            Op::DataProc
        }
        1 => {
            // データ処理（即値形式）
            let op = (word >> 21) & 0xF;
            if (8..=11).contains(&op) && word & (1 << 20) == 0 {
                if op & 1 == 1 {
                    return Op::Msr; // MSR 即値形式
                }
                return Op::UnimplMrsImm;
            }
            Op::DataProc
        }
        2 => Op::Ldst, // LDR/STR 即値オフセット
        3 => {
            // LDR/STR レジスタオフセット
            if word & 0x10 != 0 {
                // bits[27:25]=011 かつ bit4=1 は ARMv4 のアーキテクチャ未定義空間
                // （実機でも未定義例外）。WinCE はこの空間の命令をトラップとして
                // 意図的に実行するので、例外として配送する。
                return Op::UndefLdstReg;
            }
            Op::Ldst
        }
        4 => Op::LdmStm,
        5 => Op::Branch,
        // コプロセッサ LDC/STC: 対応コプロセッサがないので実機同様に未定義例外
        6 => Op::UndefLdcStc,
        _ => {
            // 7: コプロセッサ演算・レジスタ転送、SWI
            if word & (1 << 24) != 0 {
                return Op::Swi;
            }
            if word & 0x10 != 0 {
                return Op::McrMrc;
            }
            Op::UndefCdp
        }
    }
}

// ---- データ処理命令（ARM ARM A3.4）とバレルシフタ（A5.1）----
//
// フラグ計算の要点:
//   - 論理系（AND/EOR/TST/TEQ/ORR/MOV/BIC/MVN）: N/Z は結果から、
//     C は「シフタキャリー」、V は変化しない。
//   - 加算系（ADD/ADC/CMN）: C はキャリーアウト、V は符号付きオーバーフロー。
//   - 減算系（SUB/SBC/RSB/RSC/CMP）: a-b を a+!b+1 として計算するので、
//     C は「ボローなし」で 1 になる（x86 と逆なので注意）。

#[inline(always)]
pub(crate) fn ror(v: u32, n: u32) -> u32 {
    v.rotate_right(n & 31)
}

/// a + b + cin を計算し、(結果, キャリー, オーバーフロー) を返す。
/// 減算は b をビット反転して cin を調整すれば同じ式に乗る（ARM ARM の定義どおり）。
#[inline(always)]
pub(crate) fn add_with_carry(a: u32, b: u32, cin: u32) -> (u32, bool, bool) {
    let r64 = a as u64 + b as u64 + cin as u64;
    let res = r64 as u32;
    // V: 同符号の 2 数を足して結果の符号が変わったらオーバーフロー
    (
        res,
        r64 > 0xFFFFFFFF,
        (a ^ res) & (b ^ res) & 0x80000000 != 0,
    )
}

/// 即値シフト量によるシフト（A5.1.5 など）。
/// ARM のエンコードでは「シフト量 0」が種類ごとに特別な意味を持つ:
/// LSR #0 → LSR #32、ASR #0 → ASR #32、ROR #0 → RRX。amount は 0〜31。
#[inline(always)]
pub(crate) fn shift_imm(v: u32, shift_type: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    match shift_type {
        0 => {
            // LSL
            if amount == 0 {
                return (v, carry_in);
            }
            (v << amount, v & (1 << (32 - amount)) != 0)
        }
        1 => {
            // LSR
            if amount == 0 {
                return (0, v & 0x80000000 != 0); // LSR #32
            }
            (v >> amount, v & (1 << (amount - 1)) != 0)
        }
        2 => {
            // ASR
            if amount == 0 {
                // ASR #32
                return if v & 0x80000000 != 0 {
                    (0xFFFFFFFF, true)
                } else {
                    (0, false)
                };
            }
            (((v as i32) >> amount) as u32, v & (1 << (amount - 1)) != 0)
        }
        _ => {
            // ROR / RRX
            if amount == 0 {
                // RRX: キャリーを最上位に入れて 1 ビット右回転
                let res = v >> 1 | if carry_in { 0x80000000 } else { 0 };
                return (res, v & 1 != 0);
            }
            let res = ror(v, amount);
            (res, res & 0x80000000 != 0)
        }
    }
}

/// レジスタ指定シフト量（Rs の下位 8 ビット。0〜255）によるシフト。
/// 即値と違い、量 0 は「シフトなし」、32 以上も規定がある（A5.1.7 など）。
#[inline(always)]
pub(crate) fn shift_reg(v: u32, shift_type: u32, amount: u32, carry_in: bool) -> (u32, bool) {
    if amount == 0 {
        return (v, carry_in);
    }
    match shift_type {
        0 => match amount {
            // LSL
            1..=31 => (v << amount, v & (1 << (32 - amount)) != 0),
            32 => (0, v & 1 != 0),
            _ => (0, false),
        },
        1 => match amount {
            // LSR
            1..=31 => (v >> amount, v & (1 << (amount - 1)) != 0),
            32 => (0, v & 0x80000000 != 0),
            _ => (0, false),
        },
        2 => {
            // ASR
            if amount < 32 {
                return (((v as i32) >> amount) as u32, v & (1 << (amount - 1)) != 0);
            }
            if v & 0x80000000 != 0 {
                (0xFFFFFFFF, true)
            } else {
                (0, false)
            }
        }
        _ => {
            // ROR
            let a = amount & 31;
            if a == 0 {
                return (v, v & 0x80000000 != 0); // 32 の倍数: 値そのまま、C は bit31
            }
            let res = ror(v, a);
            (res, res & 0x80000000 != 0)
        }
    }
}

impl Cpu {
    #[inline(always)]
    pub(crate) fn flag_c(&self) -> bool {
        self.cpsr & FLAG_C != 0
    }

    /// shifter_operand を評価して（値, シフタキャリー）を返す。
    #[inline(always)]
    fn data_proc_operand(&self, word: u32) -> (u32, bool) {
        let carry_in = self.flag_c();
        if word & (1 << 25) != 0 {
            // 即値: imm8 を rot*2 だけ右ローテート
            let rot = ((word >> 8) & 0xF) * 2;
            let val = ror(word & 0xFF, rot);
            if rot == 0 {
                return (val, carry_in);
            }
            return (val, val & 0x80000000 != 0);
        }
        let rm = self.read_reg(word & 0xF);
        let shift_type = (word >> 5) & 3;
        if word & (1 << 4) != 0 {
            // レジスタ指定シフト。Rm/Rn に r15 を使うのは UNPREDICTABLE なので
            // PC+8/+12 の区別はしない（read_reg の +8 のまま）。
            let amount = self.read_reg((word >> 8) & 0xF) & 0xFF;
            return shift_reg(rm, shift_type, amount, carry_in);
        }
        shift_imm(rm, shift_type, (word >> 7) & 0x1F, carry_in)
    }
}

// データ処理オペコード（bits 24:21）。
pub(crate) const OP_AND: u32 = 0;
pub(crate) const OP_EOR: u32 = 1;
pub(crate) const OP_SUB: u32 = 2;
pub(crate) const OP_RSB: u32 = 3;
pub(crate) const OP_ADD: u32 = 4;
pub(crate) const OP_ADC: u32 = 5;
pub(crate) const OP_SBC: u32 = 6;
pub(crate) const OP_RSC: u32 = 7;
pub(crate) const OP_TST: u32 = 8;
pub(crate) const OP_TEQ: u32 = 9;
pub(crate) const OP_CMP: u32 = 10;
pub(crate) const OP_CMN: u32 = 11;
pub(crate) const OP_ORR: u32 = 12;
pub(crate) const OP_MOV: u32 = 13;
pub(crate) const OP_BIC: u32 = 14;
pub(crate) const OP_MVN: u32 = 15;

pub(super) fn exec_data_proc<S: System>(c: &mut Cpu, sys: &mut S, word: u32) -> ExecResult {
    let op = (word >> 21) & 0xF;
    let s_bit = word & (1 << 20) != 0;
    let rn = (word >> 16) & 0xF;
    let rd = (word >> 12) & 0xF;

    let (opnd2, sh_carry) = c.data_proc_operand(word);
    let rn_val = c.read_reg(rn);
    let cin = c.flag_c() as u32;

    // arithmetic は加減算系なら Some((C, V))（C/V の由来が違う）。
    let (res, arith) = match op {
        OP_AND | OP_TST => (rn_val & opnd2, None),
        OP_EOR | OP_TEQ => (rn_val ^ opnd2, None),
        OP_SUB | OP_CMP => {
            let (r, cf, vf) = add_with_carry(rn_val, !opnd2, 1);
            (r, Some((cf, vf)))
        }
        OP_RSB => {
            let (r, cf, vf) = add_with_carry(opnd2, !rn_val, 1);
            (r, Some((cf, vf)))
        }
        OP_ADD | OP_CMN => {
            let (r, cf, vf) = add_with_carry(rn_val, opnd2, 0);
            (r, Some((cf, vf)))
        }
        OP_ADC => {
            let (r, cf, vf) = add_with_carry(rn_val, opnd2, cin);
            (r, Some((cf, vf)))
        }
        OP_SBC => {
            let (r, cf, vf) = add_with_carry(rn_val, !opnd2, cin);
            (r, Some((cf, vf)))
        }
        OP_RSC => {
            let (r, cf, vf) = add_with_carry(opnd2, !rn_val, cin);
            (r, Some((cf, vf)))
        }
        OP_ORR => (rn_val | opnd2, None),
        OP_MOV => (opnd2, None),
        OP_BIC => (rn_val & !opnd2, None),
        _ => {
            debug_assert_eq!(op, OP_MVN);
            (!opnd2, None)
        }
    };

    let is_test = (OP_TST..=OP_CMN).contains(&op);

    if s_bit {
        if rd == 15 && !is_test {
            // 例外復帰イディオム（MOVS pc, lr / SUBS pc, lr, #4）:
            // フラグ更新ではなく CPSR ← SPSR。
            let b = c.cur_bank();
            if b != BANK_USR {
                c.write_cpsr(c.spsr[b], sys);
            }
            // TODO: usr/sys モードでの S=1, rd=15 は UNPREDICTABLE。今は CPSR 据え置き。
            c.regs[15] = if c.cpsr & FLAG_T != 0 {
                res & !1
            } else {
                res & !3
            };
            return Ok(());
        }
        let mut p = set_nz(c.cpsr, res);
        match arith {
            Some((cf, vf)) => p = set_flag(set_flag(p, FLAG_C, cf), FLAG_V, vf),
            None => p = set_flag(p, FLAG_C, sh_carry), // 論理系: C はシフタキャリー、V 不変
        }
        c.cpsr = p;
    }

    if !is_test {
        c.write_reg(rd, res);
    }
    Ok(())
}

// ---- ロード/ストア命令（ARM ARM A3.11、アドレッシングは A5.2/A5.3/A5.4）----

/// LDR/STR/LDRB/STRB（ワード・バイト転送）。
pub(super) fn exec_ldst<S: System>(c: &mut Cpu, sys: &mut S, word: u32) -> ExecResult {
    let pre = word & (1 << 24) != 0; // P: プリインデックス
    let up = word & (1 << 23) != 0; // U: オフセットを加算
    let byte_xfer = word & (1 << 22) != 0; // B: バイト転送
    let writeback = word & (1 << 21) != 0; // W
    let load = word & (1 << 20) != 0; // L
    let rn = (word >> 16) & 0xF;
    let rd = (word >> 12) & 0xF;

    let offset = if word & (1 << 25) != 0 {
        // スケーリング付きレジスタオフセット（シフト量は即値のみ）。
        // シフタキャリーはアドレス計算では使わない。
        let rm = c.read_reg(word & 0xF);
        shift_imm(rm, (word >> 5) & 3, (word >> 7) & 0x1F, c.flag_c()).0
    } else {
        word & 0xFFF
    };

    let base = c.read_reg(rn);
    let indexed = if up {
        base.wrapping_add(offset)
    } else {
        base.wrapping_sub(offset)
    };
    let addr = if pre { indexed } else { base };
    // TODO: P=0 かつ W=1 は LDRT/STRT（ユーザー権限でのアクセス）。
    // MMU の権限チェックを実装するまでは通常アクセスと同じ扱いにする。

    if load {
        let val = if byte_xfer {
            sys.read(addr, 1)?
        } else {
            // ARMv4 の非アラインワードロード: アラインしたワードを
            // アドレス下位 2 ビット×8 だけ右ローテートした値になる。
            // （CP15 の A ビットでアボートにもできるが、既定は回転動作）
            ror(sys.read(addr & !3, 4)?, 8 * (addr & 3))
        };
        // ライトバック → レジスタ書き込みの順。rd == rn のときは
        // ロード値が勝つ（ARM ARM の規定どおり）。
        if !pre || writeback {
            c.write_reg(rn, indexed);
        }
        c.write_reg(rd, val); // rd=15 なら分岐（v4T: Thumb 切替なし、bit1:0 無視）
    } else {
        // STR で rd=15 のとき格納される値は実装定義（PC+8 か PC+12）。
        // read_reg は PC+8 を返す。TODO: ARM920T の実際の値を確認する。
        let val = c.read_reg(rd);
        if byte_xfer {
            sys.write(addr, 1, val & 0xFF)?;
        } else {
            sys.write(addr & !3, 4, val)?;
        }
        if !pre || writeback {
            c.write_reg(rn, indexed);
        }
    }
    Ok(())
}

/// ハーフワード・符号付き転送（LDRH/STRH/LDRSB/LDRSH）。
/// エンコードはデータ処理空間の bit7=1,bit4=1 側（A5.3）。
pub(super) fn exec_ldst_misc<S: System>(c: &mut Cpu, sys: &mut S, word: u32) -> ExecResult {
    let pre = word & (1 << 24) != 0;
    let up = word & (1 << 23) != 0;
    let imm_form = word & (1 << 22) != 0; // 1: 8bit 即値オフセット / 0: レジスタ
    let writeback = word & (1 << 21) != 0;
    let load = word & (1 << 20) != 0;
    let rn = (word >> 16) & 0xF;
    let rd = (word >> 12) & 0xF;
    let sh = (word >> 5) & 3; // 01=H, 10=SB, 11=SH

    let offset = if imm_form {
        ((word >> 4) & 0xF0) | (word & 0xF)
    } else {
        c.read_reg(word & 0xF)
    };
    let base = c.read_reg(rn);
    let indexed = if up {
        base.wrapping_add(offset)
    } else {
        base.wrapping_sub(offset)
    };
    let addr = if pre { indexed } else { base };

    if !load {
        if sh != 1 {
            // L=0 の SB/SH は v5TE では LDRD/STRD。
            // TODO(v5TE): PXA27x 対応時に実装する。
            return Err(unimpl!("LDRD/STRD (ARMv5TE) not implemented"));
        }
        // STRH。非アラインは UNPREDICTABLE なのでアラインして扱う。
        sys.write(addr & !1, 2, c.read_reg(rd) & 0xFFFF)?;
        if !pre || writeback {
            c.write_reg(rn, indexed);
        }
        return Ok(());
    }

    let val = match sh {
        1 => sys.read(addr & !1, 2)?,                      // LDRH（ゼロ拡張）
        2 => sys.read(addr, 1)? as u8 as i8 as i32 as u32, // LDRSB（符号拡張）
        3 => sys.read(addr & !1, 2)? as u16 as i16 as i32 as u32, // LDRSH（符号拡張）
        // sh=00 はここに来ない（decode で乗算系に振り分け済み）
        _ => return Err(unimpl!("misc load/store with sh=00")),
    };
    if !pre || writeback {
        c.write_reg(rn, indexed);
    }
    c.write_reg(rd, val);
    Ok(())
}

impl Cpu {
    /// 現在モードに関係なく usr バンクのレジスタを読む（LDM(2)/STM(2) 用）。
    /// r0-r7 は全モード共有。r8-r12 は FIQ モードのときだけ退避領域
    /// （bank_r8_usr）側が usr の値。r13/r14 は usr/sys 以外のモードでは
    /// bank_r13/r14[BANK_USR] 側が usr の値。
    fn user_reg(&self, i: u32) -> u32 {
        let b = self.cur_bank();
        match i {
            8..=12 if b == BANK_FIQ => self.bank_r8_usr[(i - 8) as usize],
            13 if b != BANK_USR => self.bank_r13[BANK_USR],
            14 if b != BANK_USR => self.bank_r14[BANK_USR],
            _ => self.regs[i as usize],
        }
    }

    fn set_user_reg(&mut self, i: u32, v: u32) {
        let b = self.cur_bank();
        match i {
            8..=12 if b == BANK_FIQ => self.bank_r8_usr[(i - 8) as usize] = v,
            13 if b != BANK_USR => self.bank_r13[BANK_USR] = v,
            14 if b != BANK_USR => self.bank_r14[BANK_USR] = v,
            _ => self.regs[i as usize] = v,
        }
    }

    /// LDM で PC をロードする（S=1 なら例外復帰: CPSR ← SPSR を先に行い、
    /// 復帰後の state（ARM/Thumb）で PC をアラインする）。
    fn load_pc(&mut self, v: u32, s_bit: bool, sys: &mut impl System) {
        if !s_bit {
            self.write_reg(15, v);
            return;
        }
        let b = self.cur_bank();
        if b != BANK_USR {
            self.write_cpsr(self.spsr[b], sys);
        }
        self.regs[15] = if self.cpsr & FLAG_T != 0 {
            v & !1
        } else {
            v & !3
        };
    }
}

/// LDM/STM の開始アドレス。転送は常に「小さい番号のレジスタが小さいアドレス」。
/// 4 つのモード (IA/IB/DA/DB) は開始アドレスの違いに正規化できる。
fn ldm_start(base: u32, pre: bool, up: bool, n: u32) -> u32 {
    match (up, pre) {
        (true, false) => base,                                      // IA
        (true, true) => base.wrapping_add(4),                       // IB
        (false, false) => base.wrapping_sub(4 * n).wrapping_add(4), // DA
        (false, true) => base.wrapping_sub(4 * n),                  // DB
    }
}

/// LDM(2)/STM(2)（ユーザーバンク転送）本体。ライトバックはなし。
fn exec_ldm_stm_user<S: System>(c: &mut Cpu, sys: &mut S, word: u32, load: bool) -> ExecResult {
    let pre = word & (1 << 24) != 0;
    let up = word & (1 << 23) != 0;
    let rn = (word >> 16) & 0xF;
    let list = word & 0xFFFF;
    let mut addr = ldm_start(c.read_reg(rn), pre, up, list.count_ones());
    for i in 0..16 {
        if list & (1 << i) == 0 {
            continue;
        }
        if load {
            // LDM(2) に PC は含まれない（含む形は LDM(3) として処理済み）。
            let v = sys.read(addr & !3, 4)?;
            c.set_user_reg(i, v);
        } else {
            // PC は通常の STM と同じ PC+8
            let v = if i == 15 {
                c.read_reg(15)
            } else {
                c.user_reg(i)
            };
            sys.write(addr & !3, 4, v)?;
        }
        addr = addr.wrapping_add(4);
    }
    Ok(())
}

/// LDM/STM（A3.12、アドレッシングは A5.4）。
pub(super) fn exec_ldm_stm<S: System>(c: &mut Cpu, sys: &mut S, word: u32) -> ExecResult {
    let pre = word & (1 << 24) != 0;
    let up = word & (1 << 23) != 0;
    let s_bit = word & (1 << 22) != 0;
    let writeback = word & (1 << 21) != 0;
    let load = word & (1 << 20) != 0;
    let rn = (word >> 16) & 0xF;
    let list = word & 0xFFFF;
    let n = list.count_ones();
    if n == 0 {
        return Err(unimpl!("LDM/STM with empty register list (UNPREDICTABLE)"));
    }
    let has_pc = list & (1 << 15) != 0;

    if s_bit && !(load && has_pc) {
        // LDM(2)/STM(2): ユーザーバンク転送（現在モードに関係なく usr の
        // r8-r14 を読み書きする）。WinCE はスレッドのコンテキスト切替で使う。
        // ライトバックは UNPREDICTABLE（W=0 であるべき）なので止めて気づく。
        if writeback {
            return Err(unimpl!("LDM(2)/STM(2) with writeback (UNPREDICTABLE)"));
        }
        return exec_ldm_stm_user(c, sys, word, load);
    }

    let base = c.read_reg(rn);
    let start = ldm_start(base, pre, up, n);
    let new_base = if up {
        base.wrapping_add(4 * n)
    } else {
        base.wrapping_sub(4 * n)
    };

    let mut addr = start;
    if load {
        // ライトバックを先に行う。rn がリストに含まれる場合はロード値が上書きする
        // （ARM ARM: その場合のライトバック値は UNPREDICTABLE。ロード値優先に倒す）。
        if writeback {
            c.write_reg(rn, new_base);
        }
        if let Some(off) = sys.ram_run(start & !3, 4 * n, false) {
            // 1 ページ内・TLB ヒット: 直接読む（下の 1 ワードずつと同じ結果）。
            let (mut list, mut k) = (list, 0);
            while list != 0 {
                let i = list.trailing_zeros();
                list &= list - 1;
                let v = sys.ram_word(off + k);
                if i == 15 {
                    c.load_pc(v, s_bit, sys);
                } else {
                    c.regs[i as usize] = v;
                }
                k += 4;
            }
            return Ok(());
        }
        for i in 0..16 {
            if list & (1 << i) == 0 {
                continue;
            }
            let v = match sys.read(addr & !3, 4) {
                Ok(v) => v,
                Err(e) => {
                    // ARM9 系はアボート時にベースを命令実行前の値へ戻す
                    // （base restored モデル）。途中までロードしたレジスタは
                    // そのまま（実機でも上書きされ得る）。
                    if writeback && rn != 15 {
                        c.regs[rn as usize] = base;
                    }
                    return Err(e.into());
                }
            };
            if i == 15 {
                c.load_pc(v, s_bit, sys);
            } else {
                c.regs[i] = v;
            }
            addr = addr.wrapping_add(4);
        }
    } else {
        // STM: 先に全ストアしてからライトバック。これにより rn がリストに
        // 含まれていても格納されるのは変更前の値になる（リスト先頭が rn の
        // 場合の ARM ARM の規定と一致。それ以外の位置は UNPREDICTABLE）。
        if let Some(off) = sys.ram_run(start & !3, 4 * n, true) {
            // 1 ページ内・TLB ヒット・コードページでない: 直接書く。
            let (mut list, mut k) = (list, 0);
            while list != 0 {
                let i = list.trailing_zeros();
                list &= list - 1;
                let v = if i == 15 {
                    c.read_reg(15)
                } else {
                    c.regs[i as usize]
                };
                sys.set_ram_word(off + k, v);
                k += 4;
            }
            if writeback {
                c.write_reg(rn, new_base);
            }
            return Ok(());
        }
        for i in 0..16 {
            if list & (1 << i) == 0 {
                continue;
            }
            // PC は PC+8（実装定義。STR と同じ扱い）
            let v = if i == 15 { c.read_reg(15) } else { c.regs[i] };
            sys.write(addr & !3, 4, v)?;
            addr = addr.wrapping_add(4);
        }
        if writeback {
            c.write_reg(rn, new_base);
        }
    }
    Ok(())
}

// ---- 乗算命令（ARM ARM A3.5）と SWP/SWPB（A4.1.51/52）----
// どちらも bits[7:4]=1001 の「データ処理と同居する空間」にある（Figure A3-3）。
//
// フラグの扱い（ARMv4）:
//   - MUL/MLA の S=1: N/Z は結果から。C は UNPREDICTABLE、V は不変。
//     ここでは C/V とも不変にする（「変えない」が最も再現性が高い）。
//   - ロング乗算の S=1: N は bit63、Z は 64bit 結果全体。C/V は同上。

/// bits[7:4]=1001 空間のディスパッチ。bit24=0 が乗算、bit24=1 が SWP/SWPB。
pub(super) fn exec_mul_swp<S: System>(c: &mut Cpu, sys: &mut S, word: u32) -> ExecResult {
    if word & (1 << 24) != 0 {
        return exec_swp(c, sys, word);
    }
    if word & (1 << 23) != 0 {
        return exec_mul_long(c, word);
    }
    exec_mul(c, word)
}

/// MUL（A=0）/ MLA（A=1）。Rd = Rm*Rs (+ Rn)。
/// オペランドに r15 を使うのは UNPREDICTABLE（read_reg の PC+8 のまま扱う）。
fn exec_mul(c: &mut Cpu, word: u32) -> ExecResult {
    let rd = (word >> 16) & 0xF;
    let rn = (word >> 12) & 0xF;
    let mut res = c
        .read_reg(word & 0xF)
        .wrapping_mul(c.read_reg((word >> 8) & 0xF));
    if word & (1 << 21) != 0 {
        res = res.wrapping_add(c.read_reg(rn)); // A: アキュムレート
    }
    c.write_reg(rd, res);
    if word & (1 << 20) != 0 {
        c.cpsr = set_nz(c.cpsr, res); // S
    }
    Ok(())
}

/// UMULL/UMLAL/SMULL/SMLAL。{RdHi,RdLo} = Rm*Rs (+ {RdHi,RdLo})。
/// bit22: 1=符号付き、bit21: 1=アキュムレート。
fn exec_mul_long(c: &mut Cpu, word: u32) -> ExecResult {
    let rd_hi = (word >> 16) & 0xF;
    let rd_lo = (word >> 12) & 0xF;
    let rm = c.read_reg(word & 0xF);
    let rs = c.read_reg((word >> 8) & 0xF);
    let mut res = if word & (1 << 22) != 0 {
        (rm as i32 as i64).wrapping_mul(rs as i32 as i64) as u64 // 符号付き
    } else {
        (rm as u64).wrapping_mul(rs as u64)
    };
    if word & (1 << 21) != 0 {
        // アキュムレート
        res = res.wrapping_add((c.read_reg(rd_hi) as u64) << 32 | c.read_reg(rd_lo) as u64);
    }
    c.write_reg(rd_lo, res as u32);
    c.write_reg(rd_hi, (res >> 32) as u32);
    if word & (1 << 20) != 0 {
        c.cpsr = set_flag(
            set_flag(c.cpsr, FLAG_N, res & (1 << 63) != 0),
            FLAG_Z,
            res == 0,
        );
    }
    Ok(())
}

/// SWP/SWPB（アトミック交換）: temp=Mem[Rn]; Mem[Rn]=Rm; Rd=temp。
/// シングルコア・逐次実行なのでアトミック性は自動的に満たされる。
fn exec_swp<S: System>(c: &mut Cpu, sys: &mut S, word: u32) -> ExecResult {
    if word & (1 << 23) != 0 || word & (3 << 20) != 0 {
        // bit24=1 空間で SWP 以外のビットパターンは v4 では未定義。
        return Err(unimpl!("undefined encoding in swap space"));
    }
    let addr = c.read_reg((word >> 16) & 0xF);
    let rd = (word >> 12) & 0xF;
    let rm_val = c.read_reg(word & 0xF);
    if word & (1 << 22) != 0 {
        // B: バイト
        let old = sys.read(addr, 1)?;
        sys.write(addr, 1, rm_val & 0xFF)?;
        c.write_reg(rd, old);
        return Ok(());
    }
    // ワード: 非アラインアドレスのロード値は LDR と同じ回転動作。
    let old = sys.read(addr & !3, 4)?;
    sys.write(addr & !3, 4, rm_val)?;
    c.write_reg(rd, ror(old, 8 * (addr & 3)));
    Ok(())
}

// ---- MRS/MSR（ステータスレジスタ転送、A4.1.38/39）----

/// MRS Rd, CPSR/SPSR。
pub(super) fn exec_mrs<S: System>(c: &mut Cpu, _sys: &mut S, word: u32) -> ExecResult {
    let rd = (word >> 12) & 0xF;
    if rd == 15 {
        return Err(unimpl!("MRS with Rd=PC (UNPREDICTABLE)"));
    }
    if word & (1 << 22) != 0 {
        // R: SPSR
        let b = c.cur_bank();
        if b == BANK_USR {
            // usr/sys に SPSR はない（UNPREDICTABLE）。0 を返すより止めて気づけるように。
            return Err(unimpl!("MRS SPSR in usr/sys mode (no SPSR)"));
        }
        c.regs[rd as usize] = c.spsr[b];
        return Ok(());
    }
    c.regs[rd as usize] = c.cpsr;
    Ok(())
}

/// MSR CPSR/SPSR_<fields>, Rm または #imm。
/// フィールドマスク（bits 19:16 = f,s,x,c）で書き込むバイトを選ぶ。
pub(super) fn exec_msr<S: System>(c: &mut Cpu, sys: &mut S, word: u32) -> ExecResult {
    let val = if word & (1 << 25) != 0 {
        ror(word & 0xFF, ((word >> 8) & 0xF) * 2) // 即値形式
    } else {
        c.read_reg(word & 0xF)
    };
    let mut mask = 0u32;
    if word & (1 << 16) != 0 {
        mask |= 0x000000FF; // c: 制御ビット（モード・I/F/T）
    }
    if word & (1 << 17) != 0 {
        mask |= 0x0000FF00; // x
    }
    if word & (1 << 18) != 0 {
        mask |= 0x00FF0000; // s
    }
    if word & (1 << 19) != 0 {
        mask |= 0xFF000000; // f: フラグ
    }

    if word & (1 << 22) != 0 {
        // R: SPSR へ
        let b = c.cur_bank();
        if b == BANK_USR {
            return Err(unimpl!("MSR SPSR in usr/sys mode (no SPSR)"));
        }
        c.spsr[b] = (c.spsr[b] & !mask) | (val & mask);
        return Ok(());
    }

    // CPSR へ。ユーザーモードでは制御ビットは書けない（フラグのみ）。
    if c.cpsr & 0x1F == MODE_USR {
        mask &= 0xFF000000;
    }
    let new_psr = (c.cpsr & !mask) | (val & mask);
    if bank_index(new_psr & 0x1F).is_none() {
        // 存在しないモード番号への切替は UNPREDICTABLE。黙って壊れるより停止。
        return Err(unimpl!(
            "MSR writes an invalid mode to CPSR (UNPREDICTABLE)"
        ));
    }
    c.write_cpsr(new_psr, sys);
    Ok(())
}

// ---- 分岐命令と SWI、コプロセッサレジスタ転送 ----

/// B/BL（A3.3）。24bit 符号付きオフセット×4 を PC+8 に加える。
pub(super) fn exec_branch<S: System>(c: &mut Cpu, sys: &mut S, word: u32) -> ExecResult {
    // 符号拡張して ×4（<<8 で上位に詰め、>>6 = 算術>>8 と <<2）
    let offset = ((word << 8) as i32) >> 6;
    if word & (1 << 24) != 0 {
        c.regs[14] = c.regs[15]; // L: リンク。regs[15] は今 PC+4 = 次命令 = 復帰先
    }
    c.regs[15] = c.regs[15].wrapping_add(4).wrapping_add(offset as u32); // PC+8 基準
    if word & 0x01FFFFFF == 0x00FFFFFC {
        // L=0 で 2 命令前へ戻る分岐: 3 命令のポーリングループの可能性
        // （idle.rs）。ここでは印を付けて run を打ち切るだけで、判定は実行ループに
        // 任せる（毎命令の判定を省くため）。
        c.spin_hint = true;
        sys.run_ctl().budget = 0;
    }
    Ok(())
}

/// BX Rm（A3.3）。bit0 で ARM/Thumb を切り替える（v4T の相互運用）。
pub(super) fn exec_bx<S: System>(c: &mut Cpu, _sys: &mut S, word: u32) -> ExecResult {
    let target = c.read_reg(word & 0xF);
    if target & 1 != 0 {
        c.cpsr |= FLAG_T;
        c.regs[15] = target & !1;
    } else {
        c.cpsr &= !FLAG_T;
        c.regs[15] = target & !3;
    }
    Ok(())
}

/// ソフトウェア割り込み。SVC モードでベクタ 0x08 へ。復帰先は SWI の次の命令。
pub(super) fn exec_swi<S: System>(c: &mut Cpu, sys: &mut S) -> ExecResult {
    c.enter_exception(VEC_SWI, MODE_SVC, c.regs[15], sys); // regs[15] = PC+4
    Ok(())
}

/// MCR/MRC（コプロセッサレジスタ転送、A4.1.32/40）。CP15（MMU・キャッシュ制御）
/// だけを通し、それ以外のコプロセッサは未定義例外にする。
pub(super) fn exec_mcr_mrc<S: System>(c: &mut Cpu, sys: &mut S, word: u32) -> ExecResult {
    let cp_num = (word >> 8) & 0xF;
    if cp_num != 15 {
        // ARM920T に CP15 以外のコプロセッサはなく、実機でも未定義命令例外
        // になる。WinCE は FPU 検出のため意図的に p10（VFP）等を叩く。
        return Err(Exec::Undef(&Undef {
            reason: "no such coprocessor (undefined exception on real HW)",
            arch: true,
        }));
    }
    let opc1 = ((word >> 21) & 7) as u8;
    let crn = ((word >> 16) & 0xF) as u8;
    let rd = (word >> 12) & 0xF;
    let opc2 = ((word >> 5) & 7) as u8;
    let crm = (word & 0xF) as u8;

    if word & (1 << 20) != 0 {
        // MRC: コプロセッサ → レジスタ
        let v = sys.cp15_read(opc1, crn, crm, opc2);
        if rd == 15 {
            // MRC ... , r15 はフラグ N/Z/C/V に上位 4 ビットを書く特殊形。
            let nzcv = FLAG_N | FLAG_Z | FLAG_C | FLAG_V;
            c.cpsr = (c.cpsr & !nzcv) | (v & nzcv);
            return Ok(());
        }
        c.regs[rd as usize] = v;
        return Ok(());
    }
    // MCR: レジスタ → コプロセッサ
    sys.cp15_write(opc1, crn, crm, opc2, c.read_reg(rd));
    Ok(())
}
