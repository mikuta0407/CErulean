package arm

import "math/bits"

// ロード/ストア命令（ARM ARM A3.11、アドレッシングは A5.2/A5.3/A5.4）。

// execLdst は LDR/STR/LDRB/STRB（ワード・バイト転送）。
func execLdst(c *Core, word uint32) error {
	var (
		pre       = word&(1<<24) != 0 // P: プリインデックス
		up        = word&(1<<23) != 0 // U: オフセットを加算
		byteXfer  = word&(1<<22) != 0 // B: バイト転送
		writeback = word&(1<<21) != 0 // W
		load      = word&(1<<20) != 0 // L
	)
	rn := (word >> 16) & 0xF
	rd := (word >> 12) & 0xF

	var offset uint32
	if word&(1<<25) != 0 {
		// スケーリング付きレジスタオフセット（シフト量は即値のみ）。
		// シフタキャリーはアドレス計算では使わない。
		rm := c.readReg(word & 0xF)
		offset, _ = shiftImm(rm, (word>>5)&3, (word>>7)&0x1F, c.cpsr.C())
	} else {
		offset = word & 0xFFF
	}

	base := c.readReg(rn)
	indexed := base + offset
	if !up {
		indexed = base - offset
	}
	addr := base
	if pre {
		addr = indexed
	}
	// TODO: P=0 かつ W=1 は LDRT/STRT（ユーザー権限でのアクセス）。
	// MMU の権限チェックを実装するまでは通常アクセスと同じ扱いにする。

	if load {
		var val uint32
		if byteXfer {
			b, err := c.mem.Read8(addr)
			if err != nil {
				return err
			}
			val = uint32(b)
		} else {
			v, err := c.mem.Read32(addr &^ 3)
			if err != nil {
				return err
			}
			// ARMv4 の非アラインワードロード: アラインしたワードを
			// アドレス下位 2 ビット×8 だけ右ローテートした値になる。
			// （CP15 の A ビットでアボートにもできるが、既定は回転動作）
			val = ror(v, 8*(addr&3))
		}
		// ライトバック → レジスタ書き込みの順。rd == rn のときは
		// ロード値が勝つ（ARM ARM の規定どおり）。
		if !pre || writeback {
			c.writeReg(rn, indexed)
		}
		c.writeReg(rd, val) // rd=15 なら分岐（v4T: Thumb 切替なし、bit1:0 無視）
	} else {
		// STR で rd=15 のとき格納される値は実装定義（PC+8 か PC+12）。
		// readReg は PC+8 を返す。TODO: ARM920T の実際の値を確認する。
		val := c.readReg(rd)
		var err error
		if byteXfer {
			err = c.mem.Write8(addr, uint8(val))
		} else {
			err = c.mem.Write32(addr&^3, val)
		}
		if err != nil {
			return err
		}
		if !pre || writeback {
			c.writeReg(rn, indexed)
		}
	}
	return nil
}

// execLdstMisc はハーフワード・符号付き転送（LDRH/STRH/LDRSB/LDRSH）。
// エンコードはデータ処理空間の bit7=1,bit4=1 側（A5.3）。
func execLdstMisc(c *Core, word uint32) error {
	var (
		pre       = word&(1<<24) != 0
		up        = word&(1<<23) != 0
		immForm   = word&(1<<22) != 0 // 1: 8bit 即値オフセット / 0: レジスタ
		writeback = word&(1<<21) != 0
		load      = word&(1<<20) != 0
	)
	rn := (word >> 16) & 0xF
	rd := (word >> 12) & 0xF
	sh := (word >> 5) & 3 // 01=H, 10=SB, 11=SH

	var offset uint32
	if immForm {
		offset = ((word >> 4) & 0xF0) | (word & 0xF)
	} else {
		offset = c.readReg(word & 0xF)
	}

	base := c.readReg(rn)
	indexed := base + offset
	if !up {
		indexed = base - offset
	}
	addr := base
	if pre {
		addr = indexed
	}

	if !load {
		if sh != 1 {
			// L=0 の SB/SH は v5TE では LDRD/STRD。
			// TODO(v5TE): PXA27x 対応時に実装する。
			return &UndefinedError{Reason: "LDRD/STRD (ARMv5TE) not implemented"}
		}
		// STRH。非アラインは UNPREDICTABLE なのでアラインして扱う。
		if err := c.mem.Write16(addr&^1, uint16(c.readReg(rd))); err != nil {
			return err
		}
		if !pre || writeback {
			c.writeReg(rn, indexed)
		}
		return nil
	}

	var val uint32
	switch sh {
	case 1: // LDRH（ゼロ拡張）
		v, err := c.mem.Read16(addr &^ 1)
		if err != nil {
			return err
		}
		val = uint32(v)
	case 2: // LDRSB（符号拡張）
		v, err := c.mem.Read8(addr)
		if err != nil {
			return err
		}
		val = uint32(int32(int8(v)))
	case 3: // LDRSH（符号拡張）
		v, err := c.mem.Read16(addr &^ 1)
		if err != nil {
			return err
		}
		val = uint32(int32(int16(v)))
	default: // sh=00 はここに来ない（decode で乗算系に振り分け済み）
		return &UndefinedError{Reason: "misc load/store with sh=00"}
	}
	if !pre || writeback {
		c.writeReg(rn, indexed)
	}
	c.writeReg(rd, val)
	return nil
}

