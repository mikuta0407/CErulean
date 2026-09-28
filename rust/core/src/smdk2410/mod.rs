//! Samsung S3C2410 リファレンスボード（SMDK2410）相当のマシン構成（Go の
//! machine/smdk2410 パッケージ）。Microsoft Device Emulator がエミュレート
//! するのもこの系統のボードなので、WM5 エミュレータイメージの最初のターゲット。
//!
//! 所有権の設計（計画書 §3.3 の案 A）: [`Machine`] が CPU の状態（`cpu`）と
//! システム（`sys`: MMU・バス・ボードのデバイス群と仮想時間）を別々のフィールドと
//! して持ち、`arm` の実行関数が両方を借りる。

mod board;
mod kbd;
mod run;
#[cfg(test)]
mod tests;

use std::fmt;

use crate::arm::{Cpu, RunCtl, StopError, System};
use crate::bus::{Bus, BusPhys};
use crate::cpu::{Abort, MemError};
use crate::loader::Image;
use crate::mmu::Mmu;
use crate::s3c2410::{Frame, FrameError, LcdConfig};

pub use board::{Board, Dev, StubId};
pub use kbd::{KEY_SCAN_CODES, KbdMcu, scan_code};

// S3C2410 の物理メモリマップ（データシート Figure 5-1）:
//   - 0x30000000: SDRAM（バンク 6、窓は 128MB）。Device Emulator の 128MB 構成に
//     合わせて全部を実 RAM にする。WinCE の OEMGetExtensionDRAM が
//     0x34000000〜 を拡張 RAM（後半は RAMFMD = RAM ディスク）として使う。
//     64MB 実装＋折り返しにすると、このプローブがエイリアスを実 RAM と
//     誤検出してカーネルメモリを二重使用してしまう（2026-09 に実測）。
//   - 0x38000000: バンク 7。SDRAM 未実装（オープンバス）。
//   - 0x48000000〜: 周辺機器レジスタ群
//   - 0x50000000: UART（UART0/1/2 が 0x4000 間隔）
pub const SDRAM_BASE: u32 = 0x30000000;
pub const SDRAM_SIZE: u32 = 128 * 1024 * 1024;
const UART_BASE: u32 = 0x50000000;

/// WinCE カーネルの仮想アドレスマッピングのうち、ロード時の変換に使う部分。
/// VA 0x80000000〜 が PA 0x30000000〜（SDRAM）に対応する（実イメージで確認済み。
/// 完全な OEMAddressTable は CLAUDE.md の「確認済みの事実」）。
const KERNEL_VA_BASE: u32 = 0x80000000;

/// 1 命令あたりの PCLK ティック数 = PCLK_TICKS_NUM/8。
/// 根拠(概算): CPU ~200MHz・平均 CPI ~1.5 → ~133M 命令/秒、PCLK ~50MHz
/// → 約 0.375 PCLK/命令 = 3/8。カーネルの時間の流れの速さが変わるだけで
/// 正しさには影響しない（タイマーは同じ仮想時間軸で数えるため）。
pub(crate) const PCLK_TICKS_NUM: u64 = 3;

/// 仮想時間 1 秒あたりの PCLK ティック数（RTC の進みに使う）。
/// 根拠（2026-09 実測）: ブートコードが MPLLCON=0x000A1031（MDIV=161, PDIV=3,
/// SDIV=1）・CLKDIVN=3 を書く。データシートの式 Fout = (MDIV+8)×Fin /
/// ((PDIV+2)×2^SDIV)、Fin=12MHz（SMDK2410 の水晶）で FCLK=202.8MHz、
/// CLKDIVN=3 で PCLK=FCLK/4=50.7MHz。カーネルの Timer4（TCNTB4=25375・1/2 分周）
/// がこれで約 1ms 周期になることとも整合する。PLL の式は User's Manual Rev 1.1 の
/// Ch.7 で確認済み（推奨値表にも Fin=12MHz・MDIV=161/PDIV=3/SDIV=1 → 202.80MHz）。
/// TODO: Fin=12MHz はボード資料で未確認（推奨値表と整合するので妥当）。
pub(crate) const PCLK_HZ: i64 = 50_700_000;

