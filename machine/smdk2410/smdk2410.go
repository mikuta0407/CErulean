// Package smdk2410 は Samsung S3C2410 リファレンスボード（SMDK2410）相当の
// マシン構成。Microsoft Device Emulator がエミュレートするのもこの系統の
// ボードなので、WM5 エミュレータイメージの最初のターゲットとする。
package smdk2410

import (
	"fmt"
	"image"
	"io"
	"time"

	"github.com/mikuta0407/cerulean/bus"
	"github.com/mikuta0407/cerulean/cpu"
	"github.com/mikuta0407/cerulean/cpu/arm"
	"github.com/mikuta0407/cerulean/device/s3c2410"
	"github.com/mikuta0407/cerulean/loader"
	"github.com/mikuta0407/cerulean/machine"
	"github.com/mikuta0407/cerulean/mmu"
)

// S3C2410 の物理メモリマップ（データシート Figure 5-1）:
//   - 0x30000000: SDRAM（バンク 6、窓は 128MB）。Device Emulator の 128MB 構成に
//     合わせて全部を実 RAM にする。WinCE の OEMGetExtensionDRAM が
//     0x34000000〜 を拡張 RAM（後半は RAMFMD = RAM ディスク）として使う。
//     64MB 実装＋折り返しにすると、このプローブがエイリアスを実 RAM と
//     誤検出してカーネルメモリを二重使用してしまう（2026-09 に実測）。
//   - 0x38000000: バンク 7。SDRAM 未実装（オープンバス）。
//   - 0x48000000〜: 周辺機器レジスタ群
//   - 0x50000000: UART（UART0/1/2 が 0x4000 間隔）
const (
	sdramBase = 0x30000000
	sdramSize = 128 * 1024 * 1024
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

	intc  *s3c2410.INTC
	timer *s3c2410.PWMTimer
	lcd   *s3c2410.LCD
	rtc   *s3c2410.RTC
	adc   *s3c2410.ADC
	spi   *s3c2410.SPI
	kbd   *kbdMCU

	// 仮想時間: 命令数から PCLK ティックを固定比で生成する（決定論的。
	// ユーザー確認済み 2026-09）。tickAcc は 1/8 ティック単位の端数累積。
	tickAcc uint32
	// steps はリセットからの実行命令数（= 仮想時間の基準）。入力スクリプトの
	// 時刻照合とスナップショット復元後の時間の継続に使う。
	steps uint64

	// 実行ループの作業領域（run.go。いずれも保存しない）:
	// pending はデバイスにまだ渡していない PCLK ティック、deadline は
	// 次のデバイスイベントまでのティック数（s3c2410.NoEvent なら予定なし）。
	pending, deadline int64
	idleSkip          bool          // アイドルスキップを行うか（既定 true）
	poll              pollCandidate // ポーリングループの 1 周前の観測
	skipped           uint64        // スキップした命令数の累計（計測用）

	entryPA uint32 // リセット時に飛ぶ物理アドレス
}

// 1 命令あたりの PCLK ティック数 = pclkTicksNum/8。
// 根拠(概算): CPU ~200MHz・平均 CPI ~1.5 → ~133M 命令/秒、PCLK ~50MHz
// → 約 0.375 PCLK/命令 = 3/8。カーネルの時間の流れの速さが変わるだけで
// 正しさには影響しない（タイマーは同じ仮想時間軸で数えるため）。
const pclkTicksNum = 3

// InstructionsPerSecond は仮想時間 1 秒あたりの命令数
// （pclkHz / (pclkTicksNum/8) = 50.7M × 8/3 = 135.2M）。入力スクリプトの
// 時刻（ms 等）を命令数に換算するのに使う。
const InstructionsPerSecond = pclkHz * 8 / pclkTicksNum

