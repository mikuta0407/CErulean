//! PC カードコントローラ（Cirrus Logic CL-PD6710。Intel 82365SL 互換のレジスタ）。
//!
//! 一次資料: CL-PD6710/'22 Preliminary Data Sheet v3.1（1997-05。以下「DS」）。
//! 82365SL 自体のデータシートは入手できなかったので、82365SL 互換
//! （Register Compatibility Type: 365）のレジスタも DS の記述に従う。
//!
//! ホスト側（ISA バス）から見た入口は 3 つ（ボードがアドレスを振り分ける）:
//!   - I/O ポート 0x3E0（Index）・0x3E1（Data）: 内部レジスタ（DS 5 章）
//!   - I/O 窓 0〜1: ISA の I/O アドレス → カードの I/O 空間（DS 7 章）
//!   - メモリ窓 0〜4: ISA のメモリアドレス → カードの属性／共通メモリ（DS 8 章）
//!
//! 出力は 2 系統（DS 3.1.4）: 管理割り込み（カードの状態変化）と、カードの
//! 割り込み（RDY/-IREQ）。どちらも IRQ ピンのどれか、管理割り込みは -INTR にも
//! 出せる。どのピンがどこに配線されているかはボードが決める。
//!
//! 1 ソケット（CL-PD6710）。ソケット B（インデックス 0x40〜0x7F）とデバイス 1
//! （0x80〜0xFF）は存在しない。
//! TODO: 存在しないソケット・デバイスのレジスタを読んだときの値は DS に記載が
//! ない。0（ボードのオープンバスと同じ）を返している。
//!
//! 簡略化:
//!   - アクセスのタイミング（Setup/Command/Recovery・WAIT）は値を保持するだけ。
//!     書き込み FIFO は常に空（書き込みは即座にカードに届く）。
//!   - パルスモードの割り込み（Misc Control 1 の bit2・bit3）は未実装（レベルとして
//!     出す）。TODO: ドライバが使ったら実装する。
//!   - 低消費電力・サスペンド（Misc Control 2）・DMA・GPSTB・電圧検出（VS）は
//!     値を保持するだけ。

use crate::snapshot::{Decoder, Encoder, Error};

/// カードの空間（-REG と、メモリ／I/O のサイクルの別）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space {
    /// 属性メモリ（-REG 有効のメモリサイクル）
    Attr,
    /// 共通メモリ（-REG 無効のメモリサイクル）
    Common,
    /// I/O サイクル（-REG 有効の -IORD/-IOWR）
    Io,
}

/// カード側の口（CompactFlash・将来の NE2000 等）。アドレスはカードの空間の
/// アドレス（メモリは 26 ビット、I/O は 16 ビット）。wide はワード（16 ビット、
/// -CE1・-CE2 とも有効。偶数番地）、そうでなければバイト（-CE1 のみ。A0 で偶奇）。
pub trait Card {
    fn read(&mut self, space: Space, addr: u32, wide: bool) -> u16;
    fn write(&mut self, space: Space, addr: u32, wide: bool, v: u16);
    /// RDY/-IREQ ピン。メモリカードのインタフェースでは RDY（true = Ready）、
    /// I/O カードのインタフェースでは -IREQ を反転した値（true = 割り込み要求）。
    fn rdy_ireq(&self) -> bool;
    /// I/O サイクルでの -IOIS16（true = 16 ビットのポート）。
    fn iois16(&self, addr: u32) -> bool;
    /// RESET ピン（true = リセット中）。電源が入っていない間も true として扱う。
    fn set_reset(&mut self, reset: bool);
    /// 電源の投入（true）・切断（false）。切断で状態は初期化される。
    fn set_power(&mut self, on: bool);
    /// BVD1/-STSCHG・BVD2/-SPKR（Interface Status の bit1:0 に出るピン）。
    fn bvd(&self) -> u8 {
        0b11
    }
}

// ソケットごとのレジスタのインデックス（DS Table 5-1）。
const REG_CHIP_REV: usize = 0x00;
const REG_IF_STATUS: usize = 0x01;
const REG_POWER: usize = 0x02;
const REG_INT_GEN: usize = 0x03;
const REG_CSC: usize = 0x04;
const REG_MGMT_INT: usize = 0x05;
const REG_MAP_ENABLE: usize = 0x06;
const REG_IO_WIN_CTL: usize = 0x07;
const REG_MISC1: usize = 0x16;
const REG_FIFO: usize = 0x17;
const REG_MISC2: usize = 0x1E;
const REG_CHIP_INFO: usize = 0x1F;

