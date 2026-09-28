package arm

import "fmt"

// Thumb 命令セット（ARMv4T。ARM ARM DDI 0100 Chapter A6/A7）。
//
// Thumb 命令は 16 ビット固定長で、実行中の regs[15] は「命令アドレス+2」、
// オペランドとして読む PC は「命令アドレス+4」（ARM と同じ 2 段先読みの
// 見え方）。フラグ計算は対応する ARM 命令と同一なので、ARM 側のヘルパー
// （addWithCarry、shiftImm/shiftReg）を流用する。

// stepThumb は Thumb 命令を 1 個実行する。呼び出し時点で regs[15] は
// 命令アドレス（実行中は +2 にする。ARM と同じ流儀）。
func (c *Core) stepThumb() error {
	pc := c.regs[15]
	// フェッチ。Thumb はハーフワード単位。MMU 有効化直後のフェッチ猶予
	// （InstructionFetcher）は ARM のブートストラップ専用なので、ここでは
	// 通常の Read16 でよい（プリフェッチアボートは AbortError で伝わる）。
	hw, err := c.mem.Read16(pc &^ 1)
	if err != nil {
		if isAbort(err) {
			c.enterException(VecPabt, ModeAbt, pc+4)
			return nil
		}
		return fmt.Errorf("instruction fetch (Thumb) at PC=%08X: %w", pc, err)
	}

	c.regs[15] = pc + 2

	if err := c.execThumb(uint32(hw)); err != nil {
		if derr := c.deliverExecError(err, pc, 2); derr != nil {
			if ue, ok := derr.(*UndefinedError); ok {
				ue.PC = pc
				ue.Word = uint32(hw)
			}
			c.regs[15] = pc
			return derr
		}
	}
	return nil
}

// execThumb は 16 ビット命令をデコードして実行する（Figure A6-1 の
// 上位ビットによる分類）。
func (c *Core) execThumb(hw uint32) error {
	switch hw >> 13 {
	case 0: // 000: シフト即値 / ADD/SUB レジスタ・3bit 即値
		return c.thumbShiftAddSub(hw)
	case 1: // 001: MOV/CMP/ADD/SUB 8bit 即値
		return c.thumbImm8(hw)
	case 2: // 010: ALU / Hi レジスタ・BX / PC 相対 LDR / レジスタオフセット LDR/STR
		return c.thumbGroup010(hw)
	case 3: // 011: LDR/STR 即値オフセット（ワード/バイト）
		return c.thumbLdstImm(hw)
	case 4: // 100: LDRH/STRH 即値 / SP 相対 LDR/STR
		return c.thumbGroup100(hw)
	case 5: // 101: ADD PC/SP 相対 / SP 調整 / PUSH/POP
		return c.thumbGroup101(hw)
	case 6: // 110: LDMIA/STMIA / 条件分岐 / SWI
		return c.thumbGroup110(hw)
	default: // 111: B / BL
		return c.thumbBranchLong(hw)
	}
}

// thumbReadPC はオペランドとしての PC（命令アドレス+4）。
func (c *Core) thumbReadPC() uint32 { return c.regs[15] + 2 }

// thumbReg はオペランド読み出し（r15 は +4 の見え方）。
func (c *Core) thumbReg(n uint32) uint32 {
	if n == 15 {
		return c.thumbReadPC()
	}
	return c.regs[n]
}

// setNZ は結果から N/Z を更新する。
func (c *Core) setNZ(v uint32) { c.cpsr = c.cpsr.SetNZ(v) }

// setNZCV は加減算の結果からフラグ 4 つを更新する。
func (c *Core) setNZCV(v uint32, cf, vf bool) {
	c.cpsr = c.cpsr.SetNZ(v).set(FlagC, cf).set(FlagV, vf)
}