// pclkHz は仮想時間 1 秒あたりの PCLK ティック数（RTC の進みに使う）。
// 根拠（2026-09 実測）: ブートコードが MPLLCON=0x000A1031（MDIV=161, PDIV=3,
// SDIV=1）・CLKDIVN=3 を書く。データシートの式 Fout = (MDIV+8)×Fin /
// ((PDIV+2)×2^SDIV)、Fin=12MHz（SMDK2410 の水晶）で FCLK=202.8MHz、
// CLKDIVN=3 で PCLK=FCLK/4=50.7MHz。カーネルの Timer4（TCNTB4=25375・1/2 分周）
// がこれで約 1ms 周期になることとも整合する。
// PLL の式は User's Manual Rev 1.1 の Ch.7 で確認済み（推奨値表にも
// Fin=12MHz・MDIV=161/PDIV=3/SDIV=1 → 202.80MHz がある）。
// TODO: Fin=12MHz はボード資料で未確認（推奨値表と整合するので妥当）。
const pclkHz = 50_700_000

var _ machine.Machine = (*Machine)(nil)

// openBus は SDRAM が実装されていないバンク窓。実機ではアクセスしても
// バスフォールトにはならず読み出しは不定値になる。WinCE のメモリサイズ
// 検出が書いた署名の読み戻しに失敗して「RAM なし」と判定できるよう、
// 書き込みは無視し読み出しは 0 を返す。
// TODO: 実機の不定値は 0 とは限らない（直前のバス値が残る等）。検出が
// 誤動作するようなら見直す。
type openBus struct{}

func (openBus) Read(off uint32, size int) uint32     { return 0 }
func (openBus) Write(off uint32, size int, v uint32) {}

