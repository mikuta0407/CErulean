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
//   - 0x48000000〜: 周辺機器レジスタ群（下の peripheralStubs も参照）
//   - 0x50000000: UART（UART0/1/2 が 0x4000 間隔）
const (
	sdramBase = 0x30000000
	sdramSize = 64 * 1024 * 1024
	uartBase  = 0x50000000
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
	// UART0/1/2。レジスタ帯は各 0x4000 だが実レジスタは先頭 0x2C バイト。
	// デバッグシリアル出力は UART0 の想定なので、1/2 は出力先なし。
	// TODO: 実イメージのバナーが UART0 以外に出ていたら見直す。
	for i, w := range []io.Writer{uartOut, nil, nil} {
		name := fmt.Sprintf("uart%d", i)
		if err := b.MapMMIO(name, uartBase+uint32(i)*0x4000, 0x4000, s3c2410.NewUART(w)); err != nil {
			return nil, err
		}
	}
	// 当面は値保持スタブで済ませる周辺ブロック（S3C2410 データシート Figure 5-1）。
	// 割り込みコントローラとタイマーは、カーネルの時限処理を動かす段階で
	// 実動作する専用実装に置き換える予定。
	for _, p := range []struct {
		name string
		base uint32
		init map[uint32]uint32
	}{
		{"memc", 0x48000000, nil},     // メモリコントローラ（BWSCON など）
		{"intc", 0x4A000000, map[uint32]uint32{ // 割り込みコントローラ
			0x08: 0xFFFFFFFF, // INTMSK: リセット値は全マスク
		}},
		{"clkpwr", 0x4C000000, map[uint32]uint32{ // クロック・電源管理
			// リセット値（データシート Ch.7）。カーネルが PLL 設定から
			// クロックを逆算する場合に 0 だと壊れるため入れておく。
			// TODO: データシート原本と再照合する（記憶ベースの値）。
			0x00: 0x00FFFFFF, // LOCKTIME
			0x04: 0x0005C080, // MPLLCON
			0x08: 0x00028080, // UPLLCON
			0x0C: 0x0007FFF0, // CLKCON
			0x10: 0x00000004, // CLKSLOW
		}},
		{"lcd", 0x4D000000, nil},   // LCD コントローラ
		{"nand", 0x4E000000, nil},  // NAND フラッシュコントローラ
		{"timer", 0x51000000, nil}, // PWM タイマー
		{"wdt", 0x53000000, map[uint32]uint32{
			0x00: 0x8021, // WTCON リセット値。TODO: データシートと再照合
		}},
		{"iic", 0x54000000, nil},
		{"gpio", 0x56000000, map[uint32]uint32{
			// GSTATUS1: チップ ID。BSP が SoC 判別に読む可能性がある。
			// TODO: データシートと再照合（0x32410000 = S3C2410 のはず）
			0xB0: 0x32410000,
		}},
		{"rtc", 0x57000000, nil},
		{"adc", 0x58000000, nil},
	} {
		if err := b.MapMMIO(p.name, p.base, 0x1000, s3c2410.NewStub(p.name, p.init)); err != nil {
			return nil, err
		}
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