// thumbShiftAddSub は Format 1（シフト即値）と Format 2（ADD/SUB）。
func (c *Core) thumbShiftAddSub(hw uint32) error {
	rd := hw & 7
	rs := (hw >> 3) & 7
	op := (hw >> 11) & 3
	if op != 3 { // LSL/LSR/ASR Rd, Rs, #imm5
		res, carry := shiftImm(c.regs[rs], op, (hw>>6)&0x1F, c.cpsr.C())
		c.regs[rd] = res
		c.setNZ(res)
		c.cpsr = c.cpsr.set(FlagC, carry)
		return nil
	}
	// ADD/SUB Rd, Rs, Rn または #imm3
	var opnd uint32
	if hw&(1<<10) != 0 {
		opnd = (hw >> 6) & 7 // 即値
	} else {
		opnd = c.regs[(hw>>6)&7]
	}
	var res uint32
	var cf, vf bool
	if hw&(1<<9) != 0 {
		res, cf, vf = addWithCarry(c.regs[rs], ^opnd, 1) // SUB
	} else {
		res, cf, vf = addWithCarry(c.regs[rs], opnd, 0) // ADD
	}
	c.regs[rd] = res
	c.setNZCV(res, cf, vf)
	return nil
}

// thumbImm8 は Format 3: MOV/CMP/ADD/SUB Rd, #imm8。
func (c *Core) thumbImm8(hw uint32) error {
	rd := (hw >> 8) & 7
	imm := hw & 0xFF
	switch (hw >> 11) & 3 {
	case 0: // MOV
		c.regs[rd] = imm
		c.setNZ(imm)
	case 1: // CMP
		res, cf, vf := addWithCarry(c.regs[rd], ^imm, 1)
		c.setNZCV(res, cf, vf)
	case 2: // ADD
		res, cf, vf := addWithCarry(c.regs[rd], imm, 0)
		c.regs[rd] = res
		c.setNZCV(res, cf, vf)
	default: // SUB
		res, cf, vf := addWithCarry(c.regs[rd], ^imm, 1)
		c.regs[rd] = res
		c.setNZCV(res, cf, vf)
	}
	return nil
}

func (c *Core) thumbGroup010(hw uint32) error {
	switch {
	case hw>>10 == 0x10: // 010000: Format 4 ALU
		return c.thumbALU(hw)
	case hw>>10 == 0x11: // 010001: Format 5 Hi レジスタ・BX
		return c.thumbHiRegBX(hw)
	case hw>>11 == 0x9: // 01001: Format 6 LDR Rd, [PC, #imm8*4]
		rd := (hw >> 8) & 7
		addr := (c.thumbReadPC() &^ 3) + (hw&0xFF)*4
		v, err := c.mem.Read32(addr)
		if err != nil {
			return err
		}
		c.regs[rd] = v
		return nil
	default: // 0101: Format 7/8 レジスタオフセット LDR/STR
		return c.thumbLdstReg(hw)
	}
}

// thumbALU は Format 4（低レジスタ同士の演算）。
func (c *Core) thumbALU(hw uint32) error {
	rd := hw & 7
	rs := (hw >> 3) & 7
	a, b := c.regs[rd], c.regs[rs]
	cin := uint32(0)
	if c.cpsr.C() {
		cin = 1
	}
	switch (hw >> 6) & 0xF {
	case 0x0: // AND
		c.regs[rd] = a & b
		c.setNZ(c.regs[rd])
	case 0x1: // EOR
		c.regs[rd] = a ^ b
		c.setNZ(c.regs[rd])
	case 0x2: // LSL
		res, carry := shiftReg(a, 0, b&0xFF, c.cpsr.C())
		c.regs[rd] = res
		c.setNZ(res)
		c.cpsr = c.cpsr.set(FlagC, carry)
	case 0x3: // LSR
		res, carry := shiftReg(a, 1, b&0xFF, c.cpsr.C())
		c.regs[rd] = res
		c.setNZ(res)
		c.cpsr = c.cpsr.set(FlagC, carry)
	case 0x4: // ASR
		res, carry := shiftReg(a, 2, b&0xFF, c.cpsr.C())
		c.regs[rd] = res
		c.setNZ(res)
		c.cpsr = c.cpsr.set(FlagC, carry)
	case 0x5: // ADC
		res, cf, vf := addWithCarry(a, b, cin)
		c.regs[rd] = res
		c.setNZCV(res, cf, vf)
	case 0x6: // SBC
		res, cf, vf := addWithCarry(a, ^b, cin)
		c.regs[rd] = res
		c.setNZCV(res, cf, vf)
	case 0x7: // ROR
		res, carry := shiftReg(a, 3, b&0xFF, c.cpsr.C())
		c.regs[rd] = res
		c.setNZ(res)
		c.cpsr = c.cpsr.set(FlagC, carry)
	case 0x8: // TST
		c.setNZ(a & b)
	case 0x9: // NEG (RSB Rd, Rs, #0)
		res, cf, vf := addWithCarry(0, ^b, 1)
		c.regs[rd] = res
		c.setNZCV(res, cf, vf)
	case 0xA: // CMP
		res, cf, vf := addWithCarry(a, ^b, 1)
		c.setNZCV(res, cf, vf)
	case 0xB: // CMN
		res, cf, vf := addWithCarry(a, b, 0)
		c.setNZCV(res, cf, vf)
	case 0xC: // ORR
		c.regs[rd] = a | b
		c.setNZ(c.regs[rd])
	case 0xD: // MUL（C は ARM 同様に不変とする）
		c.regs[rd] = a * b
		c.setNZ(c.regs[rd])
	case 0xE: // BIC
		c.regs[rd] = a &^ b
		c.setNZ(c.regs[rd])
	default: // MVN
		c.regs[rd] = ^b
		c.setNZ(c.regs[rd])
	}
	return nil
}