// Power Control（DS 6.3）
const PWR_CARD_ENABLE: u8 = 1 << 7;
const PWR_AUTO: u8 = 1 << 5;
const PWR_VCC: u8 = 1 << 4;
// Interrupt and General Control（DS 6.4）
const IGC_RESET_INACTIVE: u8 = 1 << 6;
const IGC_CARD_IS_IO: u8 = 1 << 5;
const IGC_MGMT_TO_INTR: u8 = 1 << 4;
// Card Status Change（DS 6.5）
const CSC_CARD_DETECT: u8 = 1 << 3;
const CSC_READY: u8 = 1 << 2;

/// Chip Revision（DS 6.1）: Interface ID=10（メモリと I/O）、Revision=0010
/// （82365SL A-step 互換）。pcc_smdk2410.dll は 0x82〜0x84 でコントローラありと
/// 判定する（2026-09-29 観察）。
const CHIP_REVISION: u8 = 0x82;

/// CL-PD6710 本体（ソケット A にカードを 1 枚挿せる）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pd6710 {
    /// Index レジスタ（DS 5.1。保存する）
    pub(crate) index: u8,
    /// ソケット A の 64 個のレジスタの保持値（保存する）。読み出しで作る値
    /// （Interface Status 等）は read_reg が組み立てる。
    pub(crate) regs: [u8; 64],
    /// Chip Information の識別ビットのトグル（DS 9.4。true なら次の読み出しで 11）
    pub(crate) chip_info_toggle: bool,
    /// ソケットに挿さっているか（-CD1/-CD2。カードの中身とは別に持つ。保存する）
    pub(crate) inserted: bool,
    /// 直前の RDY/-IREQ（Ready Change の検出用。保存する）
    pub(crate) last_rdy: bool,
    /// カードに電源が入っているか（直前の判定。電源の入り切りの検出用。保存する）
    pub(crate) powered: bool,
    /// カードの RESET ピンの状態（直前の判定。保存する）
    pub(crate) reset: bool,
}

impl Default for Pd6710 {
    fn default() -> Self {
        Self::new()
    }
}

impl Pd6710 {
    pub const STATE_VERSION: u16 = 1;

    pub fn new() -> Pd6710 {
        let mut regs = [0u8; 64];
        // リセット値（DS の各レジスタの表）。Misc Control 2 は Low-Power Dynamic
        // Mode=1、FIFO Control は空（bit7 は読み出し時に作る）。他は 0。
        regs[REG_MISC2] = 0x02;
        Pd6710 {
            index: 0,
            regs,
            chip_info_toggle: true,
            inserted: false,
            last_rdy: false,
            powered: false,
            reset: true,
        }
    }

    // ---- ピンの状態 ----

    /// VCC がカードに供給されているか（DS Table 6-1）。PWRGOOD は常に High とする。
    fn vcc_on(&self) -> bool {
        let p = self.regs[REG_POWER];
        p & PWR_VCC != 0 && (p & PWR_AUTO == 0 || self.inserted)
    }

    /// カードへの出力（アドレス・データ・制御線）が有効か（DS Table 6-2）。
    fn outputs_enabled(&self) -> bool {
        self.inserted && self.vcc_on() && self.regs[REG_POWER] & PWR_CARD_ENABLE != 0
    }

    /// カードの RESET が有効か。Card Enable が 0 の間 RESET は高インピーダンス
    /// （DS 6.4 の bit6）。TODO: 高インピーダンスの RESET をカードがどう受けるかは
    /// DS に記載がない。リセット中として扱う。
    fn reset_active(&self) -> bool {
        !self.outputs_enabled() || self.regs[REG_INT_GEN] & IGC_RESET_INACTIVE == 0
    }

    fn card_is_io(&self) -> bool {
        self.regs[REG_INT_GEN] & IGC_CARD_IS_IO != 0
    }

