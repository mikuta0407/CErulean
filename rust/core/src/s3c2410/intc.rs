//! 割り込みコントローラ（データシート Ch.14）。

/// S3C2410 の割り込みコントローラ。
///
/// デバイス → raise/raise_sub → SRCPND/SUBSRCPND → マスク・モード判定 →
/// INTPND/INTOFFSET 確定 → 割り込み線（irq/fiq フィールド）。CPU は命令境界で
/// irq/fiq を読む（Go では update コールバックで CPU の線を動かしていた）。
///
/// 簡略化（ユーザー確認済み 2026-09）: PRIORITY レジスタの回転アービトレーション
/// は実装せず、固定優先度（ビット番号の小さい順）で選択する。
/// TODO: 実機依存の挙動が問題になったら ARB_SEL/ARB_MODE を実装する。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intc {
    pub(crate) srcpnd: u32,
    pub(crate) intmod: u32,
    pub(crate) intmsk: u32,
    pub(crate) priority: u32,
    pub(crate) intpnd: u32,
    pub(crate) intoffset: u32,
    pub(crate) subsrcpnd: u32,
    pub(crate) intsubmsk: u32,
    /// レベルで要求し続けているソース（外部割り込み。s3c2410::eint）。立っている間は
    /// SRCPND をクリアしても立て直す。GPIO と部品の状態から決まる派生情報（保存しない。
    /// 読み込み後にボードが求め直す）
    pub(crate) level_src: u32,
    /// CPU の IRQ 線のレベル（最後の recompute の結果）
    pub(crate) irq: bool,
    /// CPU の FIQ 線のレベル
    pub(crate) fiq: bool,
}

// 割り込みソース番号（SRCPND のビット位置。データシート Table 14-2）。
pub const INT_EINT0: u32 = 0;
pub const INT_TICK: u32 = 8;
pub const INT_WDT: u32 = 9;
pub const INT_TIMER0: u32 = 10;
pub const INT_TIMER1: u32 = 11;
pub const INT_TIMER2: u32 = 12;
pub const INT_TIMER3: u32 = 13;
pub const INT_TIMER4: u32 = 14;
pub const INT_UART2: u32 = 15;
pub const INT_LCD: u32 = 16;
pub const INT_DMA0: u32 = 17;
pub const INT_DMA1: u32 = 18;
pub const INT_DMA2: u32 = 19;
pub const INT_DMA3: u32 = 20;
pub const INT_SPI0: u32 = 22; // SPI0/1 のビット番号は User's Manual Rev 1.1 で確認済み
pub const INT_UART1: u32 = 23;
pub const INT_UART0: u32 = 28;
pub const INT_SPI1: u32 = 29;
pub const INT_RTC: u32 = 30;
pub const INT_ADC: u32 = 31;

// SUBSRCPND のビット位置（UART の送受信・エラー、ADC）。
pub const SUB_RXD0: u32 = 0;
pub const SUB_TXD0: u32 = 1;
pub const SUB_ERR0: u32 = 2;
pub const SUB_RXD1: u32 = 3;
pub const SUB_TXD1: u32 = 4;
pub const SUB_ERR1: u32 = 5;
pub const SUB_RXD2: u32 = 6;
pub const SUB_TXD2: u32 = 7;
pub const SUB_ERR2: u32 = 8;
pub const SUB_TC: u32 = 9;
pub const SUB_ADC: u32 = 10;

pub(crate) const REG_SRCPND: u32 = 0x00;
pub(crate) const REG_INTMOD: u32 = 0x04;
pub(crate) const REG_INTMSK: u32 = 0x08;
pub(crate) const REG_PRIORITY: u32 = 0x0C;
pub(crate) const REG_INTPND: u32 = 0x10;
pub(crate) const REG_INTOFFSET: u32 = 0x14;
pub(crate) const REG_SUBSRCPND: u32 = 0x18;
pub(crate) const REG_INTSUBMSK: u32 = 0x1C;

impl Default for Intc {
    fn default() -> Self {
        Self::new()
    }
}

impl Intc {
    /// リセット値: INTMSK/INTSUBMSK は全マスク（データシート）。
    pub fn new() -> Intc {
        Intc {
            srcpnd: 0,
            intmod: 0,
            intmsk: 0xFFFFFFFF,
            priority: 0,
            intpnd: 0,
            intoffset: 0,
            subsrcpnd: 0,
            intsubmsk: 0x7FF,
            level_src: 0,
            irq: false,
            fiq: false,
        }
    }

