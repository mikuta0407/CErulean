//! UART 1 チャネル分（データシート Ch.11。送信のみ）。

/// レジスタオフセット（リトルエンディアン時）:
///
/// ```text
/// 0x00 ULCON   0x04 UCON    0x08 UFCON   0x0C UMCON
/// 0x10 UTRSTAT 0x14 UERSTAT 0x18 UFSTAT  0x1C UMSTAT
/// 0x20 UTXH    0x24 URXH    0x28 UBRDIV
/// ```
///
/// 送信したバイトは `tx` にためる（コアは出力先を持たない。呼び出し側が
/// [`take_tx`](Uart::take_tx) で取り出して表示する）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Uart {
    // 設定レジスタは書かれた値を保持するだけ（ボーレート等は動作に影響しない）。
    pub(crate) ulcon: u32,
    pub(crate) ucon: u32,
    pub(crate) ufcon: u32,
    pub(crate) umcon: u32,
    pub(crate) ubrdiv: u32,
    /// 送信バイトをためるか（出力先のないチャネルは捨てる）
    capture: bool,
    /// 送信済みでまだ取り出されていないバイト（派生情報: 保存しない）
    tx: Vec<u8>,
}

const REG_ULCON: u32 = 0x00;
const REG_UCON: u32 = 0x04;
const REG_UFCON: u32 = 0x08;
const REG_UMCON: u32 = 0x0C;
const REG_UTRSTAT: u32 = 0x10;
const REG_UERSTAT: u32 = 0x14;
const REG_UFSTAT: u32 = 0x18;
const REG_UMSTAT: u32 = 0x1C;
const REG_UTXH: u32 = 0x20;
const REG_URXH: u32 = 0x24;
const REG_UBRDIV: u32 = 0x28;

impl Uart {
    /// capture=true なら送信バイトをためる（Go の NewUART(w) の w が nil でない場合）。
    pub fn new(capture: bool) -> Uart {
        Uart {
            capture,
            ..Default::default()
        }
    }

    /// 送信済みのバイトを取り出す（中身は空になる）。
    pub fn take_tx(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.tx)
    }

    pub fn read(&self, off: u32, _size: u32) -> u32 {
        match off & !3 {
            REG_ULCON => self.ulcon,
            REG_UCON => self.ucon,
            REG_UFCON => self.ufcon,
            REG_UMCON => self.umcon,
            // bit2: transmitter empty, bit1: TX buffer empty。
            // 送信は即時に完了するので常に空。bit0（RX ready）は 0 = 受信なし。
            REG_UTRSTAT => 0x6,
            REG_UFSTAT => 0,                          // FIFO 空
            REG_UERSTAT | REG_UMSTAT | REG_URXH => 0, // 受信は未実装
            REG_UBRDIV => self.ubrdiv,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, _size: u32, v: u32) {
        match off & !3 {
            REG_ULCON => self.ulcon = v,
            REG_UCON => self.ucon = v,
            REG_UFCON => self.ufcon = v,
            REG_UMCON => self.umcon = v,
            REG_UBRDIV => self.ubrdiv = v,
            // 送信ホールディングレジスタ: 下位 8 ビットを即時出力する。
            REG_UTXH if self.capture => self.tx.push(v as u8),
            _ => {}
        }
    }
}
