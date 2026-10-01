//! ボードのデバイス群・MMIO の振り分け・仮想時間。

use crate::arm::RunCtl;
use crate::bus::Devices;
use crate::s3c2410::*;

use super::kbd::KbdMcu;
use super::{PCLK_HZ, PCLK_TICKS_NUM};
use crate::pccard::{Card, Pd6710, Slot};
use crate::s3c2410::eint;

/// バスに登録する MMIO デバイスの識別子。
/// デバイスはボードが持ち、バスには番号だけを置く。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dev {
    /// SDRAM が実装されていないバンク窓（読み 0・書き無視）
    OpenBus,
    /// バンク2（nGCS2）: PC カードコントローラ（ISA のメモリ空間と I/O 空間）
    Bank2,
    /// 0x500F4000〜0x500F5FFF: Device Emulator のフォルダ共有（deshare.rs）
    DeShare,
    /// バンク0 の NOR フラッシュ（WM6 のイメージ。中身はバスのフラッシュ領域）
    Flash,
    /// GPIO（値保持スタブ＋外部割り込み。StubId::Gpio のスタブを使う）
    Gpio,
    Uart(u8),
    Intc,
    Timer,
    Lcd,
    Rtc,
    Adc,
    Spi,
    Dma,
    /// IIS（オーディオ）
    Iis,
    Stub(StubId),
}

/// 値保持スタブのブロック（S3C2410 データシート Figure 5-1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StubId {
    /// メモリコントローラ（BWSCON など）
    Memc = 0,
    UsbHost,
    /// クロック・電源管理
    ClkPwr,
    /// NAND フラッシュコントローラ
    Nand,
    Wdt,
    Iic,
    UsbDev,
    Sdi,
    Gpio,
    /// 0x500F0000: Device Emulator 固有の準仮想デバイス群（smdk2410 の構成のコメント参照）
    DeParavirt,
}

pub const NUM_STUBS: usize = 10;

/// ボード上のデバイス群と仮想時間。
/// バスとは別のフィールドなので、バスの読み書きに `&mut Board` を渡せる。
pub struct Board {
    pub intc: Intc,
    pub timer: PwmTimer,
    pub lcd: Lcd,
    pub rtc: Rtc,
    pub adc: Adc,
    pub spi: Spi,
    pub kbd: KbdMcu,
    /// PC カードコントローラ（バンク2）とソケットのカード
    pub pcic: Pd6710,
    pub card: Option<Slot>,
    /// Device Emulator のフォルダ共有（「Storage Card」。ソケットを使わない）
    pub deshare: super::deshare::DeShare,
    /// バンク0 の NOR フラッシュ（WM6 のイメージの構成だけ。中身はバスが持つ）
    pub flash: Option<crate::norflash::NorFlash>,
    /// 外部割り込みのピンのレベル（ビット n = EINTn。部品の状態から決まる派生情報。
    /// 保存しない）
    pub(crate) eint_levels: u32,
    pub uart: [Uart; 3],
    pub dma: Dma,
    pub iis: Iis,
    /// 音の出力（出力側の都合: 保存しない）
    pub(crate) audio: AudioOut,
    pub stubs: [Stub; NUM_STUBS],

    /// 仮想時間: 命令数から PCLK ティックを固定比で生成する（決定論的。
    /// ユーザー確認済み 2026-09）。tick_acc は 1/8 ティック単位の端数累積（保存する）。
    pub(crate) tick_acc: u32,
    /// リセットからの実行命令数（= 仮想時間の基準。保存する）。
    pub(crate) steps: u64,

    // 実行ループの作業領域（run.rs。いずれも保存しない）:
    /// デバイスにまだ渡していない PCLK ティック
    pub(crate) pending: i64,
    /// 次のデバイスイベントまでのティック数（NO_EVENT なら予定なし）
    pub(crate) deadline: i64,
    /// CPU のブロック実行中（catch_up）
    pub(crate) in_run: bool,
    /// ブロック実行中、steps に反映済みの命令数
    pub(crate) accounted: u64,
    /// CPU の実行の上限
    pub(crate) run: RunCtl,
}

