//! SPI1 につながるキーボード用マイコン（SMDK2410 ボード上の部品で、S3C2410 の
//! データシートの範囲外）。Go の kbd.go。

use std::collections::VecDeque;

/// kbdmouse.dll が SPI1 と EINT1 で使うキーボード用マイコン。
///
/// 観察した事実（2026-09、実イメージの kbdmouse.dll をトレース）:
///   - 初期化: GPG5〜7 を SPI1、GPB6 を出力（チップセレクト）、GPF1 を EINT1
///     （立ち下がりエッジ）にし、SPCON1=0x3A・SPPRE1=0xFF。0xFF を 10 回送って
///     受信を読み捨て、その後 3 バイトのコマンド（1B A0 7B、1B A1 7A 等。
///     3 バイトの XOR が 0xC0）を送る。コマンドの意味は未解明（応答も読まない）。
///   - EINT1 の IST は 1 回の割り込みで 1 バイトだけ読む: GPB6=Low → SPTDAT1 に
///     0xFF → REDY 待ち → GPB6=High → SPRDAT1 を読む。
///   - バイトの bit7=1 はキーを離した、bit6〜0 はスキャンコード。直前と同じ
///     バイトは無視する（二重通知の除去）。
///   - スキャンコード → VK はドライバ内の固定表（コード 0x00〜0x6F）。
///
/// モデル: キー操作をバイト列としてキューに積み、1 バイト渡すごとに（まだ
/// 残っていれば）EINT1 を上げ直す。まとめて上げると INTC の SRCPND で 1 回に
/// 潰れて取りこぼすため、1 割り込み 1 バイトを守る。EINT1 を上げるかは
/// 戻り値で返す（ボードが INTC に渡す）。
///
/// TODO: 未確認事項:
///   - 送るデータがないときに実機のマイコンが返す値（ここでは 0）
///   - チップセレクト（GPB6）を見ていない（GPIO が値保持スタブのため）
///   - 初期化時の 3 バイトコマンドの意味（LED・リピート設定等？）
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KbdMcu {
    /// ドライバに返す（ドライバが受信する）バイト列（保存する）
    pub(crate) out: VecDeque<u8>,
    /// ドライバが送ったバイト（調査用。状態ではない）
    pub(crate) log: Vec<u8>,
}

impl KbdMcu {
    /// SPI の 1 バイト交換。戻り値は (返すバイト, EINT1 を上げ直すか)。
    pub fn transfer(&mut self, tx: u8) -> (u8, bool) {
        self.log.push(tx);
        match self.out.pop_front() {
            // 次のバイトの割り込み（IST の InterruptDone 後に受理される）
            Some(b) => (b, !self.out.is_empty()),
            None => (0, false),
        }
    }

    /// バイトを積む。戻り値は EINT1 を上げるか（キューが空だったとき）。
    pub fn push(&mut self, b: u8) -> bool {
        self.out.push_back(b);
        self.out.len() == 1
    }
}

/// kbdmouse.dll のスキャンコード → VK の表（2026-09 に実行中のメモリから
/// 読み出した）のうち、キー名を付けて公開するもの（名前の順）。
///
/// ソフトキー（WM5 の VK_TSOFT1/2 = VK_F1/VK_F2）は表に無く、この経路では
/// 押せない。画面下端のソフトキー表示をタップすれば同じ操作になる。
/// TODO: Device Emulator の本体ボタン（ソフトキー・電源等）が別経路か確認する。
/// キー名の綴りは基準シナリオのスクリプトをそのまま使うため Go と同じにする。
pub const KEY_SCAN_CODES: &[(&str, u8)] = &[
    ("0", 0x49),
    ("1", 0x31),
    ("2", 0x39),
    ("3", 0x2F),
    ("4", 0x37),
    ("5", 0x3F),
    ("6", 0x47),
    ("7", 0x4F),
    ("8", 0x57),
    ("9", 0x41),
    ("A", 0x0D),
    ("Alt", 0x01),
    ("App1", 0x64), // アプリケーションボタン（VK_APP1〜5）
    ("App2", 0x65),
    ("App3", 0x66),
    ("App4", 0x67),
    ("App5", 0x68),
    ("B", 0x3E),
    ("Back", 0x69),
    ("C", 0x2E),
    ("CapsLock", 0x2C),
    ("Ctrl", 0x19),
    ("D", 0x35),
    ("Delete", 0x2A),
    ("Down", 0x6A),
    ("E", 0x3B),
    ("Enter", 0x5A),
    ("Esc", 0x29),
    ("F", 0x3D),
    ("G", 0x45),
    ("H", 0x4D),
    ("I", 0x52),
    ("J", 0x55),
    ("K", 0x44),
    ("L", 0x4C),
    ("Left", 0x6D),
    ("M", 0x4E),
    ("N", 0x46),
    ("O", 0x4B),
    ("P", 0x53),
    ("Q", 0x2B),
    ("R", 0x43),
    ("RShift", 0x62),
    ("Right", 0x6F),
    ("S", 0x2D),
    ("Shift", 0x12),
    ("Space", 0x6E),
    ("T", 0x3A),
    ("Tab", 0x0B),
    ("U", 0x4A),
    ("Up", 0x6C),
    ("V", 0x36),
    ("W", 0x33),
    ("Win", 0x60),
    ("X", 0x0E),
    ("Y", 0x42),
    ("Z", 0x0C),
];

/// キー名のスキャンコード。
pub fn scan_code(name: &str) -> Option<u8> {
    KEY_SCAN_CODES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|&(_, c)| c)
}
