package arm

// 頻出する命令形の特化（性能対策。ユーザー確認済み 2026-09）。
//
// デコード時に、よく実行される形の命令にはフィールド（レジスタ番号・即値・
// シフタキャリー）を取り出し済みの専用の実行関数を返す。汎用の exec 関数
// （execDataProc など）は実行のたびに命令語を分解し、16 種類の演算の switch と
// フラグ処理の分岐を通るため。デコード結果はデコードキャッシュに残るので、
// 取り出しは物理ページ上の命令ごとに 1 回で済む。
//
// 対象は WM5 の実行で多い形（2026-09 に実行命令の分布を計測: LDR 即値 18%、
// CMP 即値 8%、MOV 即値・レジスタ 各 7〜8%、条件分岐 9% など）。
// 意味は汎用の実装と完全に同じでなければならない（special_test.go が
// ランダムな命令語と状態で汎用版と突き合わせる）。境界的な形（Rd/Rn=PC、
// ライトバック、シフトつきオペランド、ADC/SBC/RSB/RSC 等）は特化せず汎用に回す。

// specialize は word に専用の実行関数があれば返す（なければ nil）。
// 条件フィールドは Run が判定するので、ここでは見ない（0xF は対象外）。
func specialize(word uint32) execFn {
	if word>>28 == 0xF {
		return nil
	}
	switch (word >> 25) & 7 {
	case 1:
		return specDataProcImm(word)
	case 0:
		if word&0xFF0 == 0 { // レジスタ・シフトなし（LSL #0）
			return specDataProcReg(word)
		}
	case 2:
		return specLdstImm(word)
	case 5:
		return specBranch(word)
	}
	return nil
}

// specDataProcImm は即値オペランドのデータ処理（Rd・Rn が PC でない形）。
func specDataProcImm(word uint32) execFn {
	op := (word >> 21) & 0xF
	s := word&(1<<20) != 0
	rn := (word >> 16) & 0xF
	rd := (word >> 12) & 0xF
	if op >= opTST && op <= opCMN && !s {
		return nil // MRS/MSR の空間
	}
	if rn == 15 || (rd == 15 && !(op >= opTST && op <= opCMN)) {
		return nil
	}
	rot := ((word >> 8) & 0xF) * 2
	imm := ror(word&0xFF, rot)
	// 論理演算の C: 回転なしならシフタキャリー = 現在の C（変えない）、
	// 回転ありなら即値の bit31。
	setC, cval := rot != 0, imm&0x80000000 != 0

	if !s {
		switch op {
		case opMOV:
			return func(c *Core, _ uint32) error { c.regs[rd] = imm; return nil }
		case opMVN:
			nimm := ^imm
			return func(c *Core, _ uint32) error { c.regs[rd] = nimm; return nil }
		case opADD:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] + imm; return nil }
		case opSUB:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] - imm; return nil }
		case opAND:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] & imm; return nil }
		case opORR:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] | imm; return nil }
		case opEOR:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] ^ imm; return nil }
		case opBIC:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] &^ imm; return nil }
		}
		return nil
	}
	switch op {
	case opCMP:
		return func(c *Core, _ uint32) error {
			res, cf, vf := addWithCarry(c.regs[rn], ^imm, 1)
			c.cpsr = c.cpsr.SetNZ(res).set(FlagC, cf).set(FlagV, vf)
			return nil
		}
	case opCMN:
		return func(c *Core, _ uint32) error {
			res, cf, vf := addWithCarry(c.regs[rn], imm, 0)
			c.cpsr = c.cpsr.SetNZ(res).set(FlagC, cf).set(FlagV, vf)
			return nil
		}
	case opSUB:
		return func(c *Core, _ uint32) error {
			res, cf, vf := addWithCarry(c.regs[rn], ^imm, 1)
			c.cpsr = c.cpsr.SetNZ(res).set(FlagC, cf).set(FlagV, vf)
			c.regs[rd] = res
			return nil
		}
	case opADD:
		return func(c *Core, _ uint32) error {
			res, cf, vf := addWithCarry(c.regs[rn], imm, 0)
			c.cpsr = c.cpsr.SetNZ(res).set(FlagC, cf).set(FlagV, vf)
			c.regs[rd] = res
			return nil
		}
	}
	// 論理演算（S=1）: N/Z は結果、C はシフタキャリー、V は不変。
	logic := func(f func(rn uint32) uint32, write bool) execFn {
		return func(c *Core, _ uint32) error {
			res := f(c.regs[rn])
			p := c.cpsr.SetNZ(res)
			if setC {
				p = p.set(FlagC, cval)
			}
			c.cpsr = p
			if write {
				c.regs[rd] = res
			}
			return nil
		}
	}
	switch op {
	case opTST:
		return logic(func(v uint32) uint32 { return v & imm }, false)
	case opTEQ:
		return logic(func(v uint32) uint32 { return v ^ imm }, false)
	case opAND:
		return logic(func(v uint32) uint32 { return v & imm }, true)
	case opORR:
		return logic(func(v uint32) uint32 { return v | imm }, true)
	case opMOV:
		return logic(func(uint32) uint32 { return imm }, true)
	}
	return nil
}