impl Board {
    pub(crate) fn new() -> Board {
        let stub = |init: &[(u32, u32)]| Stub::new(init);
        Board {
            intc: Intc::new(),
            timer: PwmTimer::new(),
            lcd: Lcd::new(),
            rtc: Rtc::new(PCLK_HZ),
            adc: Adc::new(),
            spi: Spi::new(),
            kbd: KbdMcu::default(),
            pcic: Pd6710::new(),
            card: None,
            deshare: Default::default(),
            flash: None,
            eint_levels: EINT_IDLE,
            // UART1 がカーネルデバッグシリアル（2026-09 に実測。ブートバナーが
            // UART1 の UTXH に書かれた）。TODO: UART0/2 の出力先はアプリの
            // シリアル対応時に決める。
            uart: [Uart::new(false), Uart::new(true), Uart::new(false)],
            dma: Dma::new(),
            iis: Iis::new(),
            audio: AudioOut::default(),
            stubs: [
                stub(&[]), // memc
                stub(&[]), // usbhost
                // クロック・電源管理のリセット値（User's Manual Rev 1.1 の Ch.7 で
                // 確認済み）。カーネルが PLL 設定からクロックを逆算する場合に 0 だと
                // 壊れるため入れておく。
                stub(&[
                    (0x00, 0x00FFFFFF), // LOCKTIME
                    (0x04, 0x0005C080), // MPLLCON
                    (0x08, 0x00028080), // UPLLCON
                    (0x0C, 0x0007FFF0), // CLKCON
                    (0x10, 0x00000004), // CLKSLOW
                ]),
                stub(&[]),               // nand
                stub(&[(0x00, 0x8021)]), // WTCON リセット値（User's Manual Rev 1.1 で確認済み）
                stub(&[]),               // iic
                stub(&[]),               // usbdev
                stub(&[]),               // sdi
                // GSTATUS1: チップ ID。BSP が SoC 判別に読む可能性がある。
                // 0x32410000（User's Manual Rev 1.1 で確認済み）
                // EINTMASK のリセット値（データシート 9-26）
                stub(&[(0xB0, 0x32410000), (eint::EINTMASK, eint::EINTMASK_RESET)]),
                stub(&[]), // de-paravirt
            ],
            tick_acc: 0,
            steps: 0,
            pending: 0,
            deadline: NO_EVENT,
            in_run: false,
            accounted: 0,
            run: RunCtl::default(),
        }
    }

    // ---- 仮想時間 ----
    //
    // デバイス時間のまとめ進め（性能対策。ユーザー確認済み 2026-09）: 仮想時間の
    // PCLK ティックは毎命令ではなく pending に溜め、次の 2 つの時点でだけデバイスの
    // advance を呼ぶ。
    //   - 溜まったティックが「次のデバイスイベント（タイマー満了・ADC 変換
    //     完了）」の期限に達した命令の直後（1 命令ずつ進めた場合と同じ命令境界）
    //   - CPU（またはホスト側の入力 API）が時間を持つデバイスに触れる直前
    //     レジスタ読み出しが常に最新の時刻を反映する。
    // 期限より手前では advance は加算的（advance(a)+advance(b) == advance(a+b)）
    // なので、全状態が毎命令 advance した場合と一致する。

    /// 今から何命令目で次のデバイスイベントの期限のティックが生まれるか（1 以上）。
    /// (tick_acc + PCLK_TICKS_NUM*k)/8 >= deadline-pending の最小の k。
    /// 64 ビットの折り返しで計算する（期限に達したら即 sync_time する
    /// ので need は 1 以上のはず）。
    pub(crate) fn steps_to_deadline(&self) -> u64 {
        let need = self.deadline.wrapping_sub(self.pending) as u64;
        need.wrapping_mul(8)
            .wrapping_sub(self.tick_acc as u64)
            .wrapping_add(PCLK_TICKS_NUM - 1)
            / PCLK_TICKS_NUM
    }

    /// ブロック実行中（または直後）に、CPU が実行を終えた命令の分だけ
    /// 命令数と仮想時間を進める。
    pub(crate) fn catch_up(&mut self) {
        if !self.in_run {
            return;
        }
        let done = self.run.n;
        let n = done - self.accounted;
        self.accounted = done;
        self.steps += n;
        self.add_ticks(n);
    }