// New は SMDK2410 相当のマシンを組み立てる。uartOut にはカーネルデバッグ
// シリアル（UART1）の送信データが流れる。
func New(uartOut io.Writer) (*Machine, error) {
	b := bus.New()
	if err := b.MapRAM("sdram", sdramBase, sdramSize); err != nil {
		return nil, err
	}
	// バンク0〜5（0x00000000〜0x30000000）: ROM/SROM 未実装。フラッシュ
	// ドライバが NOR フラッシュの CFI/JEDEC プローブ（0xAAAA/0x5500 の
	// 書き込み）を PA 0 に対して行うので、オープンバスで空振りさせる
	// （Device Emulator 構成はフラッシュではなく RAMFMD を使う）。
	// TODO: バンク3 の Ethernet（CS8900 相当）等が必要になったら分割する。
	if err := b.MapMMIO("bank0-5-empty", 0, sdramBase, openBus{}); err != nil {
		return nil, err
	}
	// バンク7（0x38000000）: SDRAM 未実装。メモリサイズ検出が触るので
	// オープンバスとして応答だけする。
	if err := b.MapMMIO("bank7-empty", sdramBase+sdramSize, sdramSize, openBus{}); err != nil {
		return nil, err
	}
	// UART0/1/2。レジスタ帯は各 0x4000 だが実レジスタは先頭 0x2C バイト。
	// 実イメージ（WM5 Device Emulator 用 PPC_USA.bin）のカーネルデバッグ
	// シリアルは UART1 に出る（2026-09 に実測。ブートバナーが UART1 の
	// UTXH に書かれた）ので、uartOut は UART1 につなぐ。
	// TODO: UART0/2 の出力先はアプリのシリアル対応時に決める。
	for i, w := range []io.Writer{nil, uartOut, nil} {
		name := fmt.Sprintf("uart%d", i)
		if err := b.MapMMIO(name, uartBase+uint32(i)*0x4000, 0x4000, s3c2410.NewUART(w)); err != nil {
			return nil, err
		}
	}
	// 割り込みコントローラとタイマーは実動作（machine が CPU へ配線する）。
	// m の cpu は後で組み立てるため、コールバックはクロージャで遅延参照する。
	m := &Machine{deadline: s3c2410.NoEvent, idleSkip: true}
	m.intc = s3c2410.NewINTC(func(irq, fiq bool) {
		m.cpu.SetIRQ(irq)
		m.cpu.SetFIQ(fiq)
	})
	m.timer = s3c2410.NewPWMTimer(func(n int) {
		m.intc.Raise(uint(s3c2410.IntTimer0 + n))
	})
	if err := b.MapMMIO("intc", 0x4A000000, 0x1000, m.intc); err != nil {
		return nil, err
	}
	if err := b.MapMMIO("timer", 0x51000000, 0x1000, timedDev{m, m.timer}); err != nil {
		return nil, err
	}
	// LCD コントローラ: レジスタからフレームバッファの位置・形式を解釈する。
	m.lcd = s3c2410.NewLCD()
	if err := b.MapMMIO("lcd", 0x4D000000, 0x1000, m.lcd); err != nil {
		return nil, err
	}
	// RTC: 時刻は仮想時間で進む（Step 参照）。初期時刻は SetRTC で与える。
	m.rtc = s3c2410.NewRTC(pclkHz)
	if err := b.MapMMIO("rtc", 0x57000000, 0x1000, timedDev{m, m.rtc}); err != nil {
		return nil, err
	}
	// ADC/タッチスクリーン: touch.dll がこれを使う（2026-09 に実測。
	// GPGCON で GPG12〜15 をタッチ用に切り替え、ADCTSC=0xD3 で
	// 割り込み待ちにする）。割り込みは INT_ADC のサブソース INT_TC/INT_ADC。
	m.adc = s3c2410.NewADC(func(sub uint) { m.intc.RaiseSub(sub) })
	if err := b.MapMMIO("adc", 0x58000000, 0x1000, timedDev{m, m.adc}); err != nil {
		return nil, err
	}
	// SPI: SPI1 にキーボード用マイコンがつながる（kbd.go）。
	m.spi = s3c2410.NewSPI(func(ch int) {
		m.intc.Raise(uint([]int{s3c2410.IntSPI0, s3c2410.IntSPI1}[ch]))
	})
	m.kbd = &kbdMCU{raise: func() { m.intc.Raise(s3c2410.IntEINT0 + 1) }} // EINT1
	m.spi.Attach(1, m.kbd)
	if err := b.MapMMIO("spi", 0x59000000, 0x1000, m.spi); err != nil {
		return nil, err
	}
	// 当面は値保持スタブで済ませる周辺ブロック（S3C2410 データシート Figure 5-1）。
	for _, p := range []struct {
		name string
		base uint32
		init map[uint32]uint32
	}{
		{"memc", 0x48000000, nil}, // メモリコントローラ（BWSCON など）
		{"usbhost", 0x49000000, nil},
		{"clkpwr", 0x4C000000, map[uint32]uint32{ // クロック・電源管理
			// リセット値（User's Manual Rev 1.1 の Ch.7 で確認済み）。
			// カーネルが PLL 設定からクロックを逆算する場合に 0 だと
			// 壊れるため入れておく。
			0x00: 0x00FFFFFF, // LOCKTIME
			0x04: 0x0005C080, // MPLLCON
			0x08: 0x00028080, // UPLLCON
			0x0C: 0x0007FFF0, // CLKCON
			0x10: 0x00000004, // CLKSLOW
		}},
		{"nand", 0x4E000000, nil}, // NAND フラッシュコントローラ
		{"wdt", 0x53000000, map[uint32]uint32{
			0x00: 0x8021, // WTCON リセット値（User's Manual Rev 1.1 で確認済み）
		}},
		{"iic", 0x54000000, nil},
		{"usbdev", 0x52000000, nil},
		{"sdi", 0x5A000000, nil},
		{"gpio", 0x56000000, map[uint32]uint32{
			// GSTATUS1: チップ ID。BSP が SoC 判別に読む可能性がある。
			// 0x32410000（User's Manual Rev 1.1 で確認済み）
			0xB0: 0x32410000,
		}},
	} {
		if err := b.MapMMIO(p.name, p.base, 0x1000, s3c2410.NewStub(p.name, p.init)); err != nil {
			return nil, err
		}
	}
	// DMA コントローラ: 転送は即完了に見せる最小スタブ（dma.go 参照）。
	if err := b.MapMMIO("dma", 0x4B000000, 0x1000, s3c2410.NewDMAStub(func(ch int) {
		m.intc.Raise(uint(s3c2410.IntDMA0 + ch))
	})); err != nil {
		return nil, err
	}
	// IIS（オーディオ）: 値保持スタブだが、IISCON(0x00) の bit7
	// （TX FIFO ready）は常に立てる。FIFO は無限シンク扱いで、オーディオ
	// ドライバの送信 ready 待ちポーリングを通すため（2026-09 に実測）。
	// TODO: 音を出すときは FIFO・DMA 込みの実装に置き換える。
	if err := b.MapMMIO("iis", 0x55000000, 0x1000,
		s3c2410.NewStub("iis", nil).ForceReadBits(0x00, 1<<7)); err != nil {
		return nil, err
	}
	// 0x500F0000: S3C2410 のデータシートにない領域。Device Emulator 固有の
	// 準仮想デバイス群と判断した（ROM の TOC からアクセス元モジュールを特定。
	// 2026-09 の実イメージ観察。いずれもブートを止める要因ではなかった）:
	//   - +0x2080 + n*0x20（n=0..3）: dmatrans.dll（DE の DMA トランスポート。
	//     ActiveSync/デバッガ用のホスト通信と推定）が 4 チャネルを初期化する。
	//     各チャネル +0x00 に 1 を書き、後で 1 を読み返してから
	//     +0x04 = 0x500F2000+4n、+0x10 = 0x26、+0x00 = 0x101 を書く。
	//     書き込みは ceddk.dll の WRITE_REGISTER_ULONG 経由。
	//   - +0x5000〜+0x5007: emulserv.dll（エミュレータサービス）が VirtualCopy で
	//     8 バイトだけマップし、+0x04 を読んで bit30 を検査、+0x00 に
	//     0xFFFFFFFF を書く。割り込みは GPF3 を EINT3（High レベル）に設定して
	//     受ける（GPFCON/GPFUP/EXTINT0 を操作）。
	// どちらもホスト側が居ないと動作しない機能なので、値保持スタブのまま
	// （初期化の読み返しが通れば十分）。
	// TODO: ホスト連携（フォルダ共有・ActiveSync 等）が必要になったら
	// レジスタの意味を観察から詰める。
	if err := b.MapMMIO("de-paravirt-500F0000", 0x500F0000, 0x10000, s3c2410.NewStub("de-paravirt", nil)); err != nil {
		return nil, err
	}
	// mmu.MMU は CPU から見たメモリ空間（cpu.Memory）と CP15（arm.Coprocessor）を兼ねる。
	mm := mmu.New(b)
	m.cpu = arm.New(mm, mm)
	m.bus = b
	m.mmu = mm
	return m, nil
}

