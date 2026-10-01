//! Thumb 命令セット（ARMv4T。ARM ARM DDI 0100 Chapter A6/A7）。
//!
//! Thumb 命令は 16 ビット固定長で、実行中の regs[15] は「命令アドレス+2」、
//! オペランドとして読む PC は「命令アドレス+4」（ARM と同じ 2 段先読みの
//! 見え方）。フラグ計算は対応する ARM 命令と同一なので、ARM 側のヘルパー
//! （add_with_carry、shift_imm/shift_reg）を流用する。

use super::exec_arm::{add_with_carry, ror, shift_imm, shift_reg};
use super::*;

impl Cpu {
    /// Thumb 命令を 1 個実行する。呼び出し時点で regs[15] は命令アドレス
    /// （実行中は +2 にする。ARM と同じ流儀）。
    pub(crate) fn step_thumb<S: System>(&mut self, sys: &mut S) -> Result<(), StopError> {
        let pc = self.regs[15];
        // フェッチ。Thumb はハーフワード単位。MMU 有効化直後のフェッチ猶予は
        // ARM のブートストラップ専用なので、ここでは通常のデータ読み出しでよい
        // （プリフェッチアボートはアボートとして伝わる）。
        let hw = match sys.read(pc & !1, 2) {
            Ok(v) => v,
            Err(MemError::Abort(_)) => {
                self.enter_exception(VEC_PABT, MODE_ABT, pc.wrapping_add(4), sys);
                return Ok(());
            }
            Err(MemError::Bus(b)) => return Err(StopError::Bus(b)),
        };
        self.regs[15] = pc.wrapping_add(2);
        if let Err(e) = self.exec_thumb(sys, hw)
            && let Err(stop) = self.deliver_exec_error(e, pc, hw, 2, sys)
        {
            self.regs[15] = pc;
            return Err(stop);
        }
        Ok(())
    }

    /// 16 ビット命令をデコードして実行する（Figure A6-1 の上位ビットによる分類）。
    fn exec_thumb<S: System>(&mut self, sys: &mut S, hw: u32) -> ExecResult {
        match hw >> 13 {
            0 => self.thumb_shift_add_sub(hw), // 000: シフト即値 / ADD/SUB レジスタ・3bit 即値
            1 => self.thumb_imm8(hw),          // 001: MOV/CMP/ADD/SUB 8bit 即値
            2 => self.thumb_group010(sys, hw), // 010: ALU / Hi レジスタ・BX / PC 相対 LDR / レジスタオフセット LDR/STR
            3 => self.thumb_ldst_imm(sys, hw), // 011: LDR/STR 即値オフセット（ワード/バイト）
            4 => self.thumb_group100(sys, hw), // 100: LDRH/STRH 即値 / SP 相対 LDR/STR
            5 => self.thumb_group101(sys, hw), // 101: ADD PC/SP 相対 / SP 調整 / PUSH/POP
            6 => self.thumb_group110(sys, hw), // 110: LDMIA/STMIA / 条件分岐 / SWI
            _ => self.thumb_branch_long(hw),   // 111: B / BL
        }
    }

    /// オペランドとしての PC（命令アドレス+4）。
    #[inline(always)]
    fn thumb_read_pc(&self) -> u32 {
        self.regs[15].wrapping_add(2)
    }

    /// オペランド読み出し（r15 は +4 の見え方）。
    fn thumb_reg(&self, n: u32) -> u32 {
        if n == 15 {
            self.thumb_read_pc()
        } else {
            self.regs[n as usize]
        }
    }

    fn set_nz_flags(&mut self, v: u32) {
        self.cpsr = set_nz(self.cpsr, v);
    }

    /// 加減算の結果からフラグ 4 つを更新する。
    fn set_nzcv(&mut self, (v, cf, vf): (u32, bool, bool)) -> u32 {
        self.cpsr = set_flag(set_flag(set_nz(self.cpsr, v), FLAG_C, cf), FLAG_V, vf);
        v
    }

    fn set_c(&mut self, c: bool) {
        self.cpsr = set_flag(self.cpsr, FLAG_C, c);
    }