    /// n 命令分のティックを pending に足す（1 命令 = 3/8 PCLK。1/8 ティックの
    /// 端数を持ち越す）。
    pub(crate) fn add_ticks(&mut self, n: u64) {
        let total = self.tick_acc as u64 + PCLK_TICKS_NUM * n;
        self.pending += (total >> 3) as i64;
        self.tick_acc = (total & 7) as u32;
    }

    /// 溜めたティックをデバイスに渡し、次の期限を求め直す。
    pub(crate) fn sync_time(&mut self) {
        self.catch_up();
        if self.pending > 0 {
            let t = self.pending;
            self.pending = 0;
            let fired = self.timer.advance(t);
            self.intc.raise_mask(fired << INT_TIMER0);
            self.rtc.advance(t);
            let subs = self.adc.advance(t);
            self.intc.raise_sub_mask(subs);
            let dma = self.dma.advance(&mut self.iis, t);
            self.intc.raise_mask(dma << INT_DMA0);
        }
        self.update_deadline();
    }

    /// 次のデバイスイベントまでのティック数を求める。時間を持つデバイスの
    /// 状態が変わったら（レジスタ書き込み・入力）呼ぶ。ブロック実行中なら、
    /// 新しい期限を生む命令で止まるよう CPU の上限を下げる（実行中の命令も
    /// 1 命令目に数える: その命令のティックも期限に向けて進むため）。
    pub(crate) fn update_deadline(&mut self) {
        self.deadline = self
            .timer
            .next_event()
            .min(self.adc.next_event())
            .min(self.dma.next_event(&self.iis));
        if self.in_run && self.deadline != NO_EVENT {
            let lim = self.accounted + self.steps_to_deadline();
            self.run.limit(lim);
        }
    }

    /// Steps。ブロック実行の途中（デバイスのアクセス中）でも、
    /// 実行を終えた命令まで数えた値を返す。
    pub fn steps(&self) -> u64 {
        if self.in_run {
            self.steps + self.run.n - self.accounted
        } else {
            self.steps
        }
    }
}

// ---- PC カード（バンク2）と外部割り込みの配線 ----
//
// 観察（2026-09-29、pcc_smdk2410.dll の初期化。docs/storage-card-design.md）:
//   - I/O 空間は PA 0x11000000 + ISA の I/O ポート、メモリ空間は PA 0x10000000 +
//     ISA のメモリアドレス（コントローラのポート 0x3E0/0x3E1 と、属性メモリの CIS を
//     この位置で読む）。
//   - GPF3 を EINT3（立ち下がり・プルアップあり）、GPG0 を EINT8（High レベル・
//     プルアップなし）に設定する。-INTR（管理割り込み。負論理）が EINT3、カードの
//     IRQ（正論理）が EINT8 と判断した。
// TODO: バンク2 のうち上の 2 つの範囲の外（0x10000000〜の 16MB と 0x11000000〜の
// 64KB 以外）の配線は不明。オープンバスにしている。

/// バンク2 内の I/O 空間の位置。
const BANK2_IO: u32 = 0x0100_0000;
/// PD6710 の -INTR がつながる外部割り込み（EINT3）。
const EINT_PCIC_INTR: u32 = 3;
/// PD6710 の IRQ ピンがつながる外部割り込み（EINT8）。
const EINT_PCIC_IRQ: u32 = 8;
/// ボードの部品が駆動する外部割り込みのピン（それ以外は s3c2410::eint が無視する）。
const EINT_DRIVEN: u32 = 1 << EINT_PCIC_INTR | 1 << EINT_PCIC_IRQ;
/// 部品が何も要求していないときのピンのレベル（-INTR は High）。
const EINT_IDLE: u32 = 1 << EINT_PCIC_INTR;
/// EINT8 につながる PD6710 の IRQ ピン（ビット n = IRQn）: IRQ3。
/// 判断の根拠（2026-09-29 観察）: CIS の構成 2 の推奨は IRQ 14 だが、atadisk.dll の
/// 構成で Card IRQ Select に 3 が書かれた（ソケットのサービスが IRQ3 だけを
/// 使えるとしていると見られる）。管理割り込みは -INTR に向けられるので IRQ ピンは
/// 使われていない。TODO: 他の IRQ ピンの配線は不明（つながっていないとしている）。
const PCIC_IRQ_TO_EINT8: u16 = 1 << 3;