/// 仮想時間 1 秒あたりの命令数（PCLK_HZ / (PCLK_TICKS_NUM/8) = 135.2M）。
/// 入力スクリプトの時刻（ms 等）を命令数に換算するのに使う。
pub const INSTRUCTIONS_PER_SECOND: u64 = PCLK_HZ as u64 * 8 / PCLK_TICKS_NUM;

// タッチパネル（S3C2410 ADC/TS）の画面座標 → ADC 生値の変換（input で使う）。
//
// 根拠（2026-09、実イメージの touch.dll をトレースして確認した事実）:
// ドライバはキャリブレーションデータを使わず（レジストリの
// HARDWARE\DEVICEMAP\TOUCH には MaxCalError=7 しかない）、固定の一次式で
// 1/4 ピクセル単位の座標（画面 240×320 → 960×1280）に変換する:
//
//   X4 = (ADCDAT1 − 85) × 960 / 880        （0〜959 にクリップ）
//   Y4 = (1023 − ADCDAT0 − 105) × 1280 / 875（0〜1279 にクリップ）
//
// つまりパネルの X/Y と画面の軸は入れ替わっていて、画面 X は ADC の
// Y 測定値（ADCDAT1）、画面 Y は X 測定値（ADCDAT0）の反転から求まる。
// 割り算は定数の逆数掛け（smull）で、0 方向への切り捨て。
// ここではその逆変換として、ピクセル中心（4x+2）に対応する生値を返す。
pub const TOUCH_SCREEN_W: u32 = 240;
pub const TOUCH_SCREEN_H: u32 = 320;
const TOUCH_D1_OFFSET: u32 = 85; // ADCDAT1 のオフセット（画面 X 用）
const TOUCH_D1_SPAN: u32 = 880; // ADCDAT1 の範囲（960 に対応）
const TOUCH_D0_OFFSET: u32 = 105; // 1023−ADCDAT0 のオフセット（画面 Y 用）
const TOUCH_D0_SPAN: u32 = 875; // 同範囲（1280 に対応）

/// 画面ピクセル (x, y) を ADCDAT0（XPDATA）・ADCDAT1（YPDATA）に入る生値へ
/// 変換する（Go と同じ整数演算。ゲストが読む値なので丸めまで同じにする）。
pub(crate) fn touch_to_raw(x: u32, y: u32) -> (u32, u32) {
    let (x4, y4) = (4 * x + 2, 4 * y + 2);
    let yp = TOUCH_D1_OFFSET + (x4 * TOUCH_D1_SPAN + 960 / 2) / 960;
    let inv = TOUCH_D0_OFFSET + (y4 * TOUCH_D0_SPAN + 1280 / 2) / 1280;
    (1023 - inv, yp)
}

/// マシンの操作のエラー（構成・ロード・入力の誤り）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "smdk2410: {}", self.0)
    }
}

impl std::error::Error for Error {}

/// CPU から見たシステム（MMU・バス・ボード）。[`System`] を実装する。
pub struct Sys {
    pub mmu: Mmu,
    pub bus: Bus<Dev>,
    pub board: Board,
}