// execLdmStm は LDM/STM（A3.12、アドレッシングは A5.4）。
func execLdmStm(c *Core, word uint32) error {
	var (
		pre       = word&(1<<24) != 0
		up        = word&(1<<23) != 0
		sBit      = word&(1<<22) != 0
		writeback = word&(1<<21) != 0
		load      = word&(1<<20) != 0
	)
	rn := (word >> 16) & 0xF
	list := word & 0xFFFF
	n := uint32(bits.OnesCount32(list))
	if n == 0 {
		return &UndefinedError{Reason: "LDM/STM with empty register list (UNPREDICTABLE)"}
	}
	hasPC := list&(1<<15) != 0

	if sBit && !(load && hasPC) {
		// S ビット付きでも LDM {..pc}^ 以外（ユーザーバンク転送）は未実装。
		// TODO: WinCE カーネルがユーザーモード復帰で使う可能性が高いので、
		// 必要になったら実装する（バンク切替なしで usr の r13/r14 を読み書きする）。
		return &UndefinedError{Reason: "LDM/STM user-bank transfer (S bit) not implemented"}
	}

	base := c.readReg(rn)
	// 転送は常に「小さい番号のレジスタが小さいアドレス」。
	// 4 つのモード (IA/IB/DA/DB) は開始アドレスの違いに正規化できる。
	start := base
	switch {
	case up && !pre: // IA
	case up && pre: // IB
		start += 4
	case !up && !pre: // DA
		start -= 4*n - 4
	default: // DB
		start -= 4 * n
	}
	newBase := base + 4*n
	if !up {
		newBase = base - 4*n
	}

	if load {
		// ライトバックを先に行う。rn がリストに含まれる場合はロード値が上書きする
		// （ARM ARM: その場合のライトバック値は UNPREDICTABLE。ロード値優先に倒す）。
		if writeback {
			c.writeReg(rn, newBase)
		}
		addr := start
		for i := uint32(0); i < 16; i++ {
			if list&(1<<i) == 0 {
				continue
			}
			v, err := c.mem.Read32(addr &^ 3)
			if err != nil {
				return err
			}
			if i == 15 {
				if sBit {
					// LDM {..pc}^: 例外復帰。CPSR ← SPSR を先に行い、
					// 復帰後の state（ARM/Thumb）で PC をアラインする。
					if b := c.curBank(); b != bankUsr {
						c.setCPSR(c.spsr[b])
					}
					if c.cpsr.T() {
						v &^= 1
					} else {
						v &^= 3
					}
					c.regs[15] = v
				} else {
					c.writeReg(15, v)
				}
			} else {
				c.regs[i] = v
			}
			addr += 4
		}
	} else {
		// STM: 先に全ストアしてからライトバック。これにより rn がリストに
		// 含まれていても格納されるのは変更前の値になる（リスト先頭が rn の
		// 場合の ARM ARM の規定と一致。それ以外の位置は UNPREDICTABLE）。
		addr := start
		for i := uint32(0); i < 16; i++ {
			if list&(1<<i) == 0 {
				continue
			}
			v := c.regs[i]
			if i == 15 {
				v = c.readReg(15) // PC は PC+8（実装定義。STR と同じ扱い）
			}
			if err := c.mem.Write32(addr&^3, v); err != nil {
				return err
			}
			addr += 4
		}
		if writeback {
			c.writeReg(rn, newBase)
		}
	}
	return nil
}