    /// カードの状態の変化（挿抜・電源・リセット・RDY）を反映する。カードへの
    /// アクセス・レジスタの書き込み・挿抜の後に呼ぶ。
    pub fn sync(&mut self, card: Option<&mut dyn Card>) {
        let Some(card) = card.filter(|_| self.inserted) else {
            self.powered = false;
            self.reset = true;
            self.last_rdy = false;
            return;
        };
        let on = self.vcc_on();
        if on != self.powered {
            self.powered = on;
            card.set_power(on);
        }
        let rst = self.reset_active();
        if rst != self.reset {
            self.reset = rst;
            card.set_reset(rst);
        }
        let rdy = self.powered && card.rdy_ireq();
        if rdy != self.last_rdy {
            self.last_rdy = rdy;
            // Ready Change はメモリカードのインタフェースのときだけ（DS 6.5 の bit2）
            if !self.card_is_io() {
                self.regs[REG_CSC] |= CSC_READY;
            }
        }
    }

    /// カードの挿入・抜去（-CD1/-CD2 の変化）。Card Detect Change を立てる。
    /// 呼び出し側は、抜いたカードの電源を切ってから（sync で）手放すこと。
    pub fn set_inserted(&mut self, inserted: bool) {
        if inserted != self.inserted {
            self.inserted = inserted;
            self.regs[REG_CSC] |= CSC_CARD_DETECT;
        }
    }

    pub fn inserted(&self) -> bool {
        self.inserted
    }

    // ---- 割り込みの出力 ----

    /// 管理割り込みの要求（Card Status Change の立っているビットのうち、
    /// Management Interrupt Configuration で有効なもの。DS 6.5・6.6）。
    fn mgmt_pending(&self) -> bool {
        self.regs[REG_CSC] & self.regs[REG_MGMT_INT] & 0x0F != 0
    }

    /// -INTR ピンが有効（Low）か。管理割り込みを -INTR に向けたとき（DS 6.4 の bit4）。
    pub fn intr(&self) -> bool {
        self.regs[REG_INT_GEN] & IGC_MGMT_TO_INTR != 0 && self.mgmt_pending()
    }

    /// 有効（High）になっている IRQ ピンの集合（ビット n = IRQn）。
    /// 管理割り込み（-INTR に向けていないとき）と、I/O カードの割り込み。
    pub fn irq_pins(&self) -> u16 {
        // IRQ の選択値のうち予約（0001・0010・0110・1000・1101）は出さない（DS 6.4・6.6）
        const VALID: u16 = 0b1101_1110_1011_1000;
        let mut pins = 0u16;
        let ig = self.regs[REG_INT_GEN];
        if ig & IGC_MGMT_TO_INTR == 0 && self.mgmt_pending() {
            pins |= 1 << (self.regs[REG_MGMT_INT] >> 4);
        }
        if self.card_is_io() && self.powered && self.last_rdy {
            pins |= 1 << (ig & 0x0F);
        }
        pins & VALID
    }

    // ---- ホストからのアクセス ----

    /// I/O ポートの読み出し（8 ビットまたは 16 ビット）。どの窓にも当たらなければ
    /// None（ボードがオープンバスにする）。
    pub fn io_read(&mut self, card: Option<&mut dyn Card>, port: u16, wide: bool) -> Option<u16> {
        match port {
            0x3E0 => return Some(self.index as u16),
            0x3E1 => return Some(self.read_reg(card)),
            _ => {}
        }
        let (addr, wide) = self.io_window(port, wide, card.as_deref())?;
        let card = card.filter(|_| self.outputs_enabled())?;
        let v = card.read(Space::Io, addr, wide);
        // 読み出しでもカードの状態（割り込み要求等）が変わる
        self.sync(Some(card));
        Some(v)
    }

    pub fn io_write(&mut self, card: Option<&mut dyn Card>, port: u16, wide: bool, v: u16) {
        match port {
            0x3E0 => {
                self.index = v as u8;
                return;
            }
            0x3E1 => {
                self.write_reg(card, v as u8);
                return;
            }
            _ => {}
        }
        let Some((addr, wide)) = self.io_window(port, wide, card.as_deref()) else {
            return;
        };
        if let Some(card) = card.filter(|_| self.outputs_enabled()) {
            card.write(Space::Io, addr, wide, v);
            self.sync(Some(card));
        }
    }

