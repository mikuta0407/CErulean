package arm

import "encoding/binary"

// デコードキャッシュとブロック実行（性能対策。ユーザー確認済み 2026-09）。
//
// 物理 RAM ページ（4KB）ごとに、デコード済みの ARM 命令（Instr）の表を持つ。
// 実行中のページ（cur）とその仮想ページ（curVA）を覚えておき、PC が同じ
// ページにあれば TLB を引かず表から命令を取る。MMU の変換世代（codeGen）が
// 上がると MMU が SetGenHook で curVA を無効にする。Decode は命令語だけの純関数なので、物理ページ単位で
// 共有してよい（decode.go のコメント参照）。
//
// 正しさ（1 命令ずつフェッチ・デコードしていた頃と全状態が一致すること）:
//   - 変換: MMU は TLB の全無効化・エントリの詰め替え・特権状態や FCSE PID の
//     変化で世代を上げる（mmu/code.go）。世代が同じ間は、同じ VA のフェッチは
//     同じ TLB エントリにヒットし、TLB の状態も変えない（= 省略してよい）。
//     ページに入るとき（enterCodePage）は TLB ヒットのときだけキャッシュを使い、
//     ミスなら通常のフェッチで TLB を埋める。
//   - 書き換え: デコードしたページは MMU に印を付け（MarkCode）、そのページへの
//     書き込みで invalidateCode が呼ばれる。表を捨てるので、次のフェッチは
//     書き換え後の命令語を読み直す（自己書き換えコード・DLL のロードで同じ
//     物理ページが再利用される場合も含む）。
//   - Thumb は未対応（1 命令ずつ Read16。WM5 の実行中にはほぼ現れない）。
//
// ここで持つものはすべて派生情報で、スナップショットには保存しない。

// CodeMemory はデコードキャッシュに必要な MMU の機能（mmu.MMU が実装する）。
// Memory がこれを実装していなければキャッシュは使わない。
type CodeMemory interface {
	// CodePage は va を含むページの物理先頭と RAM の実体を返す。TLB ヒットで
	// RAM のときだけ ok=true で、MMU の状態を変えない。
	CodePage(va uint32) (pa uint32, ram []byte, ok bool)
	// CodeGen は変換世代へのポインタ。
	CodeGen() *uint64
	// MarkCode は物理ページ pa をコードとして書き込み検出の対象にする。
	MarkCode(pa uint32)
	// SetCodeInvalidator は印を付けたページへの書き込みの通知先を設定する。
	SetCodeInvalidator(func(pa uint32))
	// SetGenHook は世代が上がるたびに呼ぶ関数を設定する（CPU は実行中の
	// ページの記憶を捨てる。毎命令の世代比較を省くため）。
	SetGenHook(func())
}

// RunMemory は LDM/STM の高速化に使う任意の Memory 機能（mmu.MMU が実装する）。
// RAMRun は va から nbytes バイト（1 ページ内）を直接読み書きしてよい RAM の
// 範囲として返す。TLB ヒット等の条件を満たすときだけ ok=true で、状態を変えない。
type RunMemory interface {
	RAMRun(va, nbytes uint32, write bool) ([]byte, bool)
}

// codePage は物理 4KB ページ 1 枚分のデコード済み ARM 命令。
type codePage struct {
	pa     uint32
	ram    []byte
	arm    [1024]Instr // exec == nil は未デコード
	marked bool        // MMU に MarkCode 済み（書き込みで外れる）
}

// noCodeGen は code が無い場合の世代（curVA が常に不一致なので読まれるだけ）。
var noCodeGen uint64

func (c *Core) initCodeCache() {
	c.runs, _ = c.mem.(RunMemory)
	c.codeGen = &noCodeGen
	if cm, ok := c.mem.(CodeMemory); ok {
		c.code = cm
		c.codeGen = cm.CodeGen()
		cm.SetCodeInvalidator(c.invalidateCode)
		cm.SetGenHook(func() { c.curVA = 1 })
	}
	c.resetCodeCache()
}

// resetCodeCache はデコードキャッシュを空にする（リセット・スナップショット
// 復元時。MMU 側も印を全部外す）。
func (c *Core) resetCodeCache() {
	c.pages = map[uint32]*codePage{}
	c.vpages = [1 << vpageBits]vpageEnt{}
	for i := range c.vpages {
		c.vpages[i].va = 1 // 無効
	}
	c.cur = nil
	c.curVA = 1 // 4KB 境界でない値 = 無効
}

// vpageEnt は仮想ページ → デコード済みページの対応（世代つき）。関数呼び出し
// などでページをまたぐたびに TLB とマップを引かないためのもの。同じ世代の
// 間は、その仮想ページの TLB エントリが残っている（CPU が覚えたエントリの
// 詰め替えで世代が上がる。mmu/code.go）ので、CodePage を呼んだのと同じ結果になる。
type vpageEnt struct {
	va  uint32
	gen uint64
	p   *codePage
}

const vpageBits = 6