// thumbHiRegBX は Format 5: ADD/CMP/MOV（高レジスタ可、フラグ変化は CMP のみ）と BX。
func (c *Core) thumbHiRegBX(hw uint32) error {
	rd := (hw & 7) | ((hw >> 4) & 8) // H1:Rd
	rs := (hw >> 3) & 0xF            // H2:Rs
	v := c.thumbReg(rs)
	switch (hw >> 8) & 3 {
	case 0: // ADD Rd, Rs
		res := c.thumbReg(rd) + v
		if rd == 15 {
			c.regs[15] = res &^ 1
		} else {
			c.regs[rd] = res
		}
	case 1: // CMP Rd, Rs
		res, cf, vf := addWithCarry(c.thumbReg(rd), ^v, 1)
		c.setNZCV(res, cf, vf)
	case 2: // MOV Rd, Rs
		if rd == 15 {
			c.regs[15] = v &^ 1
		} else {
			c.regs[rd] = v
		}
	default: // BX Rs（bit0 で ARM/Thumb 切替）
		if v&1 != 0 {
			c.cpsr |= FlagT
			c.regs[15] = v &^ 1
		} else {
			c.cpsr &^= FlagT
			c.regs[15] = v &^ 3
		}
	}
	return nil
}

// thumbLdstReg は Format 7/8: レジスタオフセットのロード/ストア。
func (c *Core) thumbLdstReg(hw uint32) error {
	rd := hw & 7
	addr := c.regs[(hw>>3)&7] + c.regs[(hw>>6)&7]
	switch (hw >> 9) & 7 {
	case 0: // STR
		return c.mem.Write32(addr&^3, c.regs[rd])
	case 1: // STRH
		return c.mem.Write16(addr&^1, uint16(c.regs[rd]))
	case 2: // STRB
		return c.mem.Write8(addr, uint8(c.regs[rd]))
	case 3: // LDRSB
		v, err := c.mem.Read8(addr)
		if err != nil {
			return err
		}
		c.regs[rd] = uint32(int32(int8(v)))
	case 4: // LDR（非アラインは ARM と同じ回転）
		v, err := c.mem.Read32(addr &^ 3)
		if err != nil {
			return err
		}
		c.regs[rd] = ror(v, 8*(addr&3))
	case 5: // LDRH
		v, err := c.mem.Read16(addr &^ 1)
		if err != nil {
			return err
		}
		c.regs[rd] = uint32(v)
	case 6: // LDRB
		v, err := c.mem.Read8(addr)
		if err != nil {
			return err
		}
		c.regs[rd] = uint32(v)
	default: // LDRSH
		v, err := c.mem.Read16(addr &^ 1)
		if err != nil {
			return err
		}
		c.regs[rd] = uint32(int32(int16(v)))
	}
	return nil
}

