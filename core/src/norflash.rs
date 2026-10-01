//! AMD 方式のコマンドで書き換える NOR フラッシュ（16 ビット幅）。SoC に依らない部品で、
//! 配置はボードが決める（smdk2410 では WM6 のイメージのバンク0）。
//!
//! 中身はバスのアリーナに置き（`Bus::map_flash`）、バスが直接読む。この装置は
//! 書き込み（コマンド）を解釈し、ID 読み出しのモードの間の先頭の数語の読みに答え
//! （id_read）、書き込み・消去を after_write で中身に反映する。
//!
//! 根拠: WM6 の Device Emulator 用イメージの IPL（ブートローダ）が、先頭の 64KB に
//! 対して AMD の自動選択（AA→555h、55→2AAh、90→555h。語アドレス）を送り、製造元
//! 0x0001・デバイス 0x225B（Am29LV800BB、16 ビット幅）を確かめてから、書き込み
//! （AA/55/A0 → アドレス・データ）とトグルビット（DQ6）での完了待ちを使う
//! （2026-09-30 に IPL をトレースして確認）。コマンドの表は Am29LV800B の
//! データシートの Command Definitions に従う。
//!
//! 時間: 書き込み・消去は瞬時に終える（次の読みは新しい中身。DQ6 のトグル・DQ7 の
//! データポーリングはどちらも「完了」に見える）。
//! TODO: 実機の書き込み・消去の時間（ゲストが時間切れを見る場合）。
//! TODO: CFI（98h）・アンロックバイパス（20h）・消去の中断（B0h/30h）は未実装
//! （ゲストが使うのを見ていない）。未知の列は読み出し配列に戻す（データシートの
//! 「不正な列はリセット」）。

/// 製造元コード（AMD）。
const MFG_ID: u16 = 0x0001;
/// デバイスコード（Am29LV800BB、ワードモード）。
const DEV_ID: u16 = 0x225B;

/// 保留中の書き換え（after_write で中身に反映する）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// off のハーフワードに v を書く（1→0 だけ変わる）
    Program { off: u32, v: u16 },
    /// off から len バイトを 0xFF にする
    Erase { off: u32, len: u32 },
}

/// コマンドの受け付けの状態。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// 読み出し配列（コマンドの 1 サイクル目待ち）
    Read,
    /// AA を受けた
    Unlock1,
    /// AA・55 を受けた
    Unlock2,
    /// 自動選択（ID 読み出し）
    Autoselect,
    /// 自動選択の中で AA を受けた
    AsUnlock1,
    /// 自動選択の中で AA・55 を受けた
    AsUnlock2,
    /// A0 を受けた（次の書き込みがデータ）
    Program,
    /// 80 を受けた
    Erase0,
    /// 80・AA を受けた
    Erase1,
    /// 80・AA・55 を受けた
    Erase2,
}

/// NOR フラッシュのコマンドの状態。
#[derive(Clone, Debug)]
pub struct NorFlash {
    /// 中身の大きさ（バイト。構成で決まる）
    size: u32,
    state: State,
    /// after_write で反映する書き換え（命令境界の間だけ持つ。保存しない）
    pub(crate) pending: Option<Op>,
}

impl NorFlash {
    pub fn new(size: u32) -> NorFlash {
        NorFlash {
            size,
            state: State::Read,
            pending: None,
        }
    }

    pub fn size(&self) -> u32 {
        self.size
    }

    /// 読み出し配列のモードか（バスが直接読んでよいか）。
    pub fn array(&self) -> bool {
        !matches!(
            self.state,
            State::Autoselect | State::AsUnlock1 | State::AsUnlock2
        )
    }

    /// 読み出し配列でないモードの読み（バスはフラッシュの先頭ページの読みだけを問う）。
    /// 自動選択では語アドレス 0 が製造元・1 がデバイス・2 がセクタの保護（保護なし = 0）で、
    /// それ以外は None（中身を読む）。
    ///
    /// 実チップは語アドレスの下位ビットだけを見てどの番地でも ID を返すが、WM6 の
    /// フラッシュドライバ（XIP、フラッシュ上で実行）は自動選択の間も同じフラッシュから
    /// 命令とリテラルを読んで動き続ける（2026-09-30 のトレース。ID の比較の直後の
    /// `ldr r3,[pc,#0x50]` がリテラルを読み、その値のアドレスに書く）。Device Emulator の
    /// フラッシュは ID を先頭の数語でだけ返すと判断した。
    /// TODO: ID を返す範囲（先頭の 3 語だけか、セクタごとか）は推定。ゲストが先頭以外の
    /// セクタの保護を読むのを見たら見直す。
    pub fn id_read(&self, off: u32, size: u32) -> Option<u32> {
        if self.array() || off >= 6 {
            return None;
        }
        let half = |o: u32| -> u32 {
            match o >> 1 {
                0 => MFG_ID as u32,
                1 => DEV_ID as u32,
                _ => 0,
            }
        };
        Some(match size {
            4 => half(off) | half(off + 2) << 16,
            2 => half(off),
            _ => (half(off & !1) >> ((off & 1) * 8)) & 0xFF,
        })
    }

