package mmu

import "encoding/binary"

// ソフト TLB（性能対策。ユーザー確認済み 2026-09）。
//
// 変換結果を 4KB ページ単位で直接マップ方式のキャッシュに持つ。毎アクセスの
// テーブルウォーク（L1/L2 の物理読み出し 1〜2 回）を省き、変換先が RAM なら
// ページ実体の []byte を直接読み書きして bus も経由しない。
//
// 正しさの根拠: 実機（ARM920T）にも TLB があり、ページテーブルを書き換えた
// OS は CP15 c8 で TLB を無効化する義務がある（ARM ARM B3.7）。したがって
// 「c8 操作まで古い変換が残る」のは実機と同じ振る舞い。エミュレータの TLB は
// 実機より大きい（エントリ数・置換規則が違う）が、無効化を怠るソフトは実機でも
// 動作が不定なので問題にしない。フォルトになる変換はキャッシュしない
// （実機の TLB もフォルト記述子は保持しない）ので、新規マッピングの追加は
// 無効化なしでも即座に見える。
//
// 全無効化の契機: c1（M/S/R ビットが権限判定に効く）・c2（TTB）・c3（DACR）・
// c8（TLB 操作。単一エントリ無効化も安全側で全無効化にする）の書き込み。
// c13（FCSE PID）はタグを MVA にしているので無効化不要。
//
// キャッシュしない変換（毎回ウォークする）:
//   - tiny ページ（1KB）と、サブページ（1KB）ごとに AP が異なる小ページ
//     （4KB 内で権限が一様でないため）
//   - MMU 有効化直後のフェッチ猶予中の命令フェッチ（Fetch32 参照）
const (
	tlbBits  = 10
	tlbSize  = 1 << tlbBits
	tlbValid = 1 << 31 // tag の有効ビット（MVA>>12 は 20 ビットなので衝突しない）
)

// 権限ビット（tlbEntry.perm）。特権/ユーザー × 読み/書き の 4 通りを
// フィル時にまとめて計算しておき、モード切替で TLB を捨てずに済ませる。
const (
	permPrivR = 1 << iota
	permPrivW
	permUserR
	permUserW
)

type tlbEntry struct {
	tag  uint32 // MVA>>12 | tlbValid
	pa   uint32 // 物理ページ先頭
	perm uint8
	ram  []byte // 物理ページが RAM ならその実体（4KB）。MMIO なら nil
}

// RAMPager は物理空間が RAM ページの実体を渡せる場合に実装する任意 interface
// （bus.Bus が実装）。nil を返したページは常に phys 経由でアクセスする。
type RAMPager interface {
	RAMPage(pa uint32) []byte
}

func (m *MMU) flushTLB() {
	for i := range m.tlb {
		m.tlb[i].tag = 0
		m.tlb[i].ram = nil
	}
}

// updatePermMask は現在の特権状態に対応する読み/書き権限ビットを選ぶ。
func (m *MMU) updatePermMask() {
	if m.priv {
		m.permR, m.permW = permPrivR, permPrivW
	} else {
		m.permR, m.permW = permUserR, permUserW
	}
}

// mva は FCSE 適用後のアドレス。
func (m *MMU) mva(va uint32) uint32 {
	if va < 0x02000000 {
		return va | m.pid
	}
	return va
}

// lookup は TLB を引き、ヒットかつ権限があればエントリを返す。
// ミスや権限なしは nil（slow path でウォークし、正しいフォルトを作る）。
func (m *MMU) lookup(va uint32, need uint8) *tlbEntry {
	mva := m.mva(va)
	e := &m.tlb[(mva>>12)&(tlbSize-1)]
	if e.tag == mva>>12|tlbValid && e.perm&need != 0 {
		return e
	}
	return nil
}

// fill は slow path で変換が成功した後に、そのページのエントリを作る。
// ページ全体の権限を 4 通り求めるため、ウォークをもう一度行う
// （ミス時だけなのでコストは問題にならない）。
func (m *MMU) fill(va uint32) {
	mva := m.mva(va)
	pa, perm, ok := m.pageInfo(mva)
	if !ok {
		return
	}
	e := &m.tlb[(mva>>12)&(tlbSize-1)]
	e.tag = mva>>12 | tlbValid
	e.pa = pa
	e.perm = perm
	e.ram = nil
	if p, ok := m.phys.(RAMPager); ok {
		e.ram = p.RAMPage(pa)
	}
}