// enterCodePage は PC が別のページに移った（または世代が変わった）ときに、
// そのページのキャッシュを引き当てる。キャッシュできなければ nil。
func (c *Core) enterCodePage(pc uint32) *codePage {
	if c.code == nil {
		return nil
	}
	va := pc &^ 0xFFF
	v := &c.vpages[(pc>>12)&(1<<vpageBits-1)]
	if v.va == va && v.gen == *c.codeGen {
		c.cur, c.curVA = v.p, va
		return v.p
	}
	pa, ram, ok := c.code.CodePage(pc)
	if !ok {
		c.curVA = 1
		return nil
	}
	p := c.pages[pa>>12]
	if p == nil {
		p = &codePage{pa: pa, ram: ram}
		c.pages[pa>>12] = p
	}
	c.cur, c.curVA = p, va
	*v = vpageEnt{va: va, gen: *c.codeGen, p: p}
	return p
}

// decodeCached は cur の pc の命令をデコードして表に入れる。
func (c *Core) decodeCached(pc uint32) Instr {
	p := c.cur
	if !p.marked {
		// 表に載せる前に書き込み検出を有効にする。
		c.code.MarkCode(p.pa)
		p.marked = true
	}
	word := binary.LittleEndian.Uint32(p.ram[pc&0xFFC:])
	in := Decode(word)
	p.arm[(pc>>2)&0x3FF] = in
	return in
}

// invalidateCode は MMU からの通知: 物理ページ pa に書き込みがある。
// デコード結果を捨てる（書き込みはこの直後に行われる）。
func (c *Core) invalidateCode(pa uint32) {
	if p := c.pages[pa>>12]; p != nil {
		clear(p.arm[:])
		p.marked = false
	}
}

// stopRun は実行中の Run を今の命令で終わらせる（spinHint を立てた分岐命令が呼ぶ）。
func (c *Core) stopRun() { c.runBudget = 0 }

// Run は最大 budget 命令を実行し、実行した命令数を返す（ブロック実行）。
// 次の場合は途中で戻る:
//   - エラー（エラーを起こした命令も 1 命令と数える。Step と同じ）
//   - ポーリングループ先頭への後方分岐（spinHint。実行ループがアイドル
//     スキップを試せるように）
//   - 実行中に LimitRun で上限が下げられた
//
// 1 命令ずつ Step した場合と状態は同一。実行中に周辺機器から「今まで何命令
// 実行したか」を知る必要があるとき（仮想時間の同期）は Executed を使う。
func (c *Core) Run(budget uint64) (uint64, error) {
	c.runN, c.runBudget = 0, budget
	// ループの本体は 1 命令の実行（旧 Step）。関数呼び出しを避けるため展開
	// してある。runN は命令を終えてから増やす（実行中の命令からデバイスが
	// Executed を読むと、終えた命令数が返る）。
	for c.runN < c.runBudget {
		if c.hist != nil {
			c.recordHistory()
		}
		// 割り込みは命令境界で受け付ける。FIQ が IRQ より優先（ARM ARM A2.6）。
		// 復帰先は「実行されなかった命令」なので LR = その PC+4
		// （ハンドラは SUBS pc, lr, #4 で戻る）。
		if c.irq || c.fiq {
			if c.fiq && c.cpsr&FlagF == 0 {
				c.enterException(VecFIQ, ModeFiq, c.regs[15]+4)
				c.runN++
				continue
			}
			if c.irq && c.cpsr&FlagI == 0 {
				c.enterException(VecIRQ, ModeIrq, c.regs[15]+4)
				c.runN++
				continue
			}
		}
		if c.cpsr&FlagT != 0 {
			err := c.stepThumb()
			c.runN++
			if err != nil {
				return c.runN, err
			}
			continue
		}

		pc := c.regs[15]
		var in Instr
		if pc&^0xFFF == c.curVA {
			// 同じページを実行中で変換も変わっていない: TLB を引かずに
			// デコード済みの命令を取る。
			in = c.cur.arm[(pc>>2)&0x3FF]
			if in.exec == nil {
				in = c.decodeCached(pc)
			}
		} else {
			var taken bool
			var err error
			if in, taken, err = c.fetchSlow(pc); err != nil || taken {
				c.runN++
				if err != nil {
					return c.runN, err
				}
				continue
			}
		}
		word := in.Word

		// 実行中は regs[15] = PC+4 にしておく。オペランドとして r15 を読むときは
		// readReg が さらに +4 して「PC+8」（パイプラインの見え方）を返す。
		c.regs[15] = pc + 4

		if cond := word >> 28; cond != 0xE && !condTable[cond<<4|uint32(c.cpsr)>>28] {
			c.runN++ // 条件不成立: 何もせず次の命令へ
			continue
		}
		if err := in.exec(c, word); err != nil {
			if derr := c.deliverExecError(err, pc, 4); derr != nil {
				if ue, ok := derr.(*UndefinedError); ok {
					ue.PC = pc
					ue.Word = word
				}
				c.regs[15] = pc
				c.runN++
				return c.runN, derr
			}
		}
		// ポーリングループ先頭への後方分岐（spinHint）は、分岐命令が
		// runBudget を 0 にしてループを抜けさせる（毎命令の判定を省くため）。
		c.runN++
	}
	return c.runN, nil
}

// Executed は実行中の Run で実行を終えた命令数（実行中の命令は含まない）。
func (c *Core) Executed() uint64 { return c.runN }

// LimitRun は実行中の Run を、通算 n 命令（Executed の値）で止めるよう上限を
// 下げる。デバイスのイベント期限が早まったときに実行ループが呼ぶ。
func (c *Core) LimitRun(n uint64) {
	if n < c.runBudget {
		c.runBudget = n
	}
}