    /// 書き込み（コマンドのサイクル）。32 ビットの書き込みは下位・上位の 2 サイクル。
    pub fn write(&mut self, off: u32, size: u32, v: u32) {
        match size {
            4 => {
                self.cycle(off, v as u16);
                self.cycle(off + 2, (v >> 16) as u16);
            }
            2 => self.cycle(off, v as u16),
            // TODO: 8 ビットの書き込み（16 ビット幅のバスでの扱いが不明）。下位バイトを
            // コマンドとして受ける
            _ => self.cycle(off & !1, v as u16 & 0xFF),
        }
    }

    /// 1 サイクル（16 ビット）の書き込み。コマンドのアドレスは語アドレスの A10〜A0 だけを
    /// 見る（上位は無関係。IPL は 5555h/2AAAh を使う）。データは下位 8 ビット。
    fn cycle(&mut self, off: u32, v: u16) {
        let a = (off >> 1) & 0x7FF;
        let d = v & 0xFF;
        if self.state == State::Program {
            // データのサイクルはアドレス・値とも任意（F0 でもデータとして書く）
            if off < self.size && self.pending.is_none() {
                self.pending = Some(Op::Program { off: off & !1, v });
            }
            self.state = State::Read;
            return;
        }
        if d == 0xF0 {
            self.state = State::Read; // リセット（どのサイクルでも受け付ける）
            return;
        }
        self.state = match (self.state, a, d) {
            (State::Read, 0x555, 0xAA) => State::Unlock1,
            (State::Unlock1, 0x2AA, 0x55) => State::Unlock2,
            (State::Unlock2, 0x555, 0x90) => State::Autoselect,
            (State::Unlock2, 0x555, 0xA0) => State::Program,
            (State::Unlock2, 0x555, 0x80) => State::Erase0,
            (State::Erase0, 0x555, 0xAA) => State::Erase1,
            (State::Erase1, 0x2AA, 0x55) => State::Erase2,
            (State::Erase2, 0x555, 0x10) => {
                self.pending = Some(Op::Erase {
                    off: 0,
                    len: self.size,
                });
                State::Read
            }
            (State::Erase2, _, 0x30) => {
                let (lo, len) = sector(off, self.size);
                self.pending = Some(Op::Erase { off: lo, len });
                State::Read
            }
            // 自動選択の中でも同じアンロックの列を受ける（F0 以外で抜けるのは下）
            (State::Autoselect, 0x555, 0xAA) => State::AsUnlock1,
            (State::AsUnlock1, 0x2AA, 0x55) => State::AsUnlock2,
            (State::AsUnlock2, 0x555, 0x90) => State::Autoselect,
            (State::Autoselect | State::AsUnlock1 | State::AsUnlock2, _, _) => State::Autoselect,
            // 不正な列は読み出し配列に戻る
            _ => State::Read,
        };
    }

    /// 保留中の書き換えを中身 data（フラッシュ全体）に反映し、書き換えた範囲
    /// （先頭, 大きさ）を返す。
    pub fn apply(&mut self, data: &mut [u8]) -> Option<(u32, u32)> {
        match self.pending.take()? {
            Op::Program { off, v } => {
                let a = off as usize;
                let h = data.get_mut(a..a + 2)?;
                let cur = u16::from_le_bytes([h[0], h[1]]);
                // 書き込みは 1→0 だけ（0→1 は消去が要る）
                h.copy_from_slice(&(cur & v).to_le_bytes());
                Some((off, 2))
            }
            Op::Erase { off, len } => {
                let (a, n) = (off as usize, len as usize);
                data.get_mut(a..a + n)?.fill(0xFF);
                Some((off, len))
            }
        }
    }

    /// 状態を 1 バイトにする（スナップショット）。
    pub fn state_byte(&self) -> u8 {
        self.state as u8
    }

