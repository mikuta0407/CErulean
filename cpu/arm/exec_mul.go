package arm

// 乗算命令（ARM ARM A3.5）と SWP/SWPB（A4.1.51/52）。
// どちらも bits[7:4]=1001 の「データ処理と同居する空間」にある（Figure A3-3）。
//
// フラグの扱い（ARMv4）:
//   - MUL/MLA の S=1: N/Z は結果から。C は UNPREDICTABLE、V は不変。
//     ここでは C/V とも不変にする（「変えない」が最も再現性が高い）。
//   - ロング乗算の S=1: N は bit63、Z は 64bit 結果全体。C/V は同上。

// execMulSwp は bits[7:4]=1001 空間のディスパッチ。
// bit24=0 が乗算、bit24=1 が SWP/SWPB。
func execMulSwp(c *Core, word uint32) error {
	if word&(1<<24) != 0 {
		return execSwp(c, word)
	}
	if word&(1<<23) != 0 {
		return execMulLong(c, word)
	}
	return execMul(c, word)
}

// execMul は MUL（A=0）/ MLA（A=1）。Rd = Rm*Rs (+ Rn)。
// オペランドに r15 を使うのは UNPREDICTABLE（readReg の PC+8 のまま扱う）。
func execMul(c *Core, word uint32) error {
	rd := (word >> 16) & 0xF
	rn := (word >> 12) & 0xF
	res := c.readReg(word&0xF) * c.readReg((word>>8)&0xF)
	if word&(1<<21) != 0 { // A: アキュムレート
		res += c.readReg(rn)
	}
	c.writeReg(rd, res)
	if word&(1<<20) != 0 { // S
		c.cpsr = c.cpsr.SetNZ(res)
	}
	return nil
}

// execMulLong は UMULL/UMLAL/SMULL/SMLAL。{RdHi,RdLo} = Rm*Rs (+ {RdHi,RdLo})。
// bit22: 1=符号付き、bit21: 1=アキュムレート。
func execMulLong(c *Core, word uint32) error {
	rdHi := (word >> 16) & 0xF
	rdLo := (word >> 12) & 0xF
	rm := c.readReg(word & 0xF)
	rs := c.readReg((word >> 8) & 0xF)

	var res uint64
	if word&(1<<22) != 0 { // 符号付き
		res = uint64(int64(int32(rm)) * int64(int32(rs)))
	} else {
		res = uint64(rm) * uint64(rs)
	}
	if word&(1<<21) != 0 { // アキュムレート
		res += uint64(c.readReg(rdHi))<<32 | uint64(c.readReg(rdLo))
	}
	c.writeReg(rdLo, uint32(res))
	c.writeReg(rdHi, uint32(res>>32))
	if word&(1<<20) != 0 { // S
		p := c.cpsr.set(FlagN, res&(1<<63) != 0)
		c.cpsr = p.set(FlagZ, res == 0)
	}
	return nil
}

// execSwp は SWP/SWPB（アトミック交換）: temp=Mem[Rn]; Mem[Rn]=Rm; Rd=temp。
// シングルコア・逐次実行なのでアトミック性は自動的に満たされる。
func execSwp(c *Core, word uint32) error {
	if word&(1<<23) != 0 || word&(3<<20) != 0 {
		// bit24=1 空間で SWP 以外のビットパターンは v4 では未定義。
		return &UndefinedError{Reason: "undefined encoding in swap space"}
	}
	addr := c.readReg((word >> 16) & 0xF)
	rd := (word >> 12) & 0xF
	rmVal := c.readReg(word & 0xF)

	if word&(1<<22) != 0 { // B: バイト
		old, err := c.mem.Read8(addr)
		if err != nil {
			return err
		}
		if err := c.mem.Write8(addr, uint8(rmVal)); err != nil {
			return err
		}
		c.writeReg(rd, uint32(old))
		return nil
	}
	// ワード: 非アラインアドレスのロード値は LDR と同じ回転動作。
	old, err := c.mem.Read32(addr &^ 3)
	if err != nil {
		return err
	}
	if err := c.mem.Write32(addr&^3, rmVal); err != nil {
		return err
	}
	c.writeReg(rd, ror(old, 8*(addr&3)))
	return nil
}