// specDataProcReg はレジスタオペランド（シフトなし）のデータ処理
// （Rd・Rn・Rm が PC でない形）。シフトなしのシフタキャリーは現在の C。
func specDataProcReg(word uint32) execFn {
	op := (word >> 21) & 0xF
	s := word&(1<<20) != 0
	rn := (word >> 16) & 0xF
	rd := (word >> 12) & 0xF
	rm := word & 0xF
	if op >= opTST && op <= opCMN && !s {
		return nil // MRS/MSR・BX などの空間
	}
	if rm == 15 || rn == 15 || (rd == 15 && !(op >= opTST && op <= opCMN)) {
		return nil
	}
	if !s {
		switch op {
		case opMOV:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rm]; return nil }
		case opADD:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] + c.regs[rm]; return nil }
		case opSUB:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] - c.regs[rm]; return nil }
		case opAND:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] & c.regs[rm]; return nil }
		case opORR:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] | c.regs[rm]; return nil }
		case opEOR:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] ^ c.regs[rm]; return nil }
		case opBIC:
			return func(c *Core, _ uint32) error { c.regs[rd] = c.regs[rn] &^ c.regs[rm]; return nil }
		}
		return nil
	}
	switch op {
	case opCMP:
		return func(c *Core, _ uint32) error {
			res, cf, vf := addWithCarry(c.regs[rn], ^c.regs[rm], 1)
			c.cpsr = c.cpsr.SetNZ(res).set(FlagC, cf).set(FlagV, vf)
			return nil
		}
	case opSUB:
		return func(c *Core, _ uint32) error {
			res, cf, vf := addWithCarry(c.regs[rn], ^c.regs[rm], 1)
			c.cpsr = c.cpsr.SetNZ(res).set(FlagC, cf).set(FlagV, vf)
			c.regs[rd] = res
			return nil
		}
	case opADD:
		return func(c *Core, _ uint32) error {
			res, cf, vf := addWithCarry(c.regs[rn], c.regs[rm], 0)
			c.cpsr = c.cpsr.SetNZ(res).set(FlagC, cf).set(FlagV, vf)
			c.regs[rd] = res
			return nil
		}
	case opMOV: // MOVS: N/Z のみ（C はシフタキャリー = 現在の C、V 不変）
		return func(c *Core, _ uint32) error {
			v := c.regs[rm]
			c.cpsr = c.cpsr.SetNZ(v)
			c.regs[rd] = v
			return nil
		}
	case opTST:
		return func(c *Core, _ uint32) error {
			c.cpsr = c.cpsr.SetNZ(c.regs[rn] & c.regs[rm])
			return nil
		}
	}
	return nil
}

// specLdstImm は LDR/STR/LDRB/STRB の即値オフセット（プリインデックス・
// ライトバックなし）。ロードの Rd=PC と、ストアの Rd=PC は汎用に回す。
// Rn=PC（リテラルプールの読み出し）は PC+8 を基準にする。
func specLdstImm(word uint32) execFn {
	if word&(1<<24) == 0 || word&(1<<21) != 0 {
		return nil // ポストインデックス・ライトバック
	}
	rn := (word >> 16) & 0xF
	rd := (word >> 12) & 0xF
	if rd == 15 {
		return nil
	}
	off := word & 0xFFF
	if word&(1<<23) == 0 {
		off = -off // uint32 の 2 の補数で加算すれば減算になる
	}
	byteXfer := word&(1<<22) != 0
	if rn == 15 {
		off += 4 // regs[15] は実行中 PC+4 なので、PC+8 基準にする
	}
	switch {
	case word&(1<<20) != 0 && !byteXfer: // LDR
		return func(c *Core, _ uint32) error {
			addr := c.regs[rn] + off
			v, err := c.mem.Read32(addr &^ 3)
			if err != nil {
				return err
			}
			c.regs[rd] = ror(v, 8*(addr&3))
			return nil
		}
	case word&(1<<20) != 0: // LDRB
		return func(c *Core, _ uint32) error {
			b, err := c.mem.Read8(c.regs[rn] + off)
			if err != nil {
				return err
			}
			c.regs[rd] = uint32(b)
			return nil
		}
	case !byteXfer: // STR
		return func(c *Core, _ uint32) error {
			return c.mem.Write32((c.regs[rn]+off)&^3, c.regs[rd])
		}
	default: // STRB
		return func(c *Core, _ uint32) error {
			return c.mem.Write8(c.regs[rn]+off, uint8(c.regs[rd]))
		}
	}
}

// specBranch は B/BL（オフセットを取り出し済み）。
func specBranch(word uint32) execFn {
	off := uint32(int32(word<<8)>>6) + 4 // PC+8 基準（regs[15] は PC+4）
	switch {
	case word&(1<<24) != 0: // BL
		return func(c *Core, _ uint32) error {
			c.regs[14] = c.regs[15]
			c.regs[15] += off
			return nil
		}
	case word&0x01FFFFFF == 0x00FFFFFC: // 3 命令ループ先頭への後方分岐（idle.go）
		return func(c *Core, _ uint32) error {
			c.regs[15] += off
			c.spinHint = true
			return nil
		}
	default:
		return func(c *Core, _ uint32) error {
			c.regs[15] += off
			return nil
		}
	}
}