func (m *Machine) Name() string { return "smdk2410" }

func (m *Machine) CPU() cpu.CPU { return m.cpu }

// Bus は物理バス（デバッグ・テスト用）。
func (m *Machine) Bus() *bus.Bus { return m.bus }

// Peek32 は CPU から見えるアドレス空間（MMU 有効なら変換込み）を
// 副作用なしで読む。デバッグ・トレース用。
// 注意: MMIO を指すと副作用が出得るが、コード領域を覗く用途では問題ない。
func (m *Machine) Peek32(addr uint32) (uint32, error) {
	return m.mmu.Read32(addr)
}

// Framebuffer は LCD コントローラの現在の設定でフレームバッファを画像化する。
// 表示無効・未対応モードなら error（設定値は診断用に返す）。
func (m *Machine) Framebuffer() (*image.RGBA, s3c2410.LCDConfig, error) {
	return m.lcd.Frame(m.bus.Read32)
}

// SetRTC は RTC の現在時刻を設定する（Reset 前に呼ぶ）。t の壁時計の値
// （年月日時分秒）がそのまま RTC に入る。ホストの時計を読むのは呼び出し側
// （cmd）の責務で、コアは渡された時刻からの仮想時間で決定論的に進める。
func (m *Machine) SetRTC(t time.Time) { m.rtc.SetTime(t) }

// Translate は CPU から見た VA を現在の MMU 状態で PA に変換する（デバッグ用）。
func (m *Machine) Translate(va uint32) (uint32, error) {
	return m.mmu.Translate(va)
}

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
	m.steps = 0
	m.poll = pollCandidate{}
	m.updateDeadline()
}

// Steps はリセット（またはスナップショットの保存時点から継続して）
// からの実行命令数。
func (m *Machine) Steps() uint64 { return m.steps }
