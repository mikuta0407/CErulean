// Package smdk2410 は Samsung S3C2410 リファレンスボード（SMDK2410）相当の
// マシン構成。Microsoft Device Emulator がエミュレートするのもこの系統の
// ボードなので、WM5 エミュレータイメージの最初のターゲットとする。
package smdk2410

import (
	"fmt"
	"io"

	"github.com/mikuta0407/cerulean/bus"
	"github.com/mikuta0407/cerulean/cpu"
	"github.com/mikuta0407/cerulean/cpu/arm"
	"github.com/mikuta0407/cerulean/device/s3c2410"
	"github.com/mikuta0407/cerulean/loader"
	"github.com/mikuta0407/cerulean/machine"
	"github.com/mikuta0407/cerulean/mmu"
)

// S3C2410 の物理メモリマップ（データシート Figure 5-1）:
//   - 0x30000000: SDRAM（バンク 6）。SMDK2410/Device Emulator は 64MB。
//   - 0x50000000: UART（UART0/1/2 が 0x4000 間隔）
const (
	sdramBase = 0x30000000
	sdramSize = 64 * 1024 * 1024
	uart0Base = 0x50000000
)

// WinCE カーネルの仮想アドレスマッピング（OEMAddressTable 相当）。
// 仮定: VA 0x80000000〜 が PA 0x30000000〜（SDRAM）に対応する。
// TODO: 実イメージで検証する。本来は BSP の OEMAddressTable が定義するもので、
// 複数エントリ（周辺機器領域など）があり得る。
const kernelVABase = 0x80000000

type Machine struct {
	cpu *arm.Core
	bus *bus.Bus
	mmu *mmu.MMU

	entryPA uint32 // リセット時に飛ぶ物理アドレス
}

var _ machine.Machine = (*Machine)(nil)

// New は SMDK2410 相当のマシンを組み立てる。uartOut に UART0 の送信データが流れる。
func New(uartOut io.Writer) (*Machine, error) {
	b := bus.New()
	if err := b.MapRAM("sdram", sdramBase, sdramSize); err != nil {
		return nil, err
	}
	// UART0 のみ。レジスタ帯は 0x4000 だが実レジスタは先頭 0x2C バイト。
	if err := b.MapMMIO("uart0", uart0Base, 0x4000, s3c2410.NewUART(uartOut)); err != nil {
		return nil, err
	}
	// mmu.MMU は CPU から見たメモリ空間（cpu.Memory）と CP15（arm.Coprocessor）を兼ねる。
	m := mmu.New(b)
	return &Machine{
		cpu: arm.New(m, m),
		bus: b,
		mmu: m,
	}, nil
}

func (m *Machine) Name() string { return "smdk2410" }

func (m *Machine) CPU() cpu.CPU { return m.cpu }

// Bus は物理バス（デバッグ・テスト用）。
func (m *Machine) Bus() *bus.Bus { return m.bus }

// vaToPA はイメージ内アドレス（CE 仮想アドレス）をロード先物理アドレスに変換する。
// MMU 有効化前のロード時にだけ使う。
func vaToPA(va uint32) (uint32, error) {
	// 0x80000000〜0x9FFFFFFF（キャッシュあり）/ 0xA0000000〜0xBFFFFFFF（なし）は
	// 同じ物理にマップされるのが CE の流儀。下位 29 ビットをオフセットとして扱う。
	if va >= kernelVABase && va < 0xC0000000 {
		return sdramBase + (va & 0x1FFFFFFF), nil
	}
	// 既に物理アドレス（.nb0 を RAM 直指定でロードする場合など）ならそのまま。
	if va >= sdramBase && va < sdramBase+sdramSize {
		return va, nil
	}
	return 0, fmt.Errorf("smdk2410: no mapping for image address %08X", va)
}

func (m *Machine) LoadImage(img *loader.Image) error {
	for _, seg := range img.Segs {
		pa, err := vaToPA(seg.Addr)
		if err != nil {
			return err
		}
		ram, off, ok := m.bus.RAM(pa)
		if !ok || int(off)+len(seg.Data) > len(ram) {
			return fmt.Errorf("smdk2410: segment %08X (PA %08X, %d bytes) does not fit in RAM",
				seg.Addr, pa, len(seg.Data))
		}
		copy(ram[off:], seg.Data)
	}
	entryPA, err := vaToPA(img.Entry)
	if err != nil {
		return fmt.Errorf("smdk2410: entry point: %w", err)
	}
	m.entryPA = entryPA
	return nil
}

// Reset は CPU をリセットし、エントリポイント（物理アドレス）から開始する。
// MMU は無効の状態で始まる。
func (m *Machine) Reset() {
	m.cpu.Reset(m.entryPA)
}

func (m *Machine) Step() error {
	return m.cpu.Step()
}
