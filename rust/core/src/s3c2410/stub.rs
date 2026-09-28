//! 値保持スタブ（GPIO・クロック等）。

use std::collections::BTreeMap;

/// 「書かれた値を保持して読み返すだけ」の汎用 MMIO ブロック。
/// GPIO・クロック等、当面は読み書きが通ればブートが進む周辺機器に使う。
/// 実動作（ステータスビットの変化、割り込み等）が必要になった機器は
/// 専用実装に置き換える。
///
/// レジスタ数は少ない（ブロックあたり数十個）ので疎に保持する。反復順が
/// 決まるよう BTreeMap にする（保存結果を実行ごとに同じにするため）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stub {
    /// ワードアラインしたオフセット → 値
    pub(crate) regs: BTreeMap<u32, u32>,
    /// 読み出し時に常に OR されるビット（オフセット → マスク）。
    /// 「ハードウェアが立てる ready 系フラグ」を書き込み値と独立に見せる
    /// ために使う（例: IISCON の TX FIFO ready）。構成で決まる（保存しない）。
    pub(crate) forced: BTreeMap<u32, u32>,
}

impl Stub {
    /// init はリセット値（ワードアラインオフセット → 値）。
    pub fn new(init: &[(u32, u32)]) -> Stub {
        Stub {
            regs: init.iter().map(|&(k, v)| (k & !3, v)).collect(),
            forced: BTreeMap::new(),
        }
    }

    /// off の読み出しで常に mask を立てる（ready 系フラグ用）。
    pub fn force_read_bits(mut self, off: u32, mask: u32) -> Stub {
        *self.forced.entry(off & !3).or_default() |= mask;
        self
    }

    /// ワード単位で保持した値から、アクセスサイズ分を切り出して返す。
    /// 未書き込みのレジスタは 0。
    pub fn read(&self, off: u32, size: u32) -> u32 {
        let a = off & !3;
        let w = self.regs.get(&a).copied().unwrap_or(0) | self.forced.get(&a).copied().unwrap_or(0);
        match size {
            1 => (w >> ((off & 3) * 8)) & 0xFF,
            2 => (w >> ((off & 2) * 8)) & 0xFFFF,
            _ => w,
        }
    }

    /// 保持ワードの該当バイト/ハーフワードだけを書き換える。
    pub fn write(&mut self, off: u32, size: u32, v: u32) {
        let a = off & !3;
        let w = self.regs.entry(a).or_default();
        *w = match size {
            1 => {
                let shift = (off & 3) * 8;
                *w & !(0xFF << shift) | (v & 0xFF) << shift
            }
            2 => {
                let shift = (off & 2) * 8;
                *w & !(0xFFFF << shift) | (v & 0xFFFF) << shift
            }
            _ => v,
        };
    }
}