    /// Format 1（シフト即値）と Format 2（ADD/SUB）。
    fn thumb_shift_add_sub(&mut self, hw: u32) -> ExecResult {
        let rd = (hw & 7) as usize;
        let rs = ((hw >> 3) & 7) as usize;
        let op = (hw >> 11) & 3;
        if op != 3 {
            // LSL/LSR/ASR Rd, Rs, #imm5
            let (res, carry) = shift_imm(self.regs[rs], op, (hw >> 6) & 0x1F, self.flag_c());
            self.regs[rd] = res;
            self.set_nz_flags(res);
            self.set_c(carry);
            return Ok(());
        }
        // ADD/SUB Rd, Rs, Rn または #imm3
        let opnd = if hw & (1 << 10) != 0 {
            (hw >> 6) & 7
        } else {
            self.regs[((hw >> 6) & 7) as usize]
        };
        let r = if hw & (1 << 9) != 0 {
            add_with_carry(self.regs[rs], !opnd, 1) // SUB
        } else {
            add_with_carry(self.regs[rs], opnd, 0) // ADD
        };
        self.regs[rd] = self.set_nzcv(r);
        Ok(())
    }

    /// Format 3: MOV/CMP/ADD/SUB Rd, #imm8。
    fn thumb_imm8(&mut self, hw: u32) -> ExecResult {
        let rd = ((hw >> 8) & 7) as usize;
        let imm = hw & 0xFF;
        match (hw >> 11) & 3 {
            0 => {
                // MOV
                self.regs[rd] = imm;
                self.set_nz_flags(imm);
            }
            1 => {
                // CMP
                self.set_nzcv(add_with_carry(self.regs[rd], !imm, 1));
            }
            2 => self.regs[rd] = self.set_nzcv(add_with_carry(self.regs[rd], imm, 0)), // ADD
            _ => self.regs[rd] = self.set_nzcv(add_with_carry(self.regs[rd], !imm, 1)), // SUB
        }
        Ok(())
    }

    fn thumb_group010<S: System>(&mut self, sys: &mut S, hw: u32) -> ExecResult {
        if hw >> 10 == 0x10 {
            return self.thumb_alu(hw); // 010000: Format 4 ALU
        }
        if hw >> 10 == 0x11 {
            return self.thumb_hi_reg_bx(hw); // 010001: Format 5 Hi レジスタ・BX
        }
        if hw >> 11 == 0x9 {
            // 01001: Format 6 LDR Rd, [PC, #imm8*4]
            let rd = ((hw >> 8) & 7) as usize;
            let addr = (self.thumb_read_pc() & !3).wrapping_add((hw & 0xFF) * 4);
            self.regs[rd] = sys.read(addr, 4)?;
            return Ok(());
        }
        self.thumb_ldst_reg(sys, hw) // 0101: Format 7/8 レジスタオフセット LDR/STR
    }

    /// Format 4（低レジスタ同士の演算）。
    fn thumb_alu(&mut self, hw: u32) -> ExecResult {
        let rd = (hw & 7) as usize;
        let rs = ((hw >> 3) & 7) as usize;
        let (a, b) = (self.regs[rd], self.regs[rs]);
        let cin = self.flag_c() as u32;
        let c_in = self.flag_c();
        match (hw >> 6) & 0xF {
            0x0 => self.logic(rd, a & b),                                     // AND
            0x1 => self.logic(rd, a ^ b),                                     // EOR
            0x2 => self.shift_op(rd, shift_reg(a, 0, b & 0xFF, c_in)),        // LSL
            0x3 => self.shift_op(rd, shift_reg(a, 1, b & 0xFF, c_in)),        // LSR
            0x4 => self.shift_op(rd, shift_reg(a, 2, b & 0xFF, c_in)),        // ASR
            0x5 => self.regs[rd] = self.set_nzcv(add_with_carry(a, b, cin)),  // ADC
            0x6 => self.regs[rd] = self.set_nzcv(add_with_carry(a, !b, cin)), // SBC
            0x7 => self.shift_op(rd, shift_reg(a, 3, b & 0xFF, c_in)),        // ROR
            0x8 => self.set_nz_flags(a & b),                                  // TST
            0x9 => self.regs[rd] = self.set_nzcv(add_with_carry(0, !b, 1)), // NEG (RSB Rd, Rs, #0)
            0xA => {
                self.set_nzcv(add_with_carry(a, !b, 1)); // CMP
            }
            0xB => {
                self.set_nzcv(add_with_carry(a, b, 0)); // CMN
            }
            0xC => self.logic(rd, a | b),             // ORR
            0xD => self.logic(rd, a.wrapping_mul(b)), // MUL（C は ARM 同様に不変とする）
            0xE => self.logic(rd, a & !b),            // BIC
            _ => self.logic(rd, !b),                  // MVN
        }
        Ok(())
    }