impl Board {
    /// 部品の状態から外部割り込みのピンのレベルを求める。
    fn eint_pin_levels(&self) -> u32 {
        let mut v = 0u32;
        if !self.pcic.intr() {
            v |= 1 << EINT_PCIC_INTR;
        }
        if self.pcic.irq_pins() & PCIC_IRQ_TO_EINT8 != 0 {
            v |= 1 << EINT_PCIC_IRQ;
        }
        v
    }

    /// ピンのレベル・GPIO の設定の変化を INTC に反映する。GPIO の書き込みと、
    /// ピンを駆動する部品の状態が変わり得る操作の後に呼ぶ。
    pub(crate) fn update_eint(&mut self) {
        let now = self.eint_pin_levels();
        let prev = self.eint_levels;
        self.eint_levels = now;
        let gpio = &mut self.stubs[StubId::Gpio as usize];
        let (edge, level) = eint::update(gpio, EINT_DRIVEN, prev, now);
        self.intc.raise_mask(edge);
        self.intc.set_level_sources(level);
    }

    /// スナップショットの読み込みの後: 派生情報（ピンのレベル・レベルのソース）を
    /// 求め直す。エッジは起きない（前回のレベル = いまのレベル）。
    pub(crate) fn restore_eint(&mut self) {
        self.eint_levels = self.eint_pin_levels();
        self.update_eint();
    }

    fn card_dyn(card: &mut Option<Slot>) -> Option<&mut dyn Card> {
        card.as_mut().map(|c| c.as_card())
    }

    /// バンク2 の読み出し（16 ビットのバス。32 ビットのアクセスは 2 回に分かれる）。
    fn bank2_read(&mut self, off: u32, size: u32) -> u32 {
        let v = if size == 4 {
            self.bank2_read16(off, true) as u32 | (self.bank2_read16(off + 2, true) as u32) << 16
        } else {
            self.bank2_read16(off, size == 2) as u32
        };
        self.update_eint();
        v
    }

    fn bank2_read16(&mut self, off: u32, wide: bool) -> u16 {
        let card = Self::card_dyn(&mut self.card);
        let v = if off < 0x0100_0000 {
            self.pcic.mem_read(card, off, wide)
        } else if (BANK2_IO..BANK2_IO + 0x1_0000).contains(&off) {
            self.pcic.io_read(card, (off - BANK2_IO) as u16, wide)
        } else {
            None
        };
        v.unwrap_or(0) // どの窓にも当たらなければオープンバス（読み 0）
    }

    fn bank2_write(&mut self, off: u32, size: u32, v: u32) {
        if size == 4 {
            self.bank2_write16(off, true, v as u16);
            self.bank2_write16(off + 2, true, (v >> 16) as u16);
        } else {
            self.bank2_write16(off, size == 2, v as u16);
        }
        self.update_eint();
    }

    fn bank2_write16(&mut self, off: u32, wide: bool, v: u16) {
        let card = Self::card_dyn(&mut self.card);
        if off < 0x0100_0000 {
            self.pcic.mem_write(card, off, wide, v);
        } else if (BANK2_IO..BANK2_IO + 0x1_0000).contains(&off) {
            self.pcic.io_write(card, (off - BANK2_IO) as u16, wide, v);
        }
    }

    /// フォルダ共有の挿抜を emulserv に知らせる（EINT11）。emulserv は EINT11 を待つが
    /// GPG3 を EINT11 の機能にしないので、EINTPEND に直接立てる（deshare.rs の先頭）。
    pub(crate) fn deshare_notify(&mut self) {
        let gpio = &mut self.stubs[StubId::Gpio as usize];
        let p = gpio.read(eint::EINTPEND, 4);
        gpio.write(eint::EINTPEND, 4, p | 1 << 11);
        self.update_eint();
    }