impl System for Sys {
    #[inline(always)]
    fn read(&mut self, va: u32, size: u32) -> Result<u32, MemError> {
        let mut p = BusPhys {
            bus: &mut self.bus,
            devs: &mut self.board,
        };
        self.mmu.read(va, size, &mut p)
    }
    #[inline(always)]
    fn write(&mut self, va: u32, size: u32, v: u32) -> Result<(), MemError> {
        let mut p = BusPhys {
            bus: &mut self.bus,
            devs: &mut self.board,
        };
        self.mmu.write(va, size, v, &mut p)
    }
    #[inline(always)]
    fn fetch32(&mut self, va: u32) -> Result<u32, MemError> {
        let mut p = BusPhys {
            bus: &mut self.bus,
            devs: &mut self.board,
        };
        self.mmu.fetch32(va, &mut p)
    }
    fn cp15_read(&mut self, opc1: u8, crn: u8, crm: u8, opc2: u8) -> u32 {
        self.mmu.cp15_read(opc1, crn, crm, opc2)
    }
    fn cp15_write(&mut self, opc1: u8, crn: u8, crm: u8, opc2: u8, v: u32) {
        self.mmu.cp15_write(opc1, crn, crm, opc2, v)
    }
    fn vector_base(&self) -> u32 {
        self.mmu.vector_base()
    }
    fn set_privileged(&mut self, privileged: bool) {
        self.mmu.set_privileged(privileged)
    }
    fn record_data_abort(&mut self, a: &Abort) {
        self.mmu.record_data_abort(a)
    }
    #[inline(always)]
    fn irq(&self) -> bool {
        self.board.intc.irq()
    }
    #[inline(always)]
    fn fiq(&self) -> bool {
        self.board.intc.fiq()
    }
    #[inline(always)]
    fn run_ctl(&mut self) -> &mut RunCtl {
        &mut self.board.run
    }
}

/// SMDK2410 相当のマシン。
pub struct Machine {
    pub cpu: Cpu,
    pub sys: Sys,
    /// リセット時に飛ぶ物理アドレス（保存する）
    pub(crate) entry_pa: u32,
}

impl Machine {
    /// SMDK2410 相当のマシンを組み立てる。
    pub fn new() -> Machine {
        let mut b = Bus::new();
        map(&mut b).expect("smdk2410: fixed memory map must be valid");
        Machine {
            cpu: Cpu::new(),
            sys: Sys {
                mmu: Mmu::new(),
                bus: b,
                board: Board::new(),
            },
            entry_pa: 0,
        }
    }

