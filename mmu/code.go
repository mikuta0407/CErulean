package mmu

// 命令フェッチの高速化の支援（CPU のデコードキャッシュ用。ユーザー確認済み
// 2026-09）。
//
// CPU（cpu/arm）は物理 RAM ページ単位でデコード済みの命令を持ち、同じ
// ページを実行している間は TLB を引かずにそこから命令を取る。MMU は次を
// 提供する:
//
//   - CodePage: 実行を始めるページの変換（TLB ヒットのときだけ。ミスなら CPU は
//     通常のフェッチでこの命令を取り、TLB が埋まる。TLB の状態の変化は
//     1 命令ずつ Fetch32 していた頃と同一になる）。
//   - CodeGen: 「CPU が覚えたページの変換がまだ有効か」の世代番号。変換・権限・
//     TLB の中身が変わり得る操作（TLB の全無効化、特権状態・FCSE PID の変化、
//     CPU が覚えているエントリの詰め替え）で増やし、SetGenHook の関数を呼ぶ
//     （CPU は実行中のページの記憶をそこで捨てるので、毎命令の比較が要らない）。詰め替えで世代を上げるのは、コードページの
//     TLB エントリがデータアクセスで追い出された場合に、CPU が次のフェッチで
//     元どおり TLB を埋め直すため（TLB の状態を通常のフェッチと一致させる）。
//     CodePage で渡したエントリに watched の印を付け、そのエントリの詰め替え
//     だけで世代を上げる（関係ないスロットの詰め替えでは上げない）。
//   - コードページの書き込み検出（書き込み保護方式）: CPU がデコードした
//     物理ページは codePages に印を付け、そのページを指す TLB エントリの
//     wram（直接書き込み用の実体）を外す。そのページへのストアだけ遅い経路に
//     回り、そこで CPU に無効化を通知してから書く。普通のストアには追加の
//     判定が入らない。同じ物理ページを別の仮想アドレスが指していても物理ページ
//     単位で管理するので漏れない。
//
// codePages・wram・世代は、デコードキャッシュと同じく実行を速くするための
// 派生情報で、スナップショットには保存しない（復元時は空から作り直す）。

// CodePage は命令フェッチ用に va を含むページの変換を TLB から引く。
// TLB ヒットかつ RAM で、フェッチ猶予中でないときだけ ok=true。状態は変えない。
func (m *MMU) CodePage(va uint32) (pa uint32, ram []byte, ok bool) {
	if m.fetchGrace != 0 {
		return 0, nil, false
	}
	e := m.lookup(va, m.permR)
	if e == nil || e.ram == nil {
		return 0, nil, false
	}
	e.watched = true
	return e.pa, e.ram, true
}

// CodeGen は変換の世代番号へのポインタ。
func (m *MMU) CodeGen() *uint64 { return &m.gen }

// SetGenHook は世代が上がるたびに呼ぶ関数を設定する。
func (m *MMU) SetGenHook(f func()) { m.onGen = f }

// bumpGen は世代を上げて CPU に知らせる。
func (m *MMU) bumpGen() {
	m.gen++
	if m.onGen != nil {
		m.onGen()
	}
}

// SetCodeInvalidator は、印を付けた物理ページへの書き込みを通知する先を
// 設定する（CPU がそのページのデコード結果を捨てる）。
func (m *MMU) SetCodeInvalidator(f func(pa uint32)) { m.onCodeWrite = f }

// MarkCode は物理ページ pa（4KB 境界）をデコード済み（コード）として印を
// 付け、以後そのページへの書き込みを検出できるようにする。
func (m *MMU) MarkCode(pa uint32) {
	pn := pa >> 12
	if m.codePages[pn>>6]&(1<<(pn&63)) != 0 {
		return
	}
	m.codePages[pn>>6] |= 1 << (pn & 63)
	m.codeMarks++
	for i := range m.tlb {
		if e := &m.tlb[i]; e.tag&tlbValid != 0 && e.pa == pa {
			e.wram = nil
		}
	}
}

// isCode は物理アドレスを含むページに印があるか。
func (m *MMU) isCode(pa uint32) bool {
	pn := pa >> 12
	return m.codePages[pn>>6]&(1<<(pn&63)) != 0
}

// codeWrite は印の付いたページへの書き込みの直前に呼ぶ。印を外して直接
// 書き込みを戻し、CPU にデコード結果を捨てさせる。
func (m *MMU) codeWrite(pa uint32) {
	page := pa &^ 0xFFF
	pn := page >> 12
	m.codePages[pn>>6] &^= 1 << (pn & 63)
	m.codeWrites++
	for i := range m.tlb {
		if e := &m.tlb[i]; e.tag&tlbValid != 0 && e.pa == page {
			e.wram = e.ram
		}
	}
	if m.onCodeWrite != nil {
		m.onCodeWrite(page)
	}
}

// checkCodeWrite は遅い経路の書き込み（TLB ミス・MMIO・監視中）で、書き込み先が
// コードページなら通知する。
func (m *MMU) checkCodeWrite(pa uint32) {
	if m.isCode(pa) {
		m.codeWrite(pa)
	}
}

// resetCode はコードページの印を全部外す（スナップショット復元時など。
// CPU 側もデコードキャッシュを捨てる前提）。
func (m *MMU) resetCode() {
	clear(m.codePages)
	for i := range m.tlb {
		m.tlb[i].wram = m.tlb[i].ram
	}
	m.bumpGen()
}

// CodeStats はコードページの印付け・書き込み検出の回数（性能調査用）。
func (m *MMU) CodeStats() (marks, writes uint64) { return m.codeMarks, m.codeWrites }
