//! 外部割り込み（EINT0〜23）の検出（データシート Ch.9 の EXTINTn・EINTMASK・EINTPEND）。
//!
//! GPIO のレジスタは値保持スタブ（board.rs）が持ち、ここはピンのレベルの変化と
//! レジスタの値から割り込みを求める関数だけを置く。
//!
//! 対象はボードの部品が駆動するピンだけ（driven）。それ以外のピンはモデル化して
//! いないので、レベルの設定（Low レベル等）でも割り込みを出さない（従来どおり）。
//!
//! 出力は 2 つ:
//!   - エッジ（と EINT0〜3 のエッジ）で立てる SRCPND のビット（INTC の raise）
//!   - レベルとして保持する SRCPND のビット（INTC の set_level_sources）:
//!     EINT0〜3 のうちレベル設定で有効なもの、EINTPEND の 4〜7・8〜23 のうち
//!     EINTMASK で許可されたものがあれば INT_EINT4_7・INT_EINT8_23。INTC は
//!     このビットが立っている限り SRCPND を立て直す（クリアしても残る）。

use super::stub::Stub;

pub const INT_EINT4_7: u32 = 4;
pub const INT_EINT8_23: u32 = 5;

const GPFCON: u32 = 0x50;
const GPGCON: u32 = 0x60;
const EXTINT0: u32 = 0x88;
pub const EINTMASK: u32 = 0xA4;
pub const EINTPEND: u32 = 0xA8;

/// EINTMASK のリセット値（データシート 9-26）。
pub const EINTMASK_RESET: u32 = 0x00FF_FFF0;

/// ピン n が外部割り込みの機能に設定されているか（GPFn・GPGn の CON が 10）。
fn is_eint(gpio: &Stub, n: u32) -> bool {
    let (con, pin) = if n < 8 { (GPFCON, n) } else { (GPGCON, n - 8) };
    (gpio.read(con, 4) >> (2 * pin)) & 3 == 0b10
}

/// EINTn の信号の方式（EXTINTn の 3 ビット。000 Low・001 High・01x 立ち下がり・
/// 10x 立ち上がり・11x 両エッジ。データシート 9-22）。
fn mode(gpio: &Stub, n: u32) -> u32 {
    let reg = EXTINT0 + 4 * (n / 8);
    (gpio.read(reg, 4) >> (4 * (n % 8))) & 7
}

/// ピンのレベルが prev → now に変わった（または変わらない）ときの割り込みを求める。
/// driven はボードが駆動するピンの集合。戻り値は (エッジで立てる SRCPND のビット,
/// レベルとして保持する SRCPND のビット)。EINTPEND は gpio の中で更新する。
pub fn update(gpio: &mut Stub, driven: u32, prev: u32, now: u32) -> (u32, u32) {
    let mut edge = 0u32;
    let mut level = 0u32;
    let old = gpio.read(EINTPEND, 4);
    let mut pend = old & 0x00FF_FFF0;
    for n in 0..24 {
        let bit = 1u32 << n;
        if driven & bit == 0 || !is_eint(gpio, n) {
            continue;
        }
        let (p, c) = (prev & bit != 0, now & bit != 0);
        let (hit, is_level) = match mode(gpio, n) {
            0 => (!c, true),
            1 => (c, true),
            2 | 3 => (p && !c, false),
            4 | 5 => (!p && c, false),
            _ => (p != c, false),
        };
        if !hit {
            continue;
        }
        if n < 4 {
            if is_level {
                level |= bit;
            } else {
                edge |= bit;
            }
        } else {
            pend |= bit;
        }
    }
    // 変わったときだけ書く（値保持スタブに項目を増やさない。保存の結果を
    // 読み込みの前後で同じにするため）
    if pend != old {
        gpio.write(EINTPEND, 4, pend);
    }
    let active = pend & !gpio.read(EINTMASK, 4);
    if active & 0xF0 != 0 {
        level |= 1 << INT_EINT4_7;
    }
    if active & 0x00FF_FF00 != 0 {
        level |= 1 << INT_EINT8_23;
    }
    (edge, level)
}