    pub fn name(&self) -> &'static str {
        "smdk2410"
    }

    /// ローダーの中間表現を RAM に配置し、リセット後にエントリポイントから
    /// 実行される状態にする。イメージ内アドレス（CE 仮想アドレス）から
    /// 物理アドレスへの変換はここで行う。
    pub fn load_image(&mut self, img: &Image) -> Result<(), Error> {
        for seg in &img.segs {
            let pa = va_to_pa(seg.addr)?;
            let fits = self.sys.bus.ram_mut(pa).and_then(|(ram, off)| {
                let dst = ram.get_mut(off as usize..off as usize + seg.data.len())?;
                dst.copy_from_slice(&seg.data);
                Some(())
            });
            if fits.is_none() {
                return Err(Error(format!(
                    "segment {:08X} (PA {pa:08X}, {} bytes) does not fit in RAM",
                    seg.addr,
                    seg.data.len()
                )));
            }
        }
        self.entry_pa = va_to_pa(img.entry).map_err(|e| Error(format!("entry point: {}", e.0)))?;
        Ok(())
    }

    /// RTC の現在時刻を年月日時分秒で設定する（reset の前に呼ぶ）。壁時計の値が
    /// そのまま RTC に入る。ホストの時計を読むのは呼び出し側の責務で、コアは
    /// 渡された時刻からの仮想時間で決定論的に進める。
    pub fn set_rtc(
        &mut self,
        year: i64,
        month: i64,
        day: i64,
        hour: i64,
        minute: i64,
        second: i64,
    ) {
        self.sys
            .board
            .rtc
            .set_time(year, month, day, hour, minute, second);
    }

    /// CPU をリセットし、エントリポイント（物理アドレス）から開始する。
    /// MMU は無効の状態で始まる。
    pub fn reset(&mut self) {
        self.cpu.reset(self.entry_pa, &mut self.sys);
        self.sys.board.steps = 0;
        self.sys.board.update_deadline();
    }

    /// リセット（またはスナップショットの保存時点から継続して）からの実行命令数。
    pub fn steps(&self) -> u64 {
        self.sys.board.steps()
    }

    /// 1 命令ぶん進める。
    pub fn step(&mut self) -> Result<(), StopError> {
        self.run_until(self.steps() + 1)
    }

    /// UART1（カーネルデバッグシリアル）が送信したバイトを取り出す。
    pub fn take_uart1(&mut self) -> Vec<u8> {
        self.sys.board.uart[1].take_tx()
    }

    /// 物理アドレス監視（-watch）を足す。監視中は MMU が RAM を直接持たない
    /// （バスを経由させる）ので、スナップショットの読み込みより前に呼ぶこと。
    pub fn add_watch(&mut self, lo: u32, hi: u32) {
        self.sys.bus.add_watch(lo, hi);
    }

    /// 監視範囲へのアクセスの記録を取り出す。
    pub fn take_watch_log(&mut self) -> Vec<crate::bus::WatchEvent> {
        std::mem::take(&mut self.sys.bus.watch_log)
    }

    /// LCD コントローラの現在の設定でフレームバッファを画像化する。表示無効・
    /// 未対応モードなら Err。Go と同じくバス経由で読む（監視の記録も同じになる）。
    pub fn frame(&mut self) -> Result<(Frame, LcdConfig), FrameError> {
        // LCD の設定を写してから読む（バスの読み出しはボード全体を借りるため）。
        // フレームバッファの読み出しは LCD の状態を変えないので結果は同じ。
        let lcd = self.sys.board.lcd.clone();
        let Sys { bus, board, .. } = &mut self.sys;
        lcd.frame(|pa| bus.read(pa, 4, board))
    }

    /// CPU から見た VA を現在の MMU 状態で PA に変換する（デバッグ用。状態は変えない）。
    pub fn translate(&mut self, va: u32) -> Result<u32, MemError> {
        let Sys { mmu, bus, board } = &mut self.sys;
        mmu.translate_debug(va, &mut BusPhys { bus, devs: board })
    }

    /// CPU から見えるアドレスの RAM を、状態を変えずに読む（トレース・履歴の
    /// 表示用）。変換できない・RAM でない場合は None。Go の Peek32 は TLB を
    /// 埋め得たが、表示でゲストの状態を変えないよう、ここでは変換だけを行う。
    pub fn peek32(&mut self, va: u32) -> Option<u32> {
        let pa = self.translate(va & !3).ok()?;
        let (ram, off) = self.sys.bus.ram(pa)?;
        let b = ram.get(off as usize..off as usize + 4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// CPU 状態のダンプ（一致確認用。並びは testdata/golden/README.md の版数 1、
    /// 212 バイト、すべて LE）。状態は変えない。
    pub fn cpu_dump(&self) -> Vec<u8> {
        const VERSION: u32 = 1;
        let a = self.cpu.arch_regs();
        let mut b = Vec::with_capacity(212);
        b.extend_from_slice(&VERSION.to_le_bytes());
        b.extend_from_slice(&self.steps().to_le_bytes());
        let words =
            a.r.iter()
                .chain(&a.usr)
                .chain(&a.fiq)
                .chain(&a.irq)
                .chain(&a.svc)
                .chain(&a.abt)
                .chain(&a.und);
        for w in words.chain([a.cpsr].iter()).chain(&a.spsr) {
            b.extend_from_slice(&w.to_le_bytes());
        }
        // CP15 の c1, c2, c3, c5, c6, c13（読むだけで MMU の状態を変えない）
        for crn in [1, 2, 3, 5, 6, 13] {
            b.extend_from_slice(&self.sys.mmu.cp15_read(0, crn, 0, 0).to_le_bytes());
        }
        b
    }

    // ---- 入力 API ----
    // UI やスクリプト再生は命令の合間にこれらを呼ぶ。決定論性は「どの命令数の
    // 時点で呼んだか」で決まるので、再現が必要な呼び出し側は steps() を基準にする。

    /// タッチ座標の範囲（= LCD の解像度）。
    pub fn touch_screen_size(&self) -> (u32, u32) {
        (TOUCH_SCREEN_W, TOUCH_SCREEN_H)
    }

    /// ペンを画面座標 (x, y) に下ろす（下ろしたまま動かすのも同じ）。
    pub fn touch_down(&mut self, x: i64, y: i64) -> Result<(), Error> {
        if x < 0 || y < 0 || x >= TOUCH_SCREEN_W as i64 || y >= TOUCH_SCREEN_H as i64 {
            return Err(Error(format!(
                "touch position ({x},{y}) outside {TOUCH_SCREEN_W}x{TOUCH_SCREEN_H}"
            )));
        }
        let (xp, yp) = touch_to_raw(x as u32, y as u32);
        self.touch_raw(true, xp, yp);
        Ok(())
    }

    /// ペンを上げる（位置は最後の値のまま）。
    pub fn touch_up(&mut self) {
        let b = &mut self.sys.board;
        b.sync_time();
        let subs = b.adc.set_pen_up();
        b.intc.raise_sub_mask(subs);
        b.update_deadline();
    }

    /// タッチパネルの ADC 生値（0〜1023）を直接与える（調査用）。down=false でペンアップ。
    /// ADC は時間を持つので、溜めた仮想時間を先に渡してから変える（Go の timedDev と
    /// 同じ理由）。
    pub fn touch_raw(&mut self, down: bool, raw_x: u32, raw_y: u32) {
        let b = &mut self.sys.board;
        b.sync_time();
        let subs = b.adc.set_pen(down, raw_x, raw_y);
        b.intc.raise_sub_mask(subs);
        b.update_deadline();
    }

    /// キーを押す。
    pub fn key_down(&mut self, name: &str) -> Result<(), Error> {
        let sc = scan_code(name).ok_or_else(|| Error(format!("unknown key {name:?}")))?;
        self.kbd_push(sc);
        Ok(())
    }

    /// キーを離す。
    pub fn key_up(&mut self, name: &str) -> Result<(), Error> {
        let sc = scan_code(name).ok_or_else(|| Error(format!("unknown key {name:?}")))?;
        self.kbd_push(sc | 0x80);
        Ok(())
    }

    /// キーボードマイコンから送るバイト列を直接積む（調査用）。
    pub fn keyboard_raw(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.kbd_push(b);
        }
    }

    fn kbd_push(&mut self, b: u8) {
        let board = &mut self.sys.board;
        if board.kbd.push(b) {
            board.intc.raise(crate::s3c2410::INT_EINT0 + 1); // EINT1
        }
    }

    /// ドライバが送ったバイトを返して記録を空にする（調査用）。
    pub fn keyboard_log(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.sys.board.kbd.log)
    }
}

