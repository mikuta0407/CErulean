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

// userReg / setUserReg は現在モードに関係なく usr バンクのレジスタを
// 読み書きする（LDM(2)/STM(2) 用）。r0-r7 は全モード共有。
// r8-r12 は FIQ モードのときだけ退避領域（bankR8Usr）側が usr の値。
// r13/r14 は usr/sys 以外のモードでは bankR13/R14[bankUsr] 側が usr の値。
func (c *Core) userReg(i uint32) uint32 {
	b := c.curBank()
	switch {
	case i >= 8 && i <= 12 && b == bankFiq:
		return c.bankR8Usr[i-8]
	case i == 13 && b != bankUsr:
		return c.bankR13[bankUsr]
	case i == 14 && b != bankUsr:
		return c.bankR14[bankUsr]
	}
	return c.regs[i]
}

func (c *Core) setUserReg(i uint32, v uint32) {
	b := c.curBank()
	switch {
	case i >= 8 && i <= 12 && b == bankFiq:
		c.bankR8Usr[i-8] = v
	case i == 13 && b != bankUsr:
		c.bankR13[bankUsr] = v
	case i == 14 && b != bankUsr:
		c.bankR14[bankUsr] = v
	default:
		c.regs[i] = v
	}
}

// execLdmStmUser は LDM(2)/STM(2)（ユーザーバンク転送）本体。
// アドレス計算は execLdmStm と同じ正規化を使う（ライトバックはなし）。
func (c *Core) execLdmStmUser(word uint32, load bool) error {
	pre := word&(1<<24) != 0
	up := word&(1<<23) != 0
	rn := (word >> 16) & 0xF
	list := word & 0xFFFF
	n := uint32(bits.OnesCount32(list))

	start := c.readReg(rn)
	switch {
	case up && !pre: // IA
	case up && pre: // IB
		start += 4
	case !up && !pre: // DA
		start -= 4*n - 4
	default: // DB
		start -= 4 * n
	}

	addr := start
	for i := uint32(0); i < 16; i++ {
		if list&(1<<i) == 0 {
			continue
		}
		if load {
			// LDM(2) に PC は含まれない（含む形は LDM(3) として処理済み）。
			v, err := c.mem.Read32(addr &^ 3)
			if err != nil {
				return err
			}
			c.setUserReg(i, v)
		} else {
			v := c.userReg(i)
			if i == 15 {
				v = c.readReg(15) // PC は通常の STM と同じ PC+8
			}
			if err := c.mem.Write32(addr&^3, v); err != nil {
				return err
			}
		}
		addr += 4
	}
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
		// LDM(2)/STM(2): ユーザーバンク転送（現在モードに関係なく usr の
		// r8-r14 を読み書きする）。WinCE はスレッドのコンテキスト切替で使う。
		// ライトバックは UNPREDICTABLE（W=0 であるべき）なので止めて気づく。
		if writeback {
			return &UndefinedError{Reason: "LDM(2)/STM(2) with writeback (UNPREDICTABLE)"}
		}
		return c.execLdmStmUser(word, load)
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
				// ARM9 系はアボート時にベースを命令実行前の値へ戻す
				// （base restored モデル）。途中までロードしたレジスタは
				// そのまま（実機でも上書きされ得る）。
				if writeback && rn != 15 {
					c.regs[rn] = base
				}
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