    /// カードを挿す（既に挿さっていればエラー）。
    pub(crate) fn insert_card(&mut self, card: Slot) -> Result<(), String> {
        if self.card.is_some() {
            return Err("a card is already inserted".into());
        }
        self.card = Some(card);
        self.pcic.set_inserted(true);
        self.pcic.sync(Self::card_dyn(&mut self.card));
        self.update_eint();
        Ok(())
    }

    /// カードを抜く（電源を切ってから手放す）。
    pub(crate) fn eject_card(&mut self) -> Option<Slot> {
        self.card.as_ref()?;
        self.pcic.set_inserted(false);
        if let Some(c) = self.card.as_mut() {
            c.as_card().set_power(false);
        }
        self.pcic.sync(None);
        self.update_eint();
        self.card.take()
    }

    /// カードの状態がバスのアクセス以外で変わった後（ネットワークからの受信）:
    /// コントローラと割り込み線に反映する。
    pub(crate) fn card_changed(&mut self) {
        self.pcic.sync(Self::card_dyn(&mut self.card));
        self.update_eint();
    }

    /// キーボード用マイコンに渡していないバイトが残っていて、EINT1 がマスクを外され
    /// 保留もないなら、次のバイトの EINT1 を上げる（kbd.rs のモデルのコメント）。
    /// 状態を持たない判定なので、取りこぼした状態で保存したスナップショットも
    /// 次のマスク解除で回復する。
    fn kbd_rearm(&mut self) {
        let bit = 1 << (INT_EINT0 + 1);
        if !self.kbd.out.is_empty()
            && self.intc.read(REG_INTMSK, 4) & bit == 0
            && self.intc.read(REG_SRCPND, 4) & bit == 0
        {
            self.intc.raise(INT_EINT0 + 1); // EINT1
        }
    }
}

/// IIS から送り出した音（16 ビット・左右交互）。フロントエンドが
/// [`Machine::take_audio`](super::Machine::take_audio) で取り出す。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudioChunk {
    /// 1 フレーム（左右 1 組）の PCLK ティック数（サンプリング周波数 = PCLK_HZ / これ）
    pub frame_ticks: u32,
    /// 左右交互のサンプル（IIS に送った順。先頭を左とする。TODO: 実機で左右の順を
    /// 確かめていない）
    pub samples: Vec<i16>,
}

/// 音の出力の溜め。capture が false の間は RAM を読まずに捨てる（出力はゲストから
/// 見えないので、取り出すかどうかで状態は変わらない）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AudioOut {
    pub(crate) capture: bool,
    pub(crate) chunks: Vec<AudioChunk>,
}

impl AudioOut {
    /// 溜めの上限（サンプル数。約 1 分の 44.1kHz ステレオ）。フロントエンドが取り出さ
    /// ないまま溜まり続けないように、超えたら古いものから捨てる。
    const LIMIT: usize = 44_100 * 2 * 60;

    fn chunk(&mut self, frame_ticks: u32) -> &mut Vec<i16> {
        if self
            .chunks
            .last()
            .is_none_or(|c| c.frame_ticks != frame_ticks)
        {
            self.chunks.push(AudioChunk {
                frame_ticks,
                samples: vec![],
            });
        }
        &mut self.chunks.last_mut().expect("pushed above").samples
    }

    /// CPU が IISFIFO に書いたサンプル。
    pub(crate) fn push_cpu(&mut self, s: u16, frame_ticks: u32) {
        if self.capture {
            self.chunk(frame_ticks).push(s as i16);
            self.trim();
        }
    }

    fn trim(&mut self) {
        let mut total: usize = self.chunks.iter().map(|c| c.samples.len()).sum();
        while total > Self::LIMIT && !self.chunks.is_empty() {
            let over = total - Self::LIMIT;
            let c = &mut self.chunks[0];
            if c.samples.len() <= over {
                total -= c.samples.len();
                self.chunks.remove(0);
            } else {
                c.samples.drain(..over);
                total -= over;
            }
        }
    }
}