impl Default for Machine {
    fn default() -> Self {
        Self::new()
    }
}

/// 物理メモリマップを登録する（Go の New のバス構成と同じアドレス・大きさ）。
fn map(b: &mut Bus<Dev>) -> Result<(), crate::bus::MapError> {
    b.map_ram("sdram", SDRAM_BASE, SDRAM_SIZE)?;
    // バンク0〜5（0x00000000〜0x30000000）: ROM/SROM 未実装。フラッシュ
    // ドライバが NOR フラッシュの CFI/JEDEC プローブ（0xAAAA/0x5500 の
    // 書き込み）を PA 0 に対して行うので、オープンバスで空振りさせる
    // （Device Emulator 構成はフラッシュではなく RAMFMD を使う）。
    // TODO: バンク3 の Ethernet（CS8900 相当）等が必要になったら分割する。
    b.map_mmio("bank0-5-empty", 0, SDRAM_BASE, Dev::OpenBus)?;
    // バンク7（0x38000000）: SDRAM 未実装。メモリサイズ検出が触るので
    // オープンバスとして応答だけする。
    // TODO: 実機の不定値は 0 とは限らない（直前のバス値が残る等）。検出が
    // 誤動作するようなら見直す。
    b.map_mmio(
        "bank7-empty",
        SDRAM_BASE + SDRAM_SIZE,
        SDRAM_SIZE,
        Dev::OpenBus,
    )?;
    // UART0/1/2。レジスタ帯は各 0x4000 だが実レジスタは先頭 0x2C バイト。
    for (i, name) in ["uart0", "uart1", "uart2"].into_iter().enumerate() {
        b.map_mmio(
            name,
            UART_BASE + i as u32 * 0x4000,
            0x4000,
            Dev::Uart(i as u8),
        )?;
    }
    // 割り込みコントローラとタイマーは実動作。
    b.map_mmio("intc", 0x4A000000, 0x1000, Dev::Intc)?;
    b.map_mmio("timer", 0x51000000, 0x1000, Dev::Timer)?;
    // LCD コントローラ: レジスタからフレームバッファの位置・形式を解釈する。
    b.map_mmio("lcd", 0x4D000000, 0x1000, Dev::Lcd)?;
    // RTC: 時刻は仮想時間で進む。初期時刻は set_rtc で与える。
    b.map_mmio("rtc", 0x57000000, 0x1000, Dev::Rtc)?;
    // ADC/タッチスクリーン: touch.dll がこれを使う（2026-09 に実測。
    // GPGCON で GPG12〜15 をタッチ用に切り替え、ADCTSC=0xD3 で
    // 割り込み待ちにする）。割り込みは INT_ADC のサブソース INT_TC/INT_ADC。
    b.map_mmio("adc", 0x58000000, 0x1000, Dev::Adc)?;
    // SPI: SPI1 にキーボード用マイコンがつながる（kbd.rs）。
    b.map_mmio("spi", 0x59000000, 0x1000, Dev::Spi)?;
    // 当面は値保持スタブで済ませる周辺ブロック（初期値は board.rs）。
    for (name, base, id) in [
        ("memc", 0x48000000, StubId::Memc),
        ("usbhost", 0x49000000, StubId::UsbHost),
        ("clkpwr", 0x4C000000, StubId::ClkPwr),
        ("nand", 0x4E000000, StubId::Nand),
        ("wdt", 0x53000000, StubId::Wdt),
        ("iic", 0x54000000, StubId::Iic),
        ("usbdev", 0x52000000, StubId::UsbDev),
        ("sdi", 0x5A000000, StubId::Sdi),
        ("gpio", 0x56000000, StubId::Gpio),
    ] {
        b.map_mmio(name, base, 0x1000, Dev::Stub(id))?;
    }
    // DMA コントローラ: 転送は即完了に見せる最小スタブ（s3c2410::DmaStub）。
    b.map_mmio("dma", 0x4B000000, 0x1000, Dev::Dma)?;
    // IIS（オーディオ）: 値保持スタブ＋ TX FIFO ready の常時ビット（board.rs）。
    b.map_mmio("iis", 0x55000000, 0x1000, Dev::Stub(StubId::Iis))?;
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
    b.map_mmio(
        "de-paravirt-500F0000",
        0x500F0000,
        0x10000,
        Dev::Stub(StubId::DeParavirt),
    )?;
    Ok(())
}

/// イメージ内アドレス（CE 仮想アドレス）をロード先物理アドレスに変換する。
/// MMU 有効化前のロード時にだけ使う。
pub(crate) fn va_to_pa(va: u32) -> Result<u32, Error> {
    // 0x80000000〜0x9FFFFFFF（キャッシュあり）/ 0xA0000000〜0xBFFFFFFF（なし）は
    // 同じ物理にマップされるのが CE の流儀。下位 29 ビットをオフセットとして扱う。
    if (KERNEL_VA_BASE..0xC0000000).contains(&va) {
        return Ok(SDRAM_BASE + (va & 0x1FFFFFFF));
    }
    // 既に物理アドレス（.nb0 を RAM 直指定でロードする場合など）ならそのまま。
    if (SDRAM_BASE..SDRAM_BASE + SDRAM_SIZE).contains(&va) {
        return Ok(va);
    }
    Err(Error(format!("no mapping for image address {va:08X}")))
}