    /// I/O 窓の引き当て（DS 7 章）。戻り値はカードの I/O アドレスとワードか。
    fn io_window(&self, port: u16, wide: bool, card: Option<&dyn Card>) -> Option<(u32, bool)> {
        let en = self.regs[REG_MAP_ENABLE];
        let ctl = self.regs[REG_IO_WIN_CTL];
        for w in 0..2usize {
            if en & (0x40 << w) == 0 {
                continue;
            }
            let b = 0x08 + 4 * w;
            let start = u16::from_le_bytes([self.regs[b], self.regs[b + 1]]);
            let end = u16::from_le_bytes([self.regs[b + 2], self.regs[b + 3]]);
            if port < start || port > end {
                continue;
            }
            let o = 0x36 + 2 * w;
            // Offset の bit0 は 0 固定（DS 7.6）
            let off = u16::from_le_bytes([self.regs[o] & 0xFE, self.regs[o + 1]]);
            let addr = port.wrapping_add(off) as u32;
            let c = ctl >> (4 * w);
            // 窓の幅: Auto-Size なら -IOIS16、そうでなければ Size ビット（DS 7.1）
            let w16 = if c & 0x02 != 0 {
                card.is_some_and(|c| c.iois16(addr))
            } else {
                c & 0x01 != 0
            };
            return Some((addr, wide && w16));
        }
        None
    }

    /// メモリ窓の引き当て（DS 8 章）。isa は ISA のメモリアドレス（24 ビット）。
    /// 戻り値はカードの空間・アドレス・16 ビット窓か・書き込み禁止か。
    ///
    /// TODO: DS 8 章は「メモリ窓は先頭 64KB に置けない（0x010000〜0xFFFFFF）」と
    /// するが、pcc_smdk2410.dll は System Start=0x000000 の窓で PA 0x10000000
    /// （= ISA 0）から CIS を読む（2026-09-29 観察）。Start/End の A23:12 の比較だけで
    /// 判定している（先頭 64KB も応答する）。
    fn mem_window(&self, isa: u32) -> Option<(Space, u32, bool, bool)> {
        let en = self.regs[REG_MAP_ENABLE];
        let page = isa >> 12;
        for w in 0..5usize {
            if en & (1 << w) == 0 {
                continue;
            }
            let b = 0x10 + 8 * w;
            let start = (self.regs[b] as u32) | ((self.regs[b + 1] as u32 & 0x0F) << 8);
            let end = (self.regs[b + 2] as u32) | ((self.regs[b + 3] as u32 & 0x0F) << 8);
            if page < start || page > end {
                continue;
            }
            let off = (self.regs[b + 4] as u32) | ((self.regs[b + 5] as u32 & 0x3F) << 8);
            let addr = (isa.wrapping_add(off << 12)) & 0x03FF_FFFF;
            let reg = self.regs[b + 5] & 0x40 != 0;
            let wp = self.regs[b + 5] & 0x80 != 0;
            let w16 = self.regs[b + 1] & 0x80 != 0;
            let space = if reg { Space::Attr } else { Space::Common };
            return Some((space, addr, w16, wp));
        }
        None
    }

    /// メモリの読み出し（8 ビットまたは 16 ビット）。どの窓にも当たらなければ None。
    /// 8 ビットの窓への 16 ビットのアクセスは、偶数・奇数の 2 回のバイトアクセスにする。
    /// TODO: 実機ではホストのバスサイジング（ISA の MEMCS16*）で分割される。
    /// S3C2410 のバンク2 との間で同じになるかは未確認。
    pub fn mem_read(&mut self, card: Option<&mut dyn Card>, isa: u32, wide: bool) -> Option<u16> {
        let (space, addr, w16, _) = self.mem_window(isa)?;
        let card = card.filter(|_| self.outputs_enabled())?;
        let v = if !wide || w16 {
            card.read(space, addr, wide)
        } else {
            let lo = card.read(space, addr, false) & 0xFF;
            let hi = card.read(space, addr | 1, false) & 0xFF;
            lo | hi << 8
        };
        self.sync(Some(card));
        Some(v)
    }

    pub fn mem_write(&mut self, card: Option<&mut dyn Card>, isa: u32, wide: bool, v: u16) {
        let Some((space, addr, w16, wp)) = self.mem_window(isa) else {
            return;
        };
        // Write Protect はメモリカードのインタフェースのときだけ有効（DS 8.6）
        if wp && !self.card_is_io() {
            return;
        }
        let Some(card) = card.filter(|_| self.outputs_enabled()) else {
            return;
        };
        if !wide || w16 {
            card.write(space, addr, wide, v);
        } else {
            card.write(space, addr, false, v & 0xFF);
            card.write(space, addr | 1, false, v >> 8);
        }
        self.sync(Some(card));
    }