impl Board {
    /// DMA が区切りを迎えた転送の中身を RAM から読んで音にする。区切りは DMA の
    /// イベント（CURR_TC が 0 になる期限。実行ループがその命令の直後に同期して読む）か、
    /// DMA・IIS のレジスタの書き込み（同じ命令のバスの書き込みの後、after_write で読む）で
    /// 生まれる。デバイスのアクセスの中の同期は期限の手前までしか進めない（CPU は実行中の
    /// 命令を数えない）ので、読み出しの中では生まれない（バスの読み出しに後処理を足すと
    /// 実行ループが 4〜5% 遅くなった。2026-09-30 の計測）。
    #[inline(always)]
    pub(crate) fn flush_audio(&mut self, ram: &mut dyn crate::bus::RamAccess) {
        if !self.dma.ready.is_empty() {
            self.flush_audio_segments(ram);
        }
    }

    #[inline(never)]
    fn flush_audio_segments(&mut self, ram: &mut dyn crate::bus::RamAccess) {
        for seg in std::mem::take(&mut self.dma.ready) {
            if !self.audio.capture || seg.size != 2 {
                continue; // TODO: バイト・ワードの転送の音（dma.rs の TODO）
            }
            let len = if seg.inc { seg.units * 2 } else { 2 };
            let Some(m) = ram.ram_slice_mut(seg.src, len) else {
                continue; // TODO: RAM 以外からの転送（今の構成では起きない）
            };
            let out = self.audio.chunk(seg.frame_ticks);
            if seg.inc {
                out.extend(m.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)));
            } else {
                let s = i16::from_le_bytes([m[0], m[1]]);
                out.extend(std::iter::repeat_n(s, seg.units as usize));
            }
        }
        self.audio.trim();
    }
}

/// SPI1 のキーボード用マイコン（SPI の転送中に使う）。
struct KbdPort<'a> {
    kbd: &'a mut KbdMcu,
}

impl SpiSlaves for KbdPort<'_> {
    fn transfer(&mut self, ch: usize, tx: u8) -> Option<u8> {
        if ch != 1 {
            return None; // SPI0 には何もつながっていない
        }
        Some(self.kbd.transfer(tx))
    }
}

impl Devices<Dev> for Board {
    fn read(&mut self, dev: Dev, off: u32, size: u32) -> u32 {
        match dev {
            Dev::OpenBus => 0,
            Dev::Bank2 => self.bank2_read(off, size),
            Dev::DeShare => self.deshare.read((off & !3) + 0x4000) >> ((off & 3) * 8),
            // 中身の読みはバスが直接行う（Bus::map_flash）
            Dev::Flash => 0,
            Dev::Gpio => self.stubs[StubId::Gpio as usize].read(off, size),
            Dev::Uart(n) => self.uart[n as usize].read(off, size),
            Dev::Intc => self.intc.read(off, size),
            Dev::Lcd => self.lcd.read(off, size),
            Dev::Spi => self.spi.read(off, size),
            Dev::Stub(s) => self.stubs[s as usize].read(off, size),
            // 時間を持つデバイス: アクセスの前に溜めたティックを
            // 渡し、後で期限を求め直す（読み出しで変換が始まる ADC の READ_START の
            // ように、読み出しも状態を変え得るため）。
            Dev::Timer | Dev::Rtc | Dev::Adc | Dev::Dma | Dev::Iis => {
                self.sync_time();
                let v = match dev {
                    Dev::Timer => self.timer.read(off, size),
                    Dev::Rtc => self.rtc.read(off, size),
                    Dev::Dma => self.dma.read(off, size),
                    Dev::Iis => self.iis.read(off, size),
                    _ => self.adc.read(off, size),
                };
                self.update_deadline();
                // 読み出しの中で進める時間は期限の手前まで（実行中の命令は数えない）
                // なので、DMA の区切り（RAM を読む）はここでは生まれない（flush_audio）。
                debug_assert!(self.dma.ready.is_empty());
                v
            }
        }
    }

