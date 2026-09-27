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
	dev        Device // MMIO 領域なら non-nil
}

// Bus は物理アドレス空間。cpu.Memory を実装する。
// 領域数は少数（数個）の想定なので線形探索で十分。
// TODO: 性能が問題になったらページテーブル化する。
type Bus struct {
	regions []region
}

var _ cpu.Memory = (*Bus)(nil)

func New() *Bus { return &Bus{} }

// MapRAM は base から size バイトの RAM を確保して配置する。
func (b *Bus) MapRAM(name string, base, size uint32) error {
	return b.add(region{base: base, size: size, name: name, ram: make([]byte, size)})
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
	b.regions = append(b.regions, r)
	return nil
}

// RAM は base を含む RAM 領域のスライスと領域内オフセットを返す（ローダー用）。
func (b *Bus) RAM(addr uint32) ([]byte, uint32, bool) {
	for i := range b.regions {
		r := &b.regions[i]
		if r.ram != nil && addr >= r.base && addr < r.base+r.size {
			return r.ram, addr - r.base, true
		}
	}
	return nil, 0, false
}

func (b *Bus) find(addr uint32) *region {
	for i := range b.regions {
		r := &b.regions[i]
		if addr >= r.base && addr-r.base < r.size {
			return r
		}
	}
	return nil
}

func (b *Bus) read(addr uint32, size int) (uint32, error) {
	r := b.find(addr)
	if r == nil || int(addr-r.base)+size > int(r.size) {
		return 0, &BusError{Addr: addr}
	}
	off := addr - r.base
	if r.ram != nil {
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
	off := addr - r.base
	if r.ram != nil {
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