// thumbLdstImm は Format 9: LDR/STR Rd, [Rs, #imm5]（ワードは imm*4）。
func (c *Core) thumbLdstImm(hw uint32) error {
	rd := hw & 7
	rs := (hw >> 3) & 7
	imm := (hw >> 6) & 0x1F
	byteXfer := hw&(1<<12) != 0
	load := hw&(1<<11) != 0
	if byteXfer {
		addr := c.regs[rs] + imm
		if load {
			v, err := c.mem.Read8(addr)
			if err != nil {
				return err
			}
			c.regs[rd] = uint32(v)
			return nil
		}
		return c.mem.Write8(addr, uint8(c.regs[rd]))
	}
	addr := c.regs[rs] + imm*4
	if load {
		v, err := c.mem.Read32(addr &^ 3)
		if err != nil {
			return err
		}
		c.regs[rd] = ror(v, 8*(addr&3))
		return nil
	}
	return c.mem.Write32(addr&^3, c.regs[rd])
}

func (c *Core) thumbGroup100(hw uint32) error {
	rd := hw & 7
	if hw&(1<<12) == 0 { // Format 10: LDRH/STRH Rd, [Rs, #imm5*2]
		addr := c.regs[(hw>>3)&7] + ((hw>>6)&0x1F)*2
		if hw&(1<<11) != 0 {
			v, err := c.mem.Read16(addr &^ 1)
			if err != nil {
				return err
			}
			c.regs[rd] = uint32(v)
			return nil
		}
		return c.mem.Write16(addr&^1, uint16(c.regs[rd]))
	}
	// Format 11: LDR/STR Rd, [SP, #imm8*4]
	rd = (hw >> 8) & 7
	addr := c.regs[13] + (hw&0xFF)*4
	if hw&(1<<11) != 0 {
		v, err := c.mem.Read32(addr &^ 3)
		if err != nil {
			return err
		}
		c.regs[rd] = ror(v, 8*(addr&3))
		return nil
	}
	return c.mem.Write32(addr&^3, c.regs[rd])
}

func (c *Core) thumbGroup101(hw uint32) error {
	if hw&(1<<12) == 0 { // Format 12: ADD Rd, PC/SP, #imm8*4
		rd := (hw >> 8) & 7
		imm := (hw & 0xFF) * 4
		if hw&(1<<11) != 0 {
			c.regs[rd] = c.regs[13] + imm
		} else {
			c.regs[rd] = (c.thumbReadPC() &^ 3) + imm
		}
		return nil
	}
	switch {
	case (hw>>8)&0xF == 0: // Format 13: ADD SP, #±imm7*4
		imm := (hw & 0x7F) * 4
		if hw&(1<<7) != 0 {
			c.regs[13] -= imm
		} else {
			c.regs[13] += imm
		}
		return nil
	case (hw>>9)&3 == 2: // Format 14: PUSH/POP
		return c.thumbPushPop(hw)
	}
	return &UndefinedError{Reason: "unallocated Thumb encoding (misc 1011 space)"}
}

// thumbPushPop は Format 14。PUSH = STMDB SP!、POP = LDMIA SP!。
// v4T の POP {..pc} は状態切替なし（bit0 無視。interworking POP は v5）。
func (c *Core) thumbPushPop(hw uint32) error {
	list := hw & 0xFF
	pop := hw&(1<<11) != 0
	r := hw&(1<<8) != 0 // PUSH: LR / POP: PC を追加

	n := uint32(0)
	for i := uint32(0); i < 8; i++ {
		if list&(1<<i) != 0 {
			n++
		}
	}
	if r {
		n++
	}
	if n == 0 {
		return &UndefinedError{Reason: "PUSH/POP with empty list (UNPREDICTABLE)"}
	}

	sp := c.regs[13]
	if pop {
		addr := sp
		// ライトバックを先に行い、アボート時は復元（ARM 側 LDM と同じ方針）。
		c.regs[13] = sp + 4*n
		for i := uint32(0); i < 8; i++ {
			if list&(1<<i) == 0 {
				continue
			}
			v, err := c.mem.Read32(addr &^ 3)
			if err != nil {
				c.regs[13] = sp
				return err
			}
			c.regs[i] = v
			addr += 4
		}
		if r {
			v, err := c.mem.Read32(addr &^ 3)
			if err != nil {
				c.regs[13] = sp
				return err
			}
			c.regs[15] = v &^ 1
		}
		return nil
	}
	start := sp - 4*n
	addr := start
	for i := uint32(0); i < 8; i++ {
		if list&(1<<i) == 0 {
			continue
		}
		if err := c.mem.Write32(addr&^3, c.regs[i]); err != nil {
			return err
		}
		addr += 4
	}
	if r {
		if err := c.mem.Write32(addr&^3, c.regs[14]); err != nil {
			return err
		}
	}
	c.regs[13] = start
	return nil
}

