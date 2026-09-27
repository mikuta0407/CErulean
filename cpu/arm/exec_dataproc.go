package arm

import "math/bits"

// データ処理命令（ARM ARM A3.4）とバレルシフタ（A5.1）。
//
// フラグ計算の要点:
//   - 論理系（AND/EOR/TST/TEQ/ORR/MOV/BIC/MVN）: N/Z は結果から、
//     C は「シフタキャリー」、V は変化しない。
//   - 加算系（ADD/ADC/CMN）: C はキャリーアウト、V は符号付きオーバーフロー。
//   - 減算系（SUB/SBC/RSB/RSC/CMP）: a-b を a+^b+1 として計算するので、
//     C は「ボローなし」で 1 になる（x86 と逆なので注意）。

func ror(v uint32, n uint32) uint32 {
	return bits.RotateLeft32(v, -int(n&31))
}

// addWithCarry は a + b + cin を計算し、(結果, キャリー, オーバーフロー) を返す。
// 減算は b をビット反転して cin を調整すれば同じ式に乗る（ARM ARM の定義どおり）。
func addWithCarry(a, b uint32, cin uint32) (res uint32, cout, vout bool) {
	r64 := uint64(a) + uint64(b) + uint64(cin)
	res = uint32(r64)
	cout = r64 > 0xFFFFFFFF
	// V: 同符号の 2 数を足して結果の符号が変わったらオーバーフロー
	vout = (a^res)&(b^res)&0x80000000 != 0
	return
}

// shiftImm は即値シフト量によるシフト（A5.1.5 など）。
// ARM のエンコードでは「シフト量 0」が種類ごとに特別な意味を持つ:
// LSR #0 → LSR #32、ASR #0 → ASR #32、ROR #0 → RRX。
func shiftImm(v uint32, shiftType, amount uint32, carryIn bool) (uint32, bool) {
	switch shiftType {
	case 0: // LSL
		if amount == 0 {
			return v, carryIn
		}
		return v << amount, v&(1<<(32-amount)) != 0
	case 1: // LSR
		if amount == 0 { // LSR #32
			return 0, v&0x80000000 != 0
		}
		return v >> amount, v&(1<<(amount-1)) != 0
	case 2: // ASR
		if amount == 0 { // ASR #32
			if v&0x80000000 != 0 {
				return 0xFFFFFFFF, true
			}
			return 0, false
		}
		return uint32(int32(v) >> amount), v&(1<<(amount-1)) != 0
	default: // ROR / RRX
		if amount == 0 { // RRX: キャリーを最上位に入れて 1 ビット右回転
			res := v >> 1
			if carryIn {
				res |= 0x80000000
			}
			return res, v&1 != 0
		}
		res := ror(v, amount)
		return res, res&0x80000000 != 0
	}
}

// shiftReg はレジスタ指定シフト量（Rs の下位 8 ビット）によるシフト。
// 即値と違い、量 0 は「シフトなし」、32 以上も規定がある（A5.1.7 など）。
func shiftReg(v uint32, shiftType, amount uint32, carryIn bool) (uint32, bool) {
	if amount == 0 {
		return v, carryIn
	}
	switch shiftType {
	case 0: // LSL
		switch {
		case amount < 32:
			return v << amount, v&(1<<(32-amount)) != 0
		case amount == 32:
			return 0, v&1 != 0
		default:
			return 0, false
		}
	case 1: // LSR
		switch {
		case amount < 32:
			return v >> amount, v&(1<<(amount-1)) != 0
		case amount == 32:
			return 0, v&0x80000000 != 0
		default:
			return 0, false
		}
	case 2: // ASR
		if amount < 32 {
			return uint32(int32(v) >> amount), v&(1<<(amount-1)) != 0
		}
		if v&0x80000000 != 0 {
			return 0xFFFFFFFF, true
		}
		return 0, false
	default: // ROR
		amount &= 31
		if amount == 0 { // 32 の倍数: 値そのまま、C は bit31
			return v, v&0x80000000 != 0
		}
		res := ror(v, amount)
		return res, res&0x80000000 != 0
	}
}

