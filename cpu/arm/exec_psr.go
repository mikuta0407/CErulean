package arm

// MRS/MSR（ステータスレジスタ転送、A4.1.38/39）。
// WinCE のブートコードはモード切替・割り込み制御でこれらを多用するので、
// マイルストーン1から実装しておく。

// execMRS は MRS Rd, CPSR/SPSR。
func execMRS(c *Core, word uint32) error {
	rd := (word >> 12) & 0xF
	if rd == 15 {
		return &UndefinedError{Reason: "MRS with Rd=PC (UNPREDICTABLE)"}
	}
	if word&(1<<22) != 0 { // R: SPSR
		b := c.curBank()
		if b == bankUsr {
			// usr/sys に SPSR はない（UNPREDICTABLE）。0 を返すより止めて気づけるように。
			return &UndefinedError{Reason: "MRS SPSR in usr/sys mode (no SPSR)"}
		}
		c.regs[rd] = uint32(c.spsr[b])
		return nil
	}
	c.regs[rd] = uint32(c.cpsr)
	return nil
}

// execMSR は MSR CPSR/SPSR_<fields>, Rm または #imm。
// フィールドマスク（bits 19:16 = f,s,x,c）で書き込むバイトを選ぶ。
func execMSR(c *Core, word uint32) error {
	var val uint32
	if word&(1<<25) != 0 { // 即値形式
		val = ror(word&0xFF, ((word>>8)&0xF)*2)
	} else {
		val = c.readReg(word & 0xF)
	}

	var mask uint32
	if word&(1<<16) != 0 {
		mask |= 0x000000FF // c: 制御ビット（モード・I/F/T）
	}
	if word&(1<<17) != 0 {
		mask |= 0x0000FF00 // x
	}
	if word&(1<<18) != 0 {
		mask |= 0x00FF0000 // s
	}
	if word&(1<<19) != 0 {
		mask |= 0xFF000000 // f: フラグ
	}

	if word&(1<<22) != 0 { // R: SPSR へ
		b := c.curBank()
		if b == bankUsr {
			return &UndefinedError{Reason: "MSR SPSR in usr/sys mode (no SPSR)"}
		}
		c.spsr[b] = PSR((uint32(c.spsr[b]) &^ mask) | (val & mask))
		return nil
	}

	// CPSR へ。ユーザーモードでは制御ビットは書けない（フラグのみ）。
	if c.cpsr.Mode() == ModeUsr {
		mask &= 0xFF000000
	}
	newPSR := PSR((uint32(c.cpsr) &^ mask) | (val & mask))
	if bankIndex(newPSR.Mode()) < 0 {
		// 存在しないモード番号への切替は UNPREDICTABLE。黙って壊れるより停止。
		return &UndefinedError{Reason: "MSR writes an invalid mode to CPSR (UNPREDICTABLE)"}
	}
	c.setCPSR(newPSR)
	return nil
}