    /// 結果を書いて N/Z を更新する。
    fn logic(&mut self, rd: usize, v: u32) {
        self.regs[rd] = v;
        self.set_nz_flags(v);
    }

    /// シフト結果を書いて N/Z/C を更新する。
    fn shift_op(&mut self, rd: usize, (res, carry): (u32, bool)) {
        self.logic(rd, res);
        self.set_c(carry);
    }

    /// Format 5: ADD/CMP/MOV（高レジスタ可、フラグ変化は CMP のみ）と BX。
    fn thumb_hi_reg_bx(&mut self, hw: u32) -> ExecResult {
        let rd = (hw & 7) | ((hw >> 4) & 8); // H1:Rd
        let rs = (hw >> 3) & 0xF; // H2:Rs
        let v = self.thumb_reg(rs);
        match (hw >> 8) & 3 {
            0 => {
                // ADD Rd, Rs
                let res = self.thumb_reg(rd).wrapping_add(v);
                if rd == 15 {
                    self.regs[15] = res & !1;
                } else {
                    self.regs[rd as usize] = res;
                }
            }
            1 => {
                // CMP Rd, Rs
                self.set_nzcv(add_with_carry(self.thumb_reg(rd), !v, 1));
            }
            2 => {
                // MOV Rd, Rs
                if rd == 15 {
                    self.regs[15] = v & !1;
                } else {
                    self.regs[rd as usize] = v;
                }
            }
            _ => {
                // BX Rs（bit0 で ARM/Thumb 切替）
                if v & 1 != 0 {
                    self.cpsr |= FLAG_T;
                    self.regs[15] = v & !1;
                } else {
                    self.cpsr &= !FLAG_T;
                    self.regs[15] = v & !3;
                }
            }
        }
        Ok(())
    }

    /// Format 7/8: レジスタオフセットのロード/ストア。
    fn thumb_ldst_reg<S: System>(&mut self, sys: &mut S, hw: u32) -> ExecResult {
        let rd = (hw & 7) as usize;
        let addr =
            self.regs[((hw >> 3) & 7) as usize].wrapping_add(self.regs[((hw >> 6) & 7) as usize]);
        match (hw >> 9) & 7 {
            0 => sys.write(addr & !3, 4, self.regs[rd])?, // STR
            1 => sys.write(addr & !1, 2, self.regs[rd] & 0xFFFF)?, // STRH
            2 => sys.write(addr, 1, self.regs[rd] & 0xFF)?, // STRB
            3 => self.regs[rd] = sys.read(addr, 1)? as u8 as i8 as i32 as u32, // LDRSB
            4 => self.regs[rd] = ror(sys.read(addr & !3, 4)?, 8 * (addr & 3)), // LDR（非アラインは ARM と同じ回転）
            5 => self.regs[rd] = sys.read(addr & !1, 2)?,                      // LDRH
            6 => self.regs[rd] = sys.read(addr, 1)?,                           // LDRB
            _ => self.regs[rd] = sys.read(addr & !1, 2)? as u16 as i16 as i32 as u32, // LDRSH
        }
        Ok(())
    }

    /// Format 9: LDR/STR Rd, [Rs, #imm5]（ワードは imm*4）。
    fn thumb_ldst_imm<S: System>(&mut self, sys: &mut S, hw: u32) -> ExecResult {
        let rd = (hw & 7) as usize;
        let rs = ((hw >> 3) & 7) as usize;
        let imm = (hw >> 6) & 0x1F;
        let load = hw & (1 << 11) != 0;
        if hw & (1 << 12) != 0 {
            // バイト
            let addr = self.regs[rs].wrapping_add(imm);
            if load {
                self.regs[rd] = sys.read(addr, 1)?;
                return Ok(());
            }
            return Ok(sys.write(addr, 1, self.regs[rd] & 0xFF)?);
        }
        let addr = self.regs[rs].wrapping_add(imm * 4);
        if load {
            self.regs[rd] = ror(sys.read(addr & !3, 4)?, 8 * (addr & 3));
            return Ok(());
        }
        Ok(sys.write(addr & !3, 4, self.regs[rd])?)
    }