// dataProcOperand は shifter_operand を評価して（値, シフタキャリー）を返す。
func (c *Core) dataProcOperand(word uint32) (uint32, bool) {
	carryIn := c.cpsr.C()
	if word&(1<<25) != 0 {
		// 即値: imm8 を rot*2 だけ右ローテート
		imm := word & 0xFF
		rot := ((word >> 8) & 0xF) * 2
		val := ror(imm, rot)
		if rot == 0 {
			return val, carryIn
		}
		return val, val&0x80000000 != 0
	}
	rm := c.readReg(word & 0xF)
	shiftType := (word >> 5) & 3
	if word&(1<<4) != 0 {
		// レジスタ指定シフト。Rm/Rn に r15 を使うのは UNPREDICTABLE なので
		// PC+8/+12 の区別はしない（readReg の +8 のまま）。
		amount := c.readReg((word>>8)&0xF) & 0xFF
		return shiftReg(rm, shiftType, amount, carryIn)
	}
	return shiftImm(rm, shiftType, (word>>7)&0x1F, carryIn)
}

// データ処理オペコード（bits 24:21）。
const (
	opAND = iota
	opEOR
	opSUB
	opRSB
	opADD
	opADC
	opSBC
	opRSC
	opTST
	opTEQ
	opCMP
	opCMN
	opORR
	opMOV
	opBIC
	opMVN
)

func execDataProc(c *Core, word uint32) error {
	op := (word >> 21) & 0xF
	sBit := word&(1<<20) != 0
	rn := (word >> 16) & 0xF
	rd := (word >> 12) & 0xF

	opnd2, shCarry := c.dataProcOperand(word)
	rnVal := c.readReg(rn)

	var (
		res        uint32
		cf, vf     bool
		arithmetic bool // 加減算系なら true（C/V の由来が違う）
	)
	cin := uint32(0)
	if c.cpsr.C() {
		cin = 1
	}
	switch op {
	case opAND, opTST:
		res = rnVal & opnd2
	case opEOR, opTEQ:
		res = rnVal ^ opnd2
	case opSUB, opCMP:
		res, cf, vf = addWithCarry(rnVal, ^opnd2, 1)
		arithmetic = true
	case opRSB:
		res, cf, vf = addWithCarry(opnd2, ^rnVal, 1)
		arithmetic = true
	case opADD, opCMN:
		res, cf, vf = addWithCarry(rnVal, opnd2, 0)
		arithmetic = true
	case opADC:
		res, cf, vf = addWithCarry(rnVal, opnd2, cin)
		arithmetic = true
	case opSBC:
		res, cf, vf = addWithCarry(rnVal, ^opnd2, cin)
		arithmetic = true
	case opRSC:
		res, cf, vf = addWithCarry(opnd2, ^rnVal, cin)
		arithmetic = true
	case opORR:
		res = rnVal | opnd2
	case opMOV:
		res = opnd2
	case opBIC:
		res = rnVal &^ opnd2
	case opMVN:
		res = ^opnd2
	}

	isTest := op >= opTST && op <= opCMN

	if sBit {
		if rd == 15 && !isTest {
			// 例外復帰イディオム（MOVS pc, lr / SUBS pc, lr, #4）:
			// フラグ更新ではなく CPSR ← SPSR。
			if b := c.curBank(); b != bankUsr {
				c.setCPSR(c.spsr[b])
			}
			// TODO: usr/sys モードでの S=1, rd=15 は UNPREDICTABLE。今は CPSR 据え置き。
			if c.cpsr.T() {
				c.regs[15] = res &^ 1
			} else {
				c.regs[15] = res &^ 3
			}
			return nil
		}
		p := c.cpsr.SetNZ(res)
		if arithmetic {
			p = p.set(FlagC, cf).set(FlagV, vf)
		} else {
			p = p.set(FlagC, shCarry) // 論理系: C はシフタキャリー、V 不変
		}
		c.cpsr = p
	}

	if !isTest {
		c.writeReg(rd, res)
	}
	return nil
}