func (c *Core) thumbGroup110(hw uint32) error {
	if hw&(1<<12) == 0 { // Format 15: LDMIA/STMIA Rn!, {rlist}
		return c.thumbLdmStm(hw)
	}
	cond := (hw >> 8) & 0xF
	switch cond {
	case 0xF: // Format 17: SWI
		// Thumb の SWI 復帰先は次の命令（regs[15] = PC+2 が既にそれ）。
		c.enterException(VecSWI, ModeSvc, c.regs[15])
		return nil
	case 0xE: // 1110 は未定義（ARM ARM A6.3.1）
		return &UndefinedError{Reason: "Thumb B with cond=1110 (undefined)", Arch: true}
	}
	// Format 16: 条件分岐 ±imm8*2
	if condPassed(c.cpsr, cond) {
		off := int32(int8(hw&0xFF)) * 2
		c.regs[15] = uint32(int32(c.thumbReadPC()) + off)
	}
	return nil
}

// thumbLdmStm は Format 15: LDMIA/STMIA Rn!, {r0-r7}。
func (c *Core) thumbLdmStm(hw uint32) error {
	rn := (hw >> 8) & 7
	list := hw & 0xFF
	if list == 0 {
		return &UndefinedError{Reason: "Thumb LDM/STM with empty list (UNPREDICTABLE)"}
	}
	load := hw&(1<<11) != 0
	base := c.regs[rn]
	n := uint32(0)
	for i := uint32(0); i < 8; i++ {
		if list&(1<<i) != 0 {
			n++
		}
	}

	addr := base
	if load {
		// ライトバック先行（rn がリストにあればロード値が勝つ）。アボートで復元。
		c.regs[rn] = base + 4*n
		for i := uint32(0); i < 8; i++ {
			if list&(1<<i) == 0 {
				continue
			}
			v, err := c.mem.Read32(addr &^ 3)
			if err != nil {
				c.regs[rn] = base
				return err
			}
			c.regs[i] = v
			addr += 4
		}
		return nil
	}
	for i := uint32(0); i < 8; i++ {
		if list&(1<<i) == 0 {
			continue
		}
		if err := c.mem.Write32(addr&^3, c.regs[i]); err != nil {
			return err
		}
		addr += 4
	}
	c.regs[rn] = base + 4*n
	return nil
}

// thumbBranchLong は Format 18（B）と Format 19（BL、2 ハーフワード）。
func (c *Core) thumbBranchLong(hw uint32) error {
	switch (hw >> 11) & 3 {
	case 0: // B ±imm11*2
		off := int32(hw<<21) >> 20 // 符号拡張して ×2
		c.regs[15] = uint32(int32(c.thumbReadPC()) + off)
		return nil
	case 2: // BL プレフィックス: LR = PC+4 + signext(imm11)<<12
		off := int32(hw<<21) >> 9 // 符号拡張して <<12
		c.regs[14] = uint32(int32(c.thumbReadPC()) + off)
		return nil
	case 3: // BL サフィックス: 分岐して LR = 次命令 | 1
		next := c.regs[14] + (hw&0x7FF)<<1
		c.regs[14] = c.regs[15] | 1 // regs[15] は今 PC+2 = 次命令
		c.regs[15] = next &^ 1
		return nil
	default: // 01: BLX サフィックス（ARMv5）
		// TODO(v5TE): PXA27x 対応時に BLX を実装する。
		return &UndefinedError{Reason: "BLX suffix (ARMv5) not implemented"}
	}
}