// pageInfo は MVA を含む 4KB ページの物理先頭と、4 通りの権限を求める。
// 4KB 内で変換・権限が一様でないマッピングやフォルトは ok=false。
func (m *MMU) pageInfo(mva uint32) (pa uint32, perm uint8, ok bool) {
	ctrl := m.ctrl
	if ctrl&ctrlM == 0 {
		return mva &^ 0xFFF, permPrivR | permPrivW | permUserR | permUserW, true
	}
	l1, err := m.phys.Read32((m.ttb &^ 0x3FFF) | (mva>>20)<<2)
	if err != nil {
		return 0, 0, false
	}
	domain := uint8((l1 >> 5) & 0xF)
	var ap uint32
	switch l1 & 3 {
	case descSection:
		pa = l1&0xFFF00000 | mva&0x000FF000
		ap = (l1 >> 10) & 3
	case descCoarse:
		l2, err := m.phys.Read32((l1 &^ 0x3FF) | ((mva>>12)&0xFF)<<2)
		if err != nil {
			return 0, 0, false
		}
		switch l2 & 3 {
		case descLarge:
			pa = l2&0xFFFF0000 | mva&0x0000F000
			ap = (l2 >> (4 + 2*((mva>>14)&3))) & 3
		case descSmall:
			// 4 サブページの AP が揃っているときだけキャッシュする。
			aps := (l2 >> 4) & 0xFF
			ap = aps & 3
			if aps != ap*0x55 {
				return 0, 0, false
			}
			pa = l2 & 0xFFFFF000
		default:
			return 0, 0, false
		}
	default:
		// 細テーブル（tiny ページ 1KB を含み得る）とフォルトはキャッシュしない。
		return 0, 0, false
	}
	switch (m.dacr >> (uint(domain) * 2)) & 3 {
	case 3: // マネージャ: 権限チェックなし
		return pa, permPrivR | permPrivW | permUserR | permUserW, true
	case 1: // クライアント: AP に従う
		for _, c := range []struct {
			priv, write bool
			bit         uint8
		}{
			{true, false, permPrivR}, {true, true, permPrivW},
			{false, false, permUserR}, {false, true, permUserW},
		} {
			if apAllowed(ctrl, ap, c.write, c.priv) {
				perm |= c.bit
			}
		}
		return pa, perm, true
	default: // ドメインフォルト: キャッシュしない
		return 0, 0, false
	}
}

// ---- fast path 付きアクセス ----
// アラインされていないアクセスは slow path に任せる（ページ境界跨ぎや
// 端数の扱いを bus と同一にするため。CPU は通常アライン済みで発行する）。

func (m *MMU) Read8(a uint32) (uint8, error) {
	if e := m.lookup(a, m.permR); e != nil {
		if e.ram != nil {
			return e.ram[a&0xFFF], nil
		}
		return m.phys.Read8(e.pa | a&0xFFF)
	}
	pa, err := m.translateFill(a, false)
	if err != nil {
		return 0, err
	}
	return m.phys.Read8(pa)
}

func (m *MMU) Read16(a uint32) (uint16, error) {
	if a&1 == 0 {
		if e := m.lookup(a, m.permR); e != nil {
			if e.ram != nil {
				return binary.LittleEndian.Uint16(e.ram[a&0xFFF:]), nil
			}
			return m.phys.Read16(e.pa | a&0xFFF)
		}
	}
	pa, err := m.translateFill(a, false)
	if err != nil {
		return 0, err
	}
	return m.phys.Read16(pa)
}

func (m *MMU) Read32(a uint32) (uint32, error) {
	if a&3 == 0 {
		if e := m.lookup(a, m.permR); e != nil {
			if e.ram != nil {
				return binary.LittleEndian.Uint32(e.ram[a&0xFFF:]), nil
			}
			return m.phys.Read32(e.pa | a&0xFFF)
		}
	}
	pa, err := m.translateFill(a, false)
	if err != nil {
		return 0, err
	}
	return m.phys.Read32(pa)
}

func (m *MMU) Write8(a uint32, v uint8) error {
	if e := m.lookup(a, m.permW); e != nil {
		if e.ram != nil {
			e.ram[a&0xFFF] = v
			return nil
		}
		return m.phys.Write8(e.pa|a&0xFFF, v)
	}
	pa, err := m.translateFill(a, true)
	if err != nil {
		return err
	}
	return m.phys.Write8(pa, v)
}

func (m *MMU) Write16(a uint32, v uint16) error {
	if a&1 == 0 {
		if e := m.lookup(a, m.permW); e != nil {
			if e.ram != nil {
				binary.LittleEndian.PutUint16(e.ram[a&0xFFF:], v)
				return nil
			}
			return m.phys.Write16(e.pa|a&0xFFF, v)
		}
	}
	pa, err := m.translateFill(a, true)
	if err != nil {
		return err
	}
	return m.phys.Write16(pa, v)
}

func (m *MMU) Write32(a uint32, v uint32) error {
	if a&3 == 0 {
		if e := m.lookup(a, m.permW); e != nil {
			if e.ram != nil {
				binary.LittleEndian.PutUint32(e.ram[a&0xFFF:], v)
				return nil
			}
			return m.phys.Write32(e.pa|a&0xFFF, v)
		}
	}
	pa, err := m.translateFill(a, true)
	if err != nil {
		return err
	}
	return m.phys.Write32(pa, v)
}

// translateFill は slow path の変換。成功したら TLB に載せる。
func (m *MMU) translateFill(a uint32, write bool) (uint32, error) {
	pa, err := m.translate(a, write)
	if err == nil {
		m.fill(a)
	}
	return pa, err
}
