// Package bus は物理アドレス空間を実装する。RAM 領域と MMIO 領域を登録し、
// アクセスをディスパッチする。SoC 固有のアドレスは持たない（machine が登録する）。
package bus

import (
	"encoding/binary"
	"fmt"

	"github.com/mikuta0407/cerulean/cpu"
)

// Device は MMIO ハンドラ。off は領域先頭からのオフセット、size は 1/2/4。
// 周辺機器レジスタは副作用（FIFO 進行など）があるため、サイズ情報ごと渡す。
type Device interface {
	Read(off uint32, size int) uint32
	Write(off uint32, size int, v uint32)
}

// BusError は未マップアドレスへのアクセス。実機ならバスフォールト相当。
type BusError struct {
	Addr  uint32
	Write bool
}

func (e *BusError) Error() string {
	kind := "read"
	if e.Write {
		kind = "write"
	}
	return fmt.Sprintf("bus: %s to unmapped address %08X", kind, e.Addr)
}

type region struct {
	base, size uint32
	name       string
	ram        []byte // RAM 領域なら non-nil
	ramMask    uint32 // 0 以外なら部分デコード: オフセットを &= ramMask して ram に届く
	dev        Device // MMIO 領域なら non-nil
}

// Bus は物理アドレス空間。cpu.Memory を実装する。
//
// 領域検索は全アクセスで走るので、4KB ページ単位の引き表（pages）で
// O(1) にしている（線形探索だとプロファイルの 1 割近くを占めた）。
// 4KB に揃っていない領域が一部だけ覆うページは pagePartial にして
// 線形探索に落とす（現状の構成には無いが、正しさのための保険）。
type Bus struct {
	regions []region
	pages   []uint16 // PA>>12 → 領域番号+1（0 = 未マップ、pagePartial = 線形探索）

	// watches はデバッグ用のアクセス監視範囲。hasWatch が false の間は
	// RAM アクセスの経路に bool 判定 1 回ぶんしかコストを足さない。
	watches  []watchRange
	hasWatch bool
	watchFn  WatchFunc
}

// WatchFunc は監視範囲へのアクセス通知。region は領域名、v は
// 読み出し値または書き込み値。デバッグ（CLI のアクセスログ）用。
type WatchFunc func(region string, addr uint32, size int, v uint32, write bool)

type watchRange struct{ lo, hi uint32 } // 両端を含む

// AddWatch は物理アドレス lo〜hi（両端含む）へのアクセスを fn に通知させる。
// fn は全範囲で共通（最後に渡したもの）。
func (b *Bus) AddWatch(lo, hi uint32, fn WatchFunc) {
	b.watches = append(b.watches, watchRange{lo, hi})
	b.watchFn = fn
	b.hasWatch = true
}

func (b *Bus) notify(r *region, addr uint32, size int, v uint32, write bool) {
	for _, w := range b.watches {
		if addr >= w.lo && addr <= w.hi {
			b.watchFn(r.name, addr, size, v, write)
			return
		}
	}
}

var _ cpu.Memory = (*Bus)(nil)

const (
	pageShift   = 12
	pagePartial = 0xFFFF
)

func New() *Bus { return &Bus{pages: make([]uint16, 1<<(32-pageShift))} }

// MapRAM は base から size バイトの RAM を確保して配置する。
func (b *Bus) MapRAM(name string, base, size uint32) error {
	return b.add(region{base: base, size: size, name: name, ram: make([]byte, size)})
}

// MapRAMMirror は window バイトの窓に size バイトの RAM を折り返しで見せる。
// 実機のメモリコントローラはバンク窓（例: S3C2410 は 128MB）に対して
// 実装 RAM が小さいとき、アドレス線の部分デコードでエイリアスが生じる。
// OS のメモリサイズ検出はこの折り返しに依存するので再現する。
// size は 2 の冪であること。
func (b *Bus) MapRAMMirror(name string, base, window, size uint32) error {
	if size == 0 || size&(size-1) != 0 || window < size {
		return fmt.Errorf("bus: MapRAMMirror %s: size %X must be a power of two <= window %X", name, size, window)
	}
	return b.add(region{base: base, size: window, name: name, ram: make([]byte, size), ramMask: size - 1})
}

// MapMMIO は base から size バイトを MMIO として dev に接続する。
func (b *Bus) MapMMIO(name string, base, size uint32, dev Device) error {
	return b.add(region{base: base, size: size, name: name, dev: dev})
}

func (b *Bus) add(r region) error {
	for _, x := range b.regions {
		if r.base < x.base+x.size && x.base < r.base+r.size {
			return fmt.Errorf("bus: region %s (%08X+%X) overlaps %s (%08X+%X)",
				r.name, r.base, r.size, x.name, x.base, x.size)
		}
	}
	if len(b.regions) >= pagePartial-1 {
		return fmt.Errorf("bus: too many regions")
	}
	b.regions = append(b.regions, r)
	idx := uint16(len(b.regions)) // 領域番号+1
	end := uint64(r.base) + uint64(r.size)
	for p := uint64(r.base) >> pageShift; p<<pageShift < end; p++ {
		full := p<<pageShift >= uint64(r.base) && (p+1)<<pageShift <= end
		if full && b.pages[p] == 0 {
			b.pages[p] = idx
		} else {
			b.pages[p] = pagePartial
		}
	}
	return nil
}

