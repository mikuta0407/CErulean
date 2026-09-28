//! DMA コントローラの最小スタブ（データシート Ch.8）。

use super::Stub;

/// DMA コントローラ（4 チャネル×0x40 間隔）の最小スタブ。実転送は行わず、
/// 「起動された転送は即座に完了する」ように見せる。オーディオ（IIS）ドライバの
/// ブートを通すのが目的。
///
/// 各チャネルのレジスタ: DISRC 0x00 / DISRCC 0x04 / DIDST 0x08 / DIDSTC 0x0C /
/// DCON 0x10 / DSTAT 0x14 / DCSRC 0x18 / DCDST 0x1C / DMASKTRIG 0x20。
///
/// 挙動（2026-09 の実イメージ観察に基づく）:
///   - DSTAT の CURR_TC[19:0] は常に DCON の TC を返す。ドライバが起動直後に
///     「CURR_TC != 0（転送中）」をポーリングするため、0 だとハングする。
///   - DMASKTRIG の ON_OFF(bit1) が書かれたら、そのチャネルの完了割り込みを
///     即座に上げる（write の戻り値）。カーネルが DMA ISR の立てるフラグを
///     スピンで待つため。ON_OFF が bit1 なのは User's Manual Rev 1.1 で確認済み。
///
/// TODO: 実 DMA（メモリ↔IIS 等の転送）はオーディオ対応時に実装する。
/// 「CURR_TC == 0（完了）」のポーリングが現れたらこのモデルでは破綻する。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DmaStub {
    pub(crate) stub: Stub,
}

impl DmaStub {
    pub fn new() -> DmaStub {
        DmaStub {
            stub: Stub::new(&[]),
        }
    }

    pub fn read(&self, off: u32, size: u32) -> u32 {
        // チャネル内オフセット 0x14 = DSTAT。対応する DCON の TC を返す。
        if off < 4 * 0x40 && off & 0x3F == 0x14 {
            return self.stub.read(off & !0x3F | 0x10, 4) & 0xFFFFF;
        }
        self.stub.read(off, size)
    }

    /// 戻り値は完了割り込みを上げるチャネル（INT_DMAn）。
    pub fn write(&mut self, off: u32, size: u32, v: u32) -> Option<usize> {
        self.stub.write(off, size, v);
        // DMASKTRIG(+0x20) の ON_OFF: 転送即完了として割り込みを上げる。
        (off < 4 * 0x40 && off & 0x3F == 0x20 && v & 2 != 0).then_some((off >> 6) as usize)
    }
}