    fn thumb_group100<S: System>(&mut self, sys: &mut S, hw: u32) -> ExecResult {
        if hw & (1 << 12) == 0 {
            // Format 10: LDRH/STRH Rd, [Rs, #imm5*2]
            let rd = (hw & 7) as usize;
            let addr = self.regs[((hw >> 3) & 7) as usize].wrapping_add(((hw >> 6) & 0x1F) * 2);
            if hw & (1 << 11) != 0 {
                self.regs[rd] = sys.read(addr & !1, 2)?;
                return Ok(());
            }
            return Ok(sys.write(addr & !1, 2, self.regs[rd] & 0xFFFF)?);
        }
        // Format 11: LDR/STR Rd, [SP, #imm8*4]
        let rd = ((hw >> 8) & 7) as usize;
        let addr = self.regs[13].wrapping_add((hw & 0xFF) * 4);
        if hw & (1 << 11) != 0 {
            self.regs[rd] = ror(sys.read(addr & !3, 4)?, 8 * (addr & 3));
            return Ok(());
        }
        Ok(sys.write(addr & !3, 4, self.regs[rd])?)
    }

    fn thumb_group101<S: System>(&mut self, sys: &mut S, hw: u32) -> ExecResult {
        if hw & (1 << 12) == 0 {
            // Format 12: ADD Rd, PC/SP, #imm8*4
            let rd = ((hw >> 8) & 7) as usize;
            let imm = (hw & 0xFF) * 4;
            self.regs[rd] = if hw & (1 << 11) != 0 {
                self.regs[13].wrapping_add(imm)
            } else {
                (self.thumb_read_pc() & !3).wrapping_add(imm)
            };
            return Ok(());
        }
        if (hw >> 8) & 0xF == 0 {
            // Format 13: ADD SP, #±imm7*4
            let imm = (hw & 0x7F) * 4;
            self.regs[13] = if hw & (1 << 7) != 0 {
                self.regs[13].wrapping_sub(imm)
            } else {
                self.regs[13].wrapping_add(imm)
            };
            return Ok(());
        }
        if (hw >> 9) & 3 == 2 {
            return self.thumb_push_pop(sys, hw); // Format 14: PUSH/POP
        }
        Err(unimpl!("unallocated Thumb encoding (misc 1011 space)"))
    }

    /// Format 14。PUSH = STMDB SP!、POP = LDMIA SP!。
    /// v4T の POP {..pc} は状態切替なし（bit0 無視。interworking POP は v5）。
    fn thumb_push_pop<S: System>(&mut self, sys: &mut S, hw: u32) -> ExecResult {
        let list = hw & 0xFF;
        let pop = hw & (1 << 11) != 0;
        let r = hw & (1 << 8) != 0; // PUSH: LR / POP: PC を追加
        let n = list.count_ones() + r as u32;
        if n == 0 {
            return Err(unimpl!("PUSH/POP with empty list (UNPREDICTABLE)"));
        }
        let sp = self.regs[13];
        if pop {
            let mut addr = sp;
            // ライトバックを先に行い、アボート時は復元（ARM 側 LDM と同じ方針）。
            self.regs[13] = sp.wrapping_add(4 * n);
            for i in 0..8 {
                if list & (1 << i) == 0 {
                    continue;
                }
                match sys.read(addr & !3, 4) {
                    Ok(v) => self.regs[i] = v,
                    Err(e) => {
                        self.regs[13] = sp;
                        return Err(e.into());
                    }
                }
                addr = addr.wrapping_add(4);
            }
            if r {
                match sys.read(addr & !3, 4) {
                    Ok(v) => self.regs[15] = v & !1,
                    Err(e) => {
                        self.regs[13] = sp;
                        return Err(e.into());
                    }
                }
            }
            return Ok(());
        }
        let start = sp.wrapping_sub(4 * n);
        let mut addr = start;
        for i in 0..8 {
            if list & (1 << i) == 0 {
                continue;
            }
            sys.write(addr & !3, 4, self.regs[i])?;
            addr = addr.wrapping_add(4);
        }
        if r {
            sys.write(addr & !3, 4, self.regs[14])?;
        }
        self.regs[13] = start;
        Ok(())
    }