// RAMPage は PA を含む 4KB ページが RAM なら、そのページの実体（長さ 4KB の
// スライス）を返す。MMU のソフト TLB が RAM を直接読み書きする fast path 用。
// 監視（AddWatch）が有効な間は nil を返して必ず bus を経由させる
// （fast path だと監視が素通りになるため）。
func (b *Bus) RAMPage(pa uint32) []byte {
	if b.hasWatch {
		return nil
	}
	r := b.find(pa)
	if r == nil || r.ram == nil {
		return nil
	}
	page := pa &^ (1<<pageShift - 1)
	if page < r.base || uint64(page)+1<<pageShift > uint64(r.base)+uint64(r.size) {
		return nil // ページが領域をはみ出す
	}
	off := page - r.base
	if r.ramMask != 0 {
		off &= r.ramMask
	}
	if int(off)+1<<pageShift > len(r.ram) {
		return nil
	}
	return r.ram[off : off+1<<pageShift : off+1<<pageShift]
}

// RAM は base を含む RAM 領域のスライスと領域内オフセットを返す（ローダー用）。
func (b *Bus) RAM(addr uint32) ([]byte, uint32, bool) {
	for i := range b.regions {
		r := &b.regions[i]
		if r.ram != nil && addr >= r.base && addr < r.base+r.size {
			off := addr - r.base
			if r.ramMask != 0 {
				off &= r.ramMask
			}
			return r.ram, off, true
		}
	}
	return nil, 0, false
}

func (b *Bus) find(addr uint32) *region {
	switch i := b.pages[addr>>pageShift]; i {
	case 0:
		return nil
	case pagePartial:
		return b.findSlow(addr)
	default:
		return &b.regions[i-1]
	}
}

func (b *Bus) findSlow(addr uint32) *region {
	for i := range b.regions {
		r := &b.regions[i]
		if addr >= r.base && addr-r.base < r.size {
			return r
		}
	}
	return nil
}

func (b *Bus) read(addr uint32, size int) (uint32, error) {
	if b.hasWatch {
		v, err := b.read1(addr, size)
		if err == nil {
			b.notify(b.find(addr), addr, size, v, false)
		}
		return v, err
	}
	return b.read1(addr, size)
}

func (b *Bus) read1(addr uint32, size int) (uint32, error) {
	r := b.find(addr)
	if r == nil || int(addr-r.base)+size > int(r.size) {
		return 0, &BusError{Addr: addr}
	}
	off := addr - r.base
	if r.ram != nil {
		if r.ramMask != 0 {
			off &= r.ramMask
			if int(off)+size > len(r.ram) {
				// 折り返し境界をまたぐアクセス。実機ならバイト単位で折り返すが、
				// アラインされたアクセスでは起きないので異常として報告する。
				return 0, &BusError{Addr: addr}
			}
		}
		switch size {
		case 1:
			return uint32(r.ram[off]), nil
		case 2:
			return uint32(binary.LittleEndian.Uint16(r.ram[off:])), nil
		default:
			return binary.LittleEndian.Uint32(r.ram[off:]), nil
		}
	}
	return r.dev.Read(off, size), nil
}

func (b *Bus) write(addr uint32, size int, v uint32) error {
	r := b.find(addr)
	if r == nil || int(addr-r.base)+size > int(r.size) {
		return &BusError{Addr: addr, Write: true}
	}
	if b.hasWatch {
		b.notify(r, addr, size, v, true)
	}
	off := addr - r.base
	if r.ram != nil {
		if r.ramMask != 0 {
			off &= r.ramMask
			if int(off)+size > len(r.ram) {
				return &BusError{Addr: addr, Write: true} // read 側と同じ扱い
			}
		}
		switch size {
		case 1:
			r.ram[off] = uint8(v)
		case 2:
			binary.LittleEndian.PutUint16(r.ram[off:], uint16(v))
		default:
			binary.LittleEndian.PutUint32(r.ram[off:], v)
		}
		return nil
	}
	r.dev.Write(off, size, v)
	return nil
}

func (b *Bus) Read8(addr uint32) (uint8, error) {
	v, err := b.read(addr, 1)
	return uint8(v), err
}
func (b *Bus) Read16(addr uint32) (uint16, error) {
	v, err := b.read(addr, 2)
	return uint16(v), err
}
func (b *Bus) Read32(addr uint32) (uint32, error) { return b.read(addr, 4) }

func (b *Bus) Write8(addr uint32, v uint8) error   { return b.write(addr, 1, uint32(v)) }
func (b *Bus) Write16(addr uint32, v uint16) error { return b.write(addr, 2, uint32(v)) }
func (b *Bus) Write32(addr uint32, v uint32) error { return b.write(addr, 4, v) }