    /// Data レジスタの読み出し（DS 5.2 と各レジスタの表）。
    fn read_reg(&mut self, card: Option<&mut dyn Card>) -> u16 {
        if self.index >= 0x40 {
            return 0; // ソケット B・デバイス 1 は無い（モジュールの先頭の TODO）
        }
        let i = self.index as usize;
        let v = match i {
            REG_CHIP_REV => CHIP_REVISION,
            REG_IF_STATUS => self.interface_status(card.as_deref()),
            REG_CSC => {
                // 読むとクリアされる（DS 6.5）
                let v = self.regs[REG_CSC];
                self.regs[REG_CSC] = 0;
                v
            }
            // Misc Control 1 の bit0（5 V Detect）は VS1/VS2 ピン。CompactFlash は
            // 3.3V/5V 両用、カードなしはピンが浮く（プルアップ）ので 1（DS 9.1）。
            // TODO: 3.3V 専用のカードを足すときはカードから受け取る。
            REG_MISC1 => self.regs[REG_MISC1] & 0xFE | 0x01,
            // FIFO Control の bit7: 書き込み FIFO は常に空（モジュールの先頭の簡略化）
            REG_FIFO => self.regs[REG_FIFO] & 0x7F | 0x80,
            REG_CHIP_INFO => {
                // bit7:6 は書き込み（とリセット）の後の最初の読み出しで 11、次は 00
                // と交互に変わる（DS 9.4）。bit5=0（1 ソケット）。
                // TODO: bit4:1（Revision Level）は「初期値 111」とあるが 4 ビットの
                // 欄で、版ごとに異なるとも書かれている。0 としている。
                let t = self.chip_info_toggle;
                self.chip_info_toggle = !t;
                if t { 0xC0 } else { 0x00 }
            }
            _ => self.regs[i],
        };
        v as u16
    }

    /// Interface Status（DS 6.2）: ピンの状態から組み立てる。
    fn interface_status(&self, card: Option<&dyn Card>) -> u8 {
        let mut v = 0u8;
        if self.powered {
            v |= 1 << 6; // Card Power On
        }
        if self.inserted {
            v |= 0b1100; // -CD1・-CD2 がともに Low
            if let Some(c) = card
                && self.powered
            {
                v |= c.bvd() & 0b11;
                if c.rdy_ireq() {
                    v |= 1 << 5;
                }
            }
        }
        // TODO: カードの電源が入っていないときの BVD・RDY・WP のピンの値は DS に
        // 記載がない（0 としている）。-VPP_VALID（bit7）も 0（VPP は使わない）。
        v
    }

    fn write_reg(&mut self, card: Option<&mut dyn Card>, v: u8) {
        if self.index >= 0x40 {
            return;
        }
        let i = self.index as usize;
        match i {
            // 読み出し専用（DS 6.1・6.2・6.5）
            REG_CHIP_REV | REG_IF_STATUS | REG_CSC => {}
            REG_CHIP_INFO => self.chip_info_toggle = true,
            // Misc Control 1 の bit0 は読み出し専用（W:0）
            REG_MISC1 => self.regs[i] = v & 0xFE,
            // FIFO Control の bit7 の書き込みは FIFO の破棄（常に空なので何もしない）
            REG_FIFO => self.regs[i] = v & 0x7F,
            // Card I/O Map Offset Low の bit0 は 0 固定（DS 7.6）
            0x36 | 0x38 => self.regs[i] = v & 0xFE,
            _ => self.regs[i] = v,
        }
        self.sync(card);
    }

    // ---- スナップショット ----

    pub fn save_state(&self, e: &mut Encoder) {
        let Pd6710 {
            index,
            regs,
            chip_info_toggle,
            inserted,
            last_rdy,
            powered,
            reset,
        } = self;
        e.u8(*index);
        e.bytes(regs);
        e.bool(*chip_info_toggle);
        e.bool(*inserted);
        e.bool(*last_rdy);
        e.bool(*powered);
        e.bool(*reset);
    }

    pub fn load_state(&mut self, d: &mut Decoder) -> Result<(), Error> {
        self.index = d.u8()?;
        let regs = d.bytes()?;
        if regs.len() != self.regs.len() {
            return d.err("bad register count");
        }
        self.regs.copy_from_slice(regs);
        self.chip_info_toggle = d.bool()?;
        self.inserted = d.bool()?;
        self.last_rdy = d.bool()?;
        self.powered = d.bool()?;
        self.reset = d.bool()?;
        Ok(())
    }
}