    fn thumb_group110<S: System>(&mut self, sys: &mut S, hw: u32) -> ExecResult {
        if hw & (1 << 12) == 0 {
            return self.thumb_ldm_stm(sys, hw); // Format 15: LDMIA/STMIA Rn!, {rlist}
        }
        let cond = (hw >> 8) & 0xF;
        match cond {
            0xF => {
                // Format 17: SWI
                // Thumb の SWI 復帰先は次の命令（regs[15] = PC+2 が既にそれ）。
                self.enter_exception(VEC_SWI, MODE_SVC, self.regs[15], sys);
                return Ok(());
            }
            // 1110 は未定義（ARM ARM A6.3.1）
            0xE => {
                return Err(Exec::Undef(&Undef {
                    reason: "Thumb B with cond=1110 (undefined)",
                    arch: true,
                }));
            }
            _ => {}
        }
        // Format 16: 条件分岐 ±imm8*2
        if cond_passed(self.cpsr, cond) {
            let off = (hw & 0xFF) as u8 as i8 as i32 * 2;
            self.regs[15] = self.thumb_read_pc().wrapping_add(off as u32);
        }
        Ok(())
    }

    /// Format 15: LDMIA/STMIA Rn!, {r0-r7}。
    fn thumb_ldm_stm<S: System>(&mut self, sys: &mut S, hw: u32) -> ExecResult {
        let rn = ((hw >> 8) & 7) as usize;
        let list = hw & 0xFF;
        if list == 0 {
            return Err(unimpl!("Thumb LDM/STM with empty list (UNPREDICTABLE)"));
        }
        let load = hw & (1 << 11) != 0;
        let base = self.regs[rn];
        let n = list.count_ones();
        let mut addr = base;
        if load {
            // ライトバック先行（rn がリストにあればロード値が勝つ）。アボートで復元。
            self.regs[rn] = base.wrapping_add(4 * n);
            for i in 0..8 {
                if list & (1 << i) == 0 {
                    continue;
                }
                match sys.read(addr & !3, 4) {
                    Ok(v) => self.regs[i] = v,
                    Err(e) => {
                        self.regs[rn] = base;
                        return Err(e.into());
                    }
                }
                addr = addr.wrapping_add(4);
            }
            return Ok(());
        }
        for i in 0..8 {
            if list & (1 << i) == 0 {
                continue;
            }
            sys.write(addr & !3, 4, self.regs[i])?;
            addr = addr.wrapping_add(4);
        }
        self.regs[rn] = base.wrapping_add(4 * n);
        Ok(())
    }

    /// Format 18（B）と Format 19（BL、2 ハーフワード）。
    fn thumb_branch_long(&mut self, hw: u32) -> ExecResult {
        match (hw >> 11) & 3 {
            0 => {
                // B ±imm11*2（符号拡張して ×2）
                let off = ((hw << 21) as i32) >> 20;
                self.regs[15] = self.thumb_read_pc().wrapping_add(off as u32);
                Ok(())
            }
            2 => {
                // BL プレフィックス: LR = PC+4 + signext(imm11)<<12
                let off = ((hw << 21) as i32) >> 9;
                self.regs[14] = self.thumb_read_pc().wrapping_add(off as u32);
                Ok(())
            }
            3 => {
                // BL サフィックス: 分岐して LR = 次命令 | 1
                let next = self.regs[14].wrapping_add((hw & 0x7FF) << 1);
                self.regs[14] = self.regs[15] | 1; // regs[15] は今 PC+2 = 次命令
                self.regs[15] = next & !1;
                Ok(())
            }
            // 01: BLX サフィックス（ARMv5）
            // TODO(v5TE): PXA27x 対応時に BLX を実装する。
            _ => Err(unimpl!("BLX suffix (ARMv5) not implemented")),
        }
    }
}