    /// state_byte の逆。
    pub fn set_state_byte(&mut self, b: u8) -> Result<(), String> {
        const ALL: [State; 10] = [
            State::Read,
            State::Unlock1,
            State::Unlock2,
            State::Autoselect,
            State::AsUnlock1,
            State::AsUnlock2,
            State::Program,
            State::Erase0,
            State::Erase1,
            State::Erase2,
        ];
        self.state = *ALL
            .get(b as usize)
            .ok_or_else(|| format!("bad flash state {b}"))?;
        self.pending = None;
        Ok(())
    }
}

/// off を含むセクタ（先頭, 大きさ）。先頭 64KB は Am29LV800BB のブートセクタ
/// （16KB・8KB・8KB・32KB。データシートの Sector Address Table）、以後は 64KB ずつ。
/// TODO: Am29LV800B は 1MB のチップで、Device Emulator の 96MB のフラッシュは実在の
/// 構成ではない。1MB より先も 64KB ずつとしているのは推定（ゲストの消去を観察して確かめる）。
fn sector(off: u32, size: u32) -> (u32, u32) {
    let (lo, len) = match off {
        0..0x4000 => (0, 0x4000),
        0x4000..0x6000 => (0x4000, 0x2000),
        0x6000..0x8000 => (0x6000, 0x2000),
        0x8000..0x10000 => (0x8000, 0x8000),
        _ => (off & !0xFFFF, 0x10000),
    };
    (lo, len.min(size.saturating_sub(lo)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unlock(f: &mut NorFlash, cmd: u32) {
        f.write(0xAAAA, 2, 0xAAAA);
        f.write(0x5554, 2, 0x5555);
        f.write(0xAAAA, 2, cmd);
    }

    #[test]
    fn autoselect_and_reset() {
        let mut f = NorFlash::new(0x20000);
        assert!(f.array());
        unlock(&mut f, 0x9090);
        assert!(!f.array());
        assert_eq!(f.id_read(0, 2), Some(0x0001));
        assert_eq!(f.id_read(2, 2), Some(0x225B));
        assert_eq!(f.id_read(0, 4), Some(0x225B_0001));
        assert_eq!(f.id_read(4, 2), Some(0));
        assert_eq!(f.id_read(0x14, 4), None);
        f.write(0, 2, 0xF0F0);
        assert!(f.array());
        assert_eq!(f.id_read(0, 2), None);
    }

    #[test]
    fn program_only_clears_bits() {
        let mut f = NorFlash::new(0x20000);
        let mut data = vec![0xFFu8; 0x20000];
        unlock(&mut f, 0xA0A0);
        f.write(0x100, 2, 0x1234);
        f.apply(&mut data);
        assert_eq!(&data[0x100..0x102], &[0x34, 0x12]);
        unlock(&mut f, 0xA0A0);
        f.write(0x100, 2, 0xFF0F);
        f.apply(&mut data);
        assert_eq!(&data[0x100..0x102], &[0x04, 0x12]);
        assert!(f.array());
    }

    #[test]
    fn sector_erase() {
        let mut f = NorFlash::new(0x30000);
        let mut data = vec![0u8; 0x30000];
        unlock(&mut f, 0x8080);
        f.write(0xAAAA, 2, 0xAAAA);
        f.write(0x5554, 2, 0x5555);
        f.write(0x12344, 2, 0x3030);
        f.apply(&mut data);
        assert!(data[0x10000..0x20000].iter().all(|&b| b == 0xFF));
        assert_eq!(data[0xFFFF], 0);
        assert_eq!(data[0x20000], 0);
        // ブートセクタ
        unlock(&mut f, 0x8080);
        f.write(0xAAAA, 2, 0xAAAA);
        f.write(0x5554, 2, 0x5555);
        f.write(0x4002, 2, 0x3030);
        f.apply(&mut data);
        assert!(data[0x4000..0x6000].iter().all(|&b| b == 0xFF));
        assert_eq!(data[0x3FFF], 0);
        assert_eq!(data[0x6000], 0);
    }

    #[test]
    fn bad_sequence_returns_to_array() {
        let mut f = NorFlash::new(0x20000);
        f.write(0xAAAA, 2, 0xAAAA);
        f.write(0x1234, 2, 0x5555);
        f.write(0xAAAA, 2, 0x9090);
        assert!(f.array());
        assert_eq!(f.pending, None);
    }
}
