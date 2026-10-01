//! CPU とメモリの境界の型。
//!
//! メモリアクセスのエラーの使い分け:
//!   - [`MemError::Abort`] … MMU 起因のフォルト。CPU がデータアボート／
//!     プリフェッチアボート例外としてゲストに配送する。
//!   - [`MemError::Bus`] … 未マップの物理アドレス（ページテーブル自体が読めない
//!     場合を含む）。エミュレータ側の不備（スタブ未実装など）とみなし、
//!     エミュレーションを停止させる。

use std::fmt;

use crate::bus::BusError;

/// MMU の変換・保護チェックで発生したフォルト。`status`/`domain` は FSR
/// （フォルトステータスレジスタ）にそのまま入る値（ARM ARM DDI 0100 の
/// フォルトステータス符号）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Abort {
    /// フォルトを起こした仮想アドレス
    pub va: u32,
    /// FSR[3:0]（例: 0101 = セクション変換フォルト）
    pub status: u8,
    /// FSR[7:4] に入るドメイン番号
    pub domain: u8,
    /// 書き込みアクセスだったか
    pub write: bool,
}

impl fmt::Display for Abort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = if self.write { "write" } else { "read" };
        write!(
            f,
            "abort: {kind} at VA={:08X} (status={:X} domain={:X})",
            self.va, self.status, self.domain
        )
    }
}

/// メモリアクセスのエラー。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemError {
    Abort(Abort),
    Bus(BusError),
}

impl From<BusError> for MemError {
    fn from(e: BusError) -> Self {
        MemError::Bus(e)
    }
}

impl fmt::Display for MemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemError::Abort(a) => a.fmt(f),
            MemError::Bus(b) => b.fmt(f),
        }
    }
}

impl std::error::Error for MemError {}
