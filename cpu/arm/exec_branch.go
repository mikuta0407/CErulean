package arm

// 分岐命令と SWI、コプロセッサレジスタ転送。

// execBranch は B/BL（A3.3）。24bit 符号付きオフセット×4 を PC+8 に加える。
func execBranch(c *Core, word uint32) error {
	offset := int32(word<<8) >> 6 // 符号拡張して ×4（<<8 で上位に詰め、>>6 = 算術>>8 と <<2）
	if word&(1<<24) != 0 {        // L: リンク
		c.regs[14] = c.regs[15] // regs[15] は今 PC+4 = 次命令 = 復帰先
	}
	c.regs[15] = uint32(int32(c.regs[15]+4) + offset) // PC+8 基準
	return nil
}

// execBX は BX Rm（A3.3）。bit0 で ARM/Thumb を切り替える（v4T の相互運用）。
func execBX(c *Core, word uint32) error {
	target := c.readReg(word & 0xF)
	if target&1 != 0 {
		c.cpsr |= FlagT
		c.regs[15] = target &^ 1
		// Thumb 実行は未実装なので、次の Step が UndefinedError で止まる。
	} else {
		c.cpsr &^= FlagT
		c.regs[15] = target &^ 3
	}
	return nil
}

// execSWI はソフトウェア割り込み。SVC モードでベクタ 0x08 へ。
// 復帰先は SWI の次の命令。
func execSWI(c *Core, word uint32) error {
	c.enterException(VecSWI, ModeSvc, c.regs[15]) // regs[15] = PC+4
	return nil
}

// execMcrMrc は MCR/MRC（コプロセッサレジスタ転送、A4.1.32/40）。
// CP15（MMU・キャッシュ制御）だけを Coprocessor interface 経由で通し、
// それ以外のコプロセッサは未実装エラーにする。
func execMcrMrc(c *Core, word uint32) error {
	cpNum := (word >> 8) & 0xF
	if cpNum != 15 {
		// ARM920T に CP15 以外のコプロセッサはなく、実機でも未定義命令例外
		// になる。WinCE は FPU 検出のため意図的に p10（VFP）等を叩く。
		return &UndefinedError{Reason: "no such coprocessor (undefined exception on real HW)", Arch: true}
	}
	if c.cp15 == nil {
		return &UndefinedError{Reason: "CP15 not connected to this core"}
	}
	opc1 := uint8((word >> 21) & 7)
	crn := uint8((word >> 16) & 0xF)
	rd := (word >> 12) & 0xF
	opc2 := uint8((word >> 5) & 7)
	crm := uint8(word & 0xF)

	if word&(1<<20) != 0 { // MRC: コプロセッサ → レジスタ
		v, err := c.cp15.Read(opc1, crn, crm, opc2)
		if err != nil {
			return err
		}
		if rd == 15 {
			// MRC ... , r15 はフラグ N/Z/C/V に上位 4 ビットを書く特殊形。
			c.cpsr = (c.cpsr &^ (FlagN | FlagZ | FlagC | FlagV)) | (PSR(v) & (FlagN | FlagZ | FlagC | FlagV))
			return nil
		}
		c.regs[rd] = v
		return nil
	}
	// MCR: レジスタ → コプロセッサ
	return c.cp15.Write(opc1, crn, crm, opc2, c.readReg(rd))
}