    /// CPU の IRQ 線のレベル。
    pub fn irq(&self) -> bool {
        self.irq
    }

    /// CPU の FIQ 線のレベル。
    pub fn fiq(&self) -> bool {
        self.fiq
    }

    /// 割り込みソース src（ビット番号）のペンディングを立てる。
    /// エッジ相当: 立てるだけで、クリアはハンドラの SRCPND 書き込みが行う。
    pub fn raise(&mut self, src: u32) {
        self.srcpnd |= 1 << (src & 31);
        self.recompute();
    }

    /// 複数のソースをまとめて立てる（デバイスが返したビットの束）。
    /// 1 個ずつ raise するのと同じ結果（recompute は最後の状態だけで決まる）。
    pub fn raise_mask(&mut self, mask: u32) {
        if mask != 0 {
            self.srcpnd |= mask;
            self.recompute();
        }
    }

    /// UART/ADC のサブソースを立てる（sub はビットの束）。
    pub fn raise_sub_mask(&mut self, mask: u32) {
        if mask != 0 {
            self.subsrcpnd |= mask;
            self.recompute();
        }
    }

    /// レベルで要求し続けるソースの束を置き換える（外部割り込み）。
    pub fn set_level_sources(&mut self, mask: u32) {
        if mask != self.level_src {
            self.level_src = mask;
            self.recompute();
        }
    }

    /// サブソースの束をメインソースのビットに反映する。サブが立っている限り
    /// SRCPND 側は立て直される（クリアしてもサブが残っていれば再セット）。
    fn sub_to_src(&mut self) {
        self.srcpnd |= self.level_src;
        let pend = self.subsrcpnd & !self.intsubmsk;
        if pend & 0x007 != 0 {
            self.srcpnd |= 1 << INT_UART0;
        }
        if pend & 0x038 != 0 {
            self.srcpnd |= 1 << INT_UART1;
        }
        if pend & 0x1C0 != 0 {
            self.srcpnd |= 1 << INT_UART2;
        }
        if pend & 0x600 != 0 {
            self.srcpnd |= 1 << INT_ADC;
        }
    }

    /// ペンディング状態から INTPND/INTOFFSET と IRQ/FIQ 線を求め直す。
    fn recompute(&mut self) {
        self.sub_to_src();
        let fiq = self.srcpnd & self.intmod; // FIQ は INTMSK の影響を受けない
        let irq_pend = self.srcpnd & !self.intmsk & !self.intmod;
        if irq_pend != 0 {
            // 固定優先度: ビット番号の小さい順（PRIORITY は未実装）。
            let off = irq_pend.trailing_zeros();
            self.intpnd = 1 << off;
            self.intoffset = off;
        } else {
            self.intpnd = 0;
            // INTOFFSET は最後の値を保持する（データシート: INTPND クリアで更新）。
        }
        self.irq = self.intpnd != 0;
        self.fiq = fiq != 0;
    }

    pub fn read(&self, off: u32, _size: u32) -> u32 {
        match off & !3 {
            REG_SRCPND => self.srcpnd,
            REG_INTMOD => self.intmod,
            REG_INTMSK => self.intmsk,
            REG_PRIORITY => self.priority,
            REG_INTPND => self.intpnd,
            REG_INTOFFSET => self.intoffset,
            REG_SUBSRCPND => self.subsrcpnd,
            REG_INTSUBMSK => self.intsubmsk,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, _size: u32, v: u32) {
        match off & !3 {
            REG_SRCPND => self.srcpnd &= !v, // 1 を書いたビットをクリア
            REG_INTMOD => self.intmod = v,
            REG_INTMSK => self.intmsk = v,
            REG_PRIORITY => self.priority = v, // 保持のみ（固定優先度で代用）
            REG_INTPND => self.intpnd &= !v,   // 1 を書いたビットをクリア
            REG_INTOFFSET => {}                // 書き込み不可レジスタ。無視。
            REG_SUBSRCPND => self.subsrcpnd &= !v,
            REG_INTSUBMSK => self.intsubmsk = v,
            _ => {}
        }
        self.recompute();
    }
}