    fn write(&mut self, dev: Dev, off: u32, size: u32, v: u32) {
        match dev {
            Dev::OpenBus => {}
            Dev::Bank2 => self.bank2_write(off, size, v),
            // TODO: 32 ビット以外の書き込み（ゲストのドライバは 32 ビットだけを使う）
            Dev::DeShare => self.deshare.write((off & !3) + 0x4000, v),
            Dev::Flash => {
                if let Some(f) = &mut self.flash {
                    f.write(off, size, v);
                }
            }
            Dev::Gpio => {
                let gpio = &mut self.stubs[StubId::Gpio as usize];
                if off & !3 == eint::EINTPEND {
                    // 1 を書いたビットをクリアする（データシート 9-27）
                    let cur = gpio.read(eint::EINTPEND, 4);
                    let mask = match size {
                        1 => (v & 0xFF) << ((off & 3) * 8),
                        2 => (v & 0xFFFF) << ((off & 2) * 8),
                        _ => v,
                    };
                    gpio.write(eint::EINTPEND, 4, cur & !mask);
                } else {
                    gpio.write(off, size, v);
                }
                self.update_eint();
            }
            Dev::Uart(n) => self.uart[n as usize].write(off, size, v),
            Dev::Intc => {
                self.intc.write(off, size, v);
                self.kbd_rearm();
            }
            Dev::Lcd => self.lcd.write(off, size, v),
            Dev::Spi => {
                let mut port = KbdPort { kbd: &mut self.kbd };
                if let Some(ch) = self.spi.write(off, size, v, &mut port) {
                    self.intc.raise([INT_SPI0, INT_SPI1][ch]);
                }
            }
            Dev::Stub(s) => self.stubs[s as usize].write(off, size, v),
            Dev::Timer | Dev::Rtc | Dev::Adc | Dev::Dma | Dev::Iis => {
                self.sync_time();
                match dev {
                    Dev::Timer => self.timer.write(off, size, v),
                    Dev::Rtc => self.rtc.write(off, size, v),
                    // DMA と IIS の書き込みは転送の要求を変え得るので、続けて処理する
                    // （FIFO に空きがあれば瞬時に埋まる）。
                    Dev::Dma | Dev::Iis => {
                        if dev == Dev::Dma {
                            let ft = self.iis.frame_ticks();
                            self.dma.write(off, size, v, ft);
                        } else if let Some(s) = self.iis.write(off, size, v) {
                            let ft = self.iis.frame_ticks();
                            self.audio.push_cpu(s, ft);
                        }
                        let ints = self.dma.service_iis_tx(&mut self.iis);
                        self.intc.raise_mask(ints << INT_DMA0);
                    }
                    _ => {
                        let subs = self.adc.write(off, size, v);
                        self.intc.raise_sub_mask(subs);
                    }
                }
                self.update_deadline();
            }
        }
    }

    fn after_write(&mut self, dev: Dev, ram: &mut dyn crate::bus::RamAccess) {
        self.flush_audio(ram);
        if dev == Dev::Flash
            && let Some(f) = &mut self.flash
            && f.pending.is_some()
        {
            // フラッシュは PA 0 から（smdk2410 の map）
            if let Some((off, len)) = ram.ram_slice_mut(0, f.size()).and_then(|d| f.apply(d)) {
                ram.changed(off, len);
            }
        }
        if dev == Dev::DeShare {
            // 新しい項目の日時はゲストの RTC の今の時刻（決定論的）
            self.sync_time();
            let t = self.rtc.now();
            let now = super::deshare::dos_time(
                t.year,
                t.month as i64,
                t.day as i64,
                t.hour as i64,
                t.minute as i64,
                t.second as i64,
            );
            self.deshare.after_write(ram, now);
        }
    }

    fn flash_array(&self, dev: Dev) -> bool {
        dev != Dev::Flash || self.flash.as_ref().is_none_or(|f| f.array())
    }

    fn flash_read(&mut self, dev: Dev, off: u32, size: u32) -> Option<u32> {
        match dev {
            Dev::Flash => self.flash.as_ref()?.id_read(off, size),
            _ => None,
        }
    }

    fn stable_read(&mut self, dev: Dev, off: u32, size: u32) -> Option<u32> {
        match dev {
            // 時間を同期してから読むので read と同じ値になる（同期は状態の見え方を
            // 変えない）。副作用のない読み出しでは期限を求め直さない。
            Dev::Adc => {
                self.sync_time();
                self.adc.stable_read(off, size)
            }
            _ => None,
        }
    }
}
