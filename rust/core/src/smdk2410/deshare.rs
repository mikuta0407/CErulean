//! Device Emulator のフォルダ共有（emulserv.dll・vcefsd.dll が使う準仮想デバイス）。
//! WM5 からは「Storage Card」に見える。PC カードのソケットを使わないので、イーサネット
//! カードと同時に使える（2026-09-30 ユーザー了承: ROM は変えずに装置を用意する）。
//!
//! 一次資料はない（Device Emulator 固有の装置で、仕様は公開されていない。Device Emulator の
//! ソースは参照しない）。以下はゲストのドライバ（emulserv.dll・vcefsd.dll の機械語。
//! 2026-09-30 の逆アセンブル）から読み取った約束で、ホスト側（この装置）は、ゲストの
//! コードが読む値だけを、そのコードの使い方と矛盾しないように返す:
//!
//! レジスタ（PA 0x500F4000〜・0x500F5000〜。32 ビット）:
//! ```text
//! 500F4000 W  共有バッファの物理アドレス（vcefsd は 0x33EFF000 を書く）
//! 500F4004 W  コマンド。書くとホストが処理する（0 は emulserv の確認応答）
//! 500F4008 R  0 以外なら処理中（vcefsd は 0 になるまで 4004 に 0 を書き続ける）。常に 0
//! 500F400C R  結果（コマンドの後は状態。0 = 成功。挿抜の通知の後は 0 = 挿した・1 = 抜いた）
//! 500F5000 W  emulserv が 0xFFFFFFFF を書く（初期化。意味は不明。保持するだけ）
//! 500F5004 R  bit30 = 挿抜の通知がある（emulserv の IST が見る）
//! ```
//! 挿抜の通知は EINT11 の割り込み。emulserv は EXTINT1 の EINT11 を High レベルにし、IRQ 39
//! （EINT11）で待つが、GPG3 を EINT11 の機能にしない。Device Emulator が EINTPEND に直接
//! 立てていたと判断し、ここでも EINTPEND の bit11 を直接立てる。
//!
//! 共有バッファ（0x42C バイト。u16 +0 は大きさ、u16 +2 は結果）:
//! ```text
//! +04 u32  検索の枠の番号（FindFirst が入れる。名前の検索では 0xFFFFFFFF）
//! +08 i16  -1 = 名前（+24）で引く、0 以上 = ディレクトリの何番目の項目を返すか
//! +0A u16  ハンドル（0xFFFF = なし）
//! +0C u32  更新日時（DOS 形式: 上位 16 ビットが日付、下位が時刻）
//! +10 u32  大きさ・読み書きのバイト数・（空き容量では）全クラスタ数
//! +14 u32  読み書きの位置・新しい大きさ・（空き容量では）空きクラスタ数
//! +18 u32  データの物理アドレス（vcefsd は 0x33EEF000、64KB）
//! +1C u16  属性（FILE_ATTRIBUTE_*）
//! +1E u16  開く方法（bit0 = 書き、bit1 = 読み書き、bit4〜6 = 共有）
//! +20 u8   パスにワイルドカードがある（ゲストが使う）
//! +22 u16  パス・名前の長さ（バイト）
//! +24      パス（UTF-16。ボリュームの根からの「\a\b」）。列挙の応答では項目の名前だけ
//! +224 u16 新しいパスの長さ（移動）
//! +226     新しいパス（移動）
//! +428 u32 作成日時（DOS 形式）
//! ```
//! コマンド: 4 初期化・5 ファイル作成・6 開く・7 読む・8 書く・9 大きさの変更・0A 閉じる・
//! 0B 空き容量・0C ディレクトリ作成・0D ディレクトリ削除・0E 属性と日時・0F 移動・10 削除・
//! 11 名前で引く／列挙・13 フラッシュ・15 1 回に送れる最大のバイト数（結果レジスタで返す）。
//!
//! ホストの決めごと（ゲストのコードから外れない範囲で決めたもの）:
//! - 列挙の応答は +14 に「どのディレクトリか」の印を入れる。FindFirst は応答を枠に写して
//!   FindNext で書き戻すので、次の列挙もその印で同じディレクトリを引ける（ゲストは名前の
//!   検索の応答の +14 を読まない）。
//! - 閉じた（0A）後の 0E はハンドルの番号で来る（vcefsd の CloseFile）ので、閉じたハンドルも
//!   番号が再利用されるまでパスを覚えておく。
//! - 新しく作ったファイル・ディレクトリの日時はゲストの RTC の時刻（決定論的）。
//!
//! 中身は装置の中（メモリ）にあり、スナップショットに入る。挿すときにフロントエンドが
//! 渡し、抜くと返す（ストレージカードと同じ「抜く → 編集 → 挿す」）。

use std::collections::BTreeMap;

use crate::bus::RamAccess;
use crate::snapshot::{Decoder, Encoder, Error};

/// 共有バッファ・データバッファを置いてよい範囲（vcefsd の MountDisk が使う 0x33EEF000〜
/// 0x33EFF42C。ゲストが他の番地を渡したら断る: RAM を直接書くのでコードのページを壊さない
/// ように）。
const BUF_LO: u32 = 0x33EE_F000;
const BUF_HI: u32 = 0x33F0_0000;
const CMD_BUF_LEN: u32 = 0x42C;
/// 1 回に送れる最大のバイト数（データバッファ 64KB）。
const MAX_XFER: u32 = 0x1_0000;
/// 中身の上限（ストレージカードと同じ 512MB）。空き容量もこれから計算する。
pub const MAX_BYTES: u64 = 512 << 20;
/// 空き容量のクラスタの大きさ（vcefsd は 64 セクタ × 512 バイトと決め打ちする）。
const CLUSTER: u64 = 64 * 512;

// 結果（0 以外は失敗。vcefsd は 0 かどうかだけを見る。値は Win32 のエラー番号に合わせた）
const OK: u32 = 0;
const E_NOT_FOUND: u32 = 2;
const E_PATH: u32 = 3;
const E_ACCESS: u32 = 5;
const E_HANDLE: u32 = 6;
const E_NO_MORE: u32 = 0x12;
const E_PARAM: u32 = 0x57;
const E_EXISTS: u32 = 0xB7;
const E_FULL: u32 = 0x70;

const ATTR_DIR: u16 = 0x10;
const ATTR_ARCHIVE: u16 = 0x20;

/// 共有フォルダの 1 項目。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareEntry {
    /// 名前（大文字・小文字を保つ）
    pub name: String,
    pub dir: bool,
    pub attrs: u16,
    /// 更新日時・作成日時（DOS 形式）
    pub mtime: u32,
    pub ctime: u32,
    pub data: Vec<u8>,
}

/// 共有フォルダの中身（パス「\a\b」の小文字の形 → 項目。根は含めない）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShareFs {
    nodes: BTreeMap<String, ShareEntry>,
}

fn key(path: &str) -> String {
    path.trim_end_matches('\\').to_lowercase()
}

fn parent(k: &str) -> &str {
    match k.rfind('\\') {
        Some(i) => &k[..i],
        None => "",
    }
}

impl ShareFs {
    pub fn new() -> ShareFs {
        ShareFs::default()
    }

    fn is_dir(&self, k: &str) -> bool {
        k.is_empty() || self.nodes.get(k).is_some_and(|e| e.dir)
    }

    /// 項目を足す（path は「\a\b」。親のディレクトリは先に足すこと）。
    pub fn insert(&mut self, path: &str, e: ShareEntry) -> Result<(), String> {
        let k = key(path);
        if k.is_empty() || !k.starts_with('\\') {
            return Err(format!("bad path {path:?}"));
        }
        if !self.is_dir(parent(&k)) {
            return Err(format!("parent of {path:?} is not a directory"));
        }
        if self.used_bytes() + e.data.len() as u64 > MAX_BYTES {
            return Err("shared folder is too large".into());
        }
        self.nodes.insert(k, e);
        Ok(())
    }

    /// 全項目（パスの順。親が子より先）。パスは元の大文字・小文字で組み立てる。
    pub fn entries(&self) -> Vec<(String, &ShareEntry)> {
        let mut out = Vec::new();
        for (k, e) in &self.nodes {
            // 元の名前でパスを組み立てる
            let mut path = String::new();
            let mut cur = String::new();
            for part in k.split('\\').skip(1) {
                cur.push('\\');
                cur.push_str(part);
                let n = self.nodes.get(&cur).map_or(part, |x| x.name.as_str());
                path.push('\\');
                path.push_str(n);
            }
            out.push((path, e));
        }
        out
    }

    fn children(&self, dir: &str) -> Vec<&str> {
        let prefix = format!("{dir}\\");
        self.nodes
            .range(prefix.clone()..)
            .take_while(|(k, _)| k.starts_with(&prefix))
            .filter(|(k, _)| !k[prefix.len()..].contains('\\'))
            .map(|(k, _)| k.as_str())
            .collect()
    }

    pub fn used_bytes(&self) -> u64 {
        self.nodes.values().map(|e| e.data.len() as u64).sum()
    }
}

/// フォルダ共有の装置。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeShare {
    /// 挿している共有フォルダ（None = 挿していない）
    pub(crate) fs: Option<ShareFs>,
    buf_pa: u32,
    result: u32,
    /// 挿抜の通知（500F5004 の bit30）
    event: bool,
    reg5000: u32,
    /// ハンドル → (パス, 開いているか)
    handles: BTreeMap<u16, (String, bool)>,
    next_handle: u16,
    /// 列挙の印 → ディレクトリのパス（印は 1 から）
    dirs: Vec<String>,
    /// コマンドが書かれた（after_write で処理する）
    pending: Option<u32>,
}

/// DOS 形式の日時（上位 16 ビットが日付）。
pub fn dos_time(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> u32 {
    let date = (((y - 1980).clamp(0, 127) as u32) << 9) | ((mo as u32) << 5) | d as u32;
    let time = ((h as u32) << 11) | ((mi as u32) << 5) | (s as u32 / 2);
    date << 16 | time
}

impl DeShare {
    pub const STATE_VERSION: u16 = 1;

    pub fn read(&mut self, off: u32) -> u32 {
        match off {
            0x4000 => self.buf_pa,
            0x4008 => 0,
            0x400C => self.result,
            0x5000 => self.reg5000,
            0x5004 => (self.event as u32) << 30,
            _ => 0,
        }
    }

    pub fn write(&mut self, off: u32, v: u32) {
        match off {
            0x4000 => self.buf_pa = v,
            0x4004 => self.pending = Some(v),
            0x5000 => self.reg5000 = v,
            _ => {}
        }
    }

    /// 挿す・抜くの通知を立てる（呼び出し側が EINT11 を立てる）。
    pub fn insert(&mut self, fs: ShareFs) {
        self.fs = Some(fs);
        self.handles.clear();
        self.dirs.clear();
        self.result = 0;
        self.event = true;
    }

    pub fn eject(&mut self) -> Option<ShareFs> {
        let fs = self.fs.take()?;
        self.result = 1;
        self.event = true;
        Some(fs)
    }

    /// コマンドを処理する（書き込みの直後）。now は新しい項目の日時（DOS 形式）。
    pub fn after_write(&mut self, ram: &mut dyn RamAccess, now: u32) {
        let Some(cmd) = self.pending.take() else {
            return;
        };
        if cmd == 0 {
            // emulserv の確認応答: 通知を下ろす
            self.event = false;
            return;
        }
        if cmd == 0x15 {
            self.result = MAX_XFER;
            return;
        }
        let pa = self.buf_pa;
        if pa < BUF_LO || pa + CMD_BUF_LEN > BUF_HI {
            self.result = E_PARAM;
            return;
        }
        let Some(b) = ram.ram_slice_mut(pa, CMD_BUF_LEN) else {
            self.result = E_PARAM;
            return;
        };
        let mut buf = b.to_vec();
        let st = self.command(cmd, &mut buf, now, ram);
        if let Some(b) = ram.ram_slice_mut(pa, CMD_BUF_LEN) {
            b.copy_from_slice(&buf);
        }
        self.result = st;
    }

    fn command(&mut self, cmd: u32, buf: &mut [u8], now: u32, ram: &mut dyn RamAccess) -> u32 {
        let Some(fs) = self.fs.as_mut() else {
            return E_NOT_FOUND;
        };
        let r16 = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let r32 = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let w16 = |b: &mut [u8], o: usize, v: u16| b[o..o + 2].copy_from_slice(&v.to_le_bytes());
        let w32 = |b: &mut [u8], o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        let path_at = |b: &[u8], len_off: usize, off: usize| -> String {
            let n = (r16(b, len_off) as usize).min(0x200);
            let units: Vec<u16> = (0..n / 2).map(|i| r16(b, off + 2 * i)).collect();
            let s = String::from_utf16_lossy(&units);
            let s = s.trim_end_matches('\0').to_string();
            if s.starts_with('\\') {
                s
            } else {
                format!("\\{s}")
            }
        };
        let fill = |b: &mut [u8], e: &ShareEntry| {
            w16(b, 0x1C, if e.dir { e.attrs | ATTR_DIR } else { e.attrs });
            w32(b, 0x0C, e.mtime);
            w32(b, 0x428, e.ctime);
            w32(b, 0x10, if e.dir { 0 } else { e.data.len() as u32 });
        };
        match cmd {
            4 | 0x13 => OK,
            0x11 => {
                let idx = r16(buf, 8) as i16;
                let p = path_at(buf, 0x22, 0x24);
                if idx < 0 {
                    let k = key(&p);
                    if k.is_empty() {
                        // 根そのもの: ディレクトリとして答える（WM6 の vcefsd.dll は一覧の前に
                        // 「\」を名前で引き、失敗すると一覧を空にする。2026-09-30 に観察。
                        // WM5 は引かない）。日時は持たないので 0
                        w16(buf, 0x1C, ATTR_DIR);
                        w32(buf, 0x0C, 0);
                        w32(buf, 0x428, 0);
                        w32(buf, 0x10, 0);
                        let tok = intern(&mut self.dirs, "");
                        w32(buf, 0x14, tok);
                        return OK;
                    }
                    let Some(e) = fs.nodes.get(&k) else {
                        return if k.is_empty() { E_PARAM } else { E_NOT_FOUND };
                    };
                    fill(buf, e);
                    let tok = intern(&mut self.dirs, parent(&k));
                    w32(buf, 0x14, tok);
                    return OK;
                }
                // 列挙: 枠の番号がなければ（名前の検索の続き）パスの親、あれば +14 の印
                let dir = if r32(buf, 4) == 0xFFFF_FFFF {
                    let k = key(&p);
                    let d = parent(&k).to_string();
                    let tok = intern(&mut self.dirs, &d);
                    w32(buf, 0x14, tok);
                    d
                } else {
                    let tok = r32(buf, 0x14);
                    match self.dirs.get((tok as usize).wrapping_sub(1)) {
                        Some(d) => d.clone(),
                        None => return E_PARAM,
                    }
                };
                if !fs.is_dir(&dir) {
                    return E_PATH;
                }
                let kids = fs.children(&dir);
                let Some(k) = kids.get(idx as usize) else {
                    return E_NO_MORE;
                };
                let e = &fs.nodes[*k];
                fill(buf, e);
                let name: Vec<u16> = e.name.encode_utf16().take(0xFF).collect();
                w16(buf, 0x22, (name.len() * 2) as u16);
                for (i, u) in name.iter().enumerate() {
                    w16(buf, 0x24 + 2 * i, *u);
                }
                w16(buf, 0x24 + 2 * name.len(), 0);
                OK
            }
            5 | 0x0C => {
                let p = path_at(buf, 0x22, 0x24);
                let k = key(&p);
                if k.is_empty() || fs.nodes.contains_key(&k) {
                    return E_EXISTS;
                }
                if !fs.is_dir(parent(&k)) {
                    return E_PATH;
                }
                let name = p
                    .trim_end_matches('\\')
                    .rsplit('\\')
                    .next()
                    .unwrap_or("")
                    .to_string();
                let dir = cmd == 0x0C;
                fs.nodes.insert(
                    k,
                    ShareEntry {
                        name,
                        dir,
                        attrs: if dir { ATTR_DIR } else { ATTR_ARCHIVE },
                        mtime: now,
                        ctime: now,
                        data: Vec::new(),
                    },
                );
                OK
            }
            6 => {
                let k = key(&path_at(buf, 0x22, 0x24));
                let Some(e) = fs.nodes.get(&k) else {
                    return E_NOT_FOUND;
                };
                if e.dir {
                    return E_ACCESS;
                }
                fill(buf, e);
                // 番号は 1〜0xFFFE を順に使う（0xFFFF は「なし」）
                let mut h = self.next_handle;
                for _ in 0..0xFFFE {
                    h = if h >= 0xFFFE { 1 } else { h + 1 };
                    if !self.handles.get(&h).is_some_and(|x| x.1) {
                        break;
                    }
                }
                self.next_handle = h;
                self.handles.insert(h, (k, true));
                w16(buf, 0x0A, h);
                OK
            }
            7..=9 => {
                let h = r16(buf, 0x0A);
                let Some((k, true)) = self.handles.get(&h).cloned() else {
                    return E_HANDLE;
                };
                let Some(e) = fs.nodes.get_mut(&k) else {
                    return E_HANDLE;
                };
                let pos = r32(buf, 0x14) as usize;
                if cmd == 9 {
                    if pos as u64 > MAX_BYTES {
                        return E_FULL;
                    }
                    e.data.resize(pos, 0);
                    return OK;
                }
                let n = (r32(buf, 0x10)).min(MAX_XFER);
                let dpa = r32(buf, 0x18);
                if dpa < BUF_LO || dpa + n > BUF_HI {
                    return E_PARAM;
                }
                let Some(d) = ram.ram_slice_mut(dpa, n) else {
                    return E_PARAM;
                };
                if cmd == 7 {
                    let m = e.data.len().saturating_sub(pos).min(n as usize);
                    d[..m].copy_from_slice(&e.data[pos..pos + m]);
                    w32(buf, 0x10, m as u32);
                } else {
                    let end = pos + n as usize;
                    if end > e.data.len() {
                        if end as u64 > MAX_BYTES {
                            return E_FULL;
                        }
                        e.data.resize(end, 0);
                    }
                    e.data[pos..end].copy_from_slice(d);
                    w32(buf, 0x10, n);
                }
                OK
            }
            0x0A => match self.handles.get_mut(&r16(buf, 0x0A)) {
                Some(x) if x.1 => {
                    x.1 = false;
                    OK
                }
                _ => E_HANDLE,
            },
            0x0B => {
                let total = MAX_BYTES / CLUSTER;
                let used = fs.used_bytes().div_ceil(CLUSTER);
                w32(buf, 0x10, total as u32);
                w32(buf, 0x14, total.saturating_sub(used) as u32);
                OK
            }
            0x0D | 0x10 => {
                let k = key(&path_at(buf, 0x22, 0x24));
                let Some(e) = fs.nodes.get(&k) else {
                    return E_NOT_FOUND;
                };
                if e.dir != (cmd == 0x0D) {
                    return E_ACCESS;
                }
                if e.dir && !fs.children(&k).is_empty() {
                    return E_ACCESS;
                }
                if !e.dir && self.handles.values().any(|(p, open)| *open && *p == k) {
                    return E_ACCESS;
                }
                fs.nodes.remove(&k);
                OK
            }
            0x0E => {
                let h = r16(buf, 0x0A);
                let k = if h != 0xFFFF {
                    match self.handles.get(&h) {
                        Some((k, _)) => k.clone(),
                        None => return E_HANDLE,
                    }
                } else {
                    key(&path_at(buf, 0x22, 0x24))
                };
                let Some(e) = fs.nodes.get_mut(&k) else {
                    return E_NOT_FOUND;
                };
                let a = r16(buf, 0x1C);
                e.attrs = if e.dir { a | ATTR_DIR } else { a & !ATTR_DIR };
                let (mt, ct) = (r32(buf, 0x0C), r32(buf, 0x428));
                if mt != 0 {
                    e.mtime = mt;
                }
                if ct != 0 {
                    e.ctime = ct;
                }
                OK
            }
            0x0F => {
                let from = key(&path_at(buf, 0x22, 0x24));
                let to_path = path_at(buf, 0x224, 0x226);
                let to = key(&to_path);
                if !fs.nodes.contains_key(&from) {
                    return E_NOT_FOUND;
                }
                if fs.nodes.contains_key(&to) {
                    return E_EXISTS;
                }
                if !fs.is_dir(parent(&to)) || to.starts_with(&format!("{from}\\")) {
                    return E_PATH;
                }
                // 自分と子を新しいパスへ（キーは小文字の形なので接頭辞で選べる）
                let prefix = format!("{from}\\");
                let moved: Vec<String> = fs
                    .nodes
                    .keys()
                    .filter(|x| **x == from || x.starts_with(&prefix))
                    .cloned()
                    .collect();
                for old in moved {
                    let mut e = fs.nodes.remove(&old).expect("listed");
                    let new = format!("{to}{}", &old[from.len()..]);
                    if old == from {
                        e.name = to_path
                            .trim_end_matches('\\')
                            .rsplit('\\')
                            .next()
                            .unwrap_or("")
                            .to_string();
                    }
                    fs.nodes.insert(new, e);
                }
                for (p, _) in self.handles.values_mut() {
                    if *p == from || p.starts_with(&prefix) {
                        *p = format!("{to}{}", &p[from.len()..]);
                    }
                }
                OK
            }
            _ => E_PARAM,
        }
    }

    pub fn save_state(&self, e: &mut Encoder) {
        let DeShare {
            fs,
            buf_pa,
            result,
            event,
            reg5000,
            handles,
            next_handle,
            dirs,
            pending,
        } = self;
        debug_assert!(
            pending.is_none(),
            "commands finish before the next instruction"
        );
        e.u32(*buf_pa);
        e.u32(*result);
        e.bool(*event);
        e.u32(*reg5000);
        e.u16(*next_handle);
        e.u64(handles.len() as u64);
        for (h, (p, open)) in handles {
            e.u16(*h);
            e.bytes(p.as_bytes());
            e.bool(*open);
        }
        e.u64(dirs.len() as u64);
        for d in dirs {
            e.bytes(d.as_bytes());
        }
        e.bool(fs.is_some());
        if let Some(fs) = fs {
            e.u64(fs.nodes.len() as u64);
            for (k, n) in &fs.nodes {
                e.bytes(k.as_bytes());
                e.bytes(n.name.as_bytes());
                e.bool(n.dir);
                e.u16(n.attrs);
                e.u32(n.mtime);
                e.u32(n.ctime);
                e.bytes(&n.data);
            }
        }
    }

    pub fn load_state(d: &mut Decoder) -> Result<DeShare, Error> {
        let s = |d: &mut Decoder| -> Result<String, Error> {
            let b = d.bytes()?;
            match std::str::from_utf8(b) {
                Ok(s) => Ok(s.to_string()),
                Err(_) => d.err("bad string"),
            }
        };
        let mut x = DeShare {
            buf_pa: d.u32()?,
            result: d.u32()?,
            event: d.bool()?,
            reg5000: d.u32()?,
            next_handle: d.u16()?,
            ..Default::default()
        };
        let n = d.count(0x10000)?;
        for _ in 0..n {
            let h = d.u16()?;
            let p = s(d)?;
            let open = d.bool()?;
            x.handles.insert(h, (p, open));
        }
        let n = d.count(1 << 20)?;
        for _ in 0..n {
            x.dirs.push(s(d)?);
        }
        if d.bool()? {
            let mut fs = ShareFs::new();
            let n = d.count(1 << 24)?;
            for _ in 0..n {
                let k = s(d)?;
                let name = s(d)?;
                let e = ShareEntry {
                    name,
                    dir: d.bool()?,
                    attrs: d.u16()?,
                    mtime: d.u32()?,
                    ctime: d.u32()?,
                    data: d.bytes()?.to_vec(),
                };
                fs.nodes.insert(k, e);
            }
            x.fs = Some(fs);
        }
        Ok(x)
    }
}

/// ディレクトリの印（1 から）。
fn intern(dirs: &mut Vec<String>, d: &str) -> u32 {
    match dirs.iter().position(|x| x == d) {
        Some(i) => i as u32 + 1,
        None => {
            dirs.push(d.to_string());
            dirs.len() as u32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 共有バッファ・データバッファの範囲だけを持つ RAM。
    struct Ram(Vec<u8>);

    impl RamAccess for Ram {
        fn ram_slice_mut(&mut self, pa: u32, len: u32) -> Option<&mut [u8]> {
            let o = pa.checked_sub(BUF_LO)? as usize;
            self.0.get_mut(o..o + len as usize)
        }
    }

    const CMD: u32 = 0x33EF_F000;
    const DATA: u32 = 0x33EE_F000;

    struct T {
        d: DeShare,
        ram: Ram,
    }

    impl T {
        fn new() -> T {
            let mut fs = ShareFs::new();
            let e = |name: &str, dir: bool, data: &[u8]| ShareEntry {
                name: name.into(),
                dir,
                attrs: if dir { ATTR_DIR } else { ATTR_ARCHIVE },
                mtime: 1,
                ctime: 1,
                data: data.to_vec(),
            };
            fs.insert("\\Dir", e("Dir", true, &[])).unwrap();
            fs.insert("\\Dir\\a.txt", e("a.txt", false, b"hello"))
                .unwrap();
            fs.insert("\\b.txt", e("b.txt", false, b"")).unwrap();
            let mut d = DeShare::default();
            d.insert(fs);
            T {
                d,
                ram: Ram(vec![0; (BUF_HI - BUF_LO) as usize]),
            }
        }
        fn buf(&mut self) -> &mut [u8] {
            self.ram.ram_slice_mut(CMD, CMD_BUF_LEN).unwrap()
        }
        /// vcefsd の手順: バッファを消して大きさを書き、+0 に番地、+4 にコマンド。
        fn reset(&mut self) {
            let b = self.buf();
            b.fill(0);
            b[0..2].copy_from_slice(&(CMD_BUF_LEN as u16).to_le_bytes());
            b[0xA..0xC].copy_from_slice(&0xFFFFu16.to_le_bytes());
            b[4..8].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        }
        fn path(&mut self, p: &str) {
            let u: Vec<u16> = p.encode_utf16().collect();
            let b = self.buf();
            b[0x22..0x24].copy_from_slice(&((u.len() * 2) as u16).to_le_bytes());
            for (i, x) in u.iter().enumerate() {
                b[0x24 + 2 * i..0x26 + 2 * i].copy_from_slice(&x.to_le_bytes());
            }
        }
        fn set16(&mut self, o: usize, v: u16) {
            self.buf()[o..o + 2].copy_from_slice(&v.to_le_bytes());
        }
        fn set32(&mut self, o: usize, v: u32) {
            self.buf()[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
        fn get16(&mut self, o: usize) -> u16 {
            let b = self.buf();
            u16::from_le_bytes([b[o], b[o + 1]])
        }
        fn get32(&mut self, o: usize) -> u32 {
            let b = self.buf();
            u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
        }
        fn cmd(&mut self, c: u32) -> u32 {
            self.d.write(0x4000, CMD);
            self.d.write(0x4004, c);
            self.d.after_write(&mut self.ram, 0x5C22_7800);
            assert_eq!(self.d.read(0x4008), 0);
            self.d.read(0x400C)
        }
        fn name(&mut self) -> String {
            let n = self.get16(0x22) as usize / 2;
            let u: Vec<u16> = (0..n).map(|i| self.get16(0x24 + 2 * i)).collect();
            String::from_utf16_lossy(&u)
        }
    }

    #[test]
    fn notify_and_ack() {
        let mut t = T::new();
        assert_eq!(t.d.read(0x5004), 1 << 30);
        assert_eq!(t.d.read(0x400C), 0, "0 = inserted");
        t.d.write(0x4004, 0);
        t.d.after_write(&mut t.ram, 0);
        assert_eq!(t.d.read(0x5004), 0);
        assert!(t.d.eject().is_some());
        assert_eq!((t.d.read(0x5004), t.d.read(0x400C)), (1 << 30, 1));
    }

    /// 名前で引く・列挙（FindFirst の流れ: 枠へ写し、+8 を増やして引き直す）。
    #[test]
    fn lookup_and_enumerate() {
        let mut t = T::new();
        assert_eq!(t.cmd(0x15), MAX_XFER);
        t.reset();
        t.set16(8, 0xFFFF);
        t.path("\\dir");
        assert_eq!(t.cmd(0x11), OK);
        assert_eq!(t.get16(0x1C) & ATTR_DIR, ATTR_DIR);
        t.path("\\DIR\\A.TXT");
        assert_eq!(t.cmd(0x11), OK);
        assert_eq!(t.get32(0x10), 5);
        t.path("\\nope");
        assert_ne!(t.cmd(0x11), OK);
        // 根そのもの（WM6 の vcefsd.dll が一覧の前に引く）: ディレクトリ
        t.reset();
        t.set16(8, 0xFFFF);
        t.path("\\");
        assert_eq!(t.cmd(0x11), OK);
        assert_eq!(t.get16(0x1C), ATTR_DIR);
        assert_eq!(t.get32(0x10), 0);
        // "\*.*" の列挙: 枠の番号なし・+8 = 0 で始め、枠の番号を入れて続ける
        t.reset();
        t.set16(8, 0);
        t.path("\\*.*");
        assert_eq!(t.cmd(0x11), OK);
        let mut names = vec![];
        t.set32(4, 3);
        for i in 0..5u16 {
            t.set16(8, i);
            if t.cmd(0x11) != OK {
                break;
            }
            names.push(t.name());
        }
        assert_eq!(names, ["b.txt", "Dir"]);
    }

    /// 作成 → 開く → 書く → 読む → 閉じる → 属性（閉じた後のハンドルで）→ 移動 → 削除。
    #[test]
    fn file_lifecycle() {
        let mut t = T::new();
        t.reset();
        t.path("\\Dir\\New.txt");
        assert_eq!(t.cmd(5), OK);
        assert_eq!(t.cmd(6), OK);
        let h = t.get16(0xA);
        assert_ne!(h, 0xFFFF);
        t.ram
            .ram_slice_mut(DATA, 3)
            .unwrap()
            .copy_from_slice(b"xyz");
        t.set32(0x10, 3);
        t.set32(0x14, 2);
        t.set32(0x18, DATA);
        assert_eq!(t.cmd(8), OK);
        t.set32(0x10, 100);
        t.set32(0x14, 0);
        assert_eq!(t.cmd(7), OK);
        assert_eq!(t.get32(0x10), 5);
        assert_eq!(t.ram.ram_slice_mut(DATA, 5).unwrap(), b"\0\0xyz");
        assert_eq!(t.cmd(0x0A), OK);
        t.set16(0x1C, 0x21);
        t.set32(0xC, 0x1234_5678);
        assert_eq!(t.cmd(0x0E), OK, "attributes by the closed handle");
        t.reset();
        t.path("\\Dir\\New.txt");
        let to: Vec<u16> = "\\Moved.txt".encode_utf16().collect();
        t.set16(0x224, (to.len() * 2) as u16);
        for (i, x) in to.iter().enumerate() {
            t.set16(0x226 + 2 * i, *x);
        }
        assert_eq!(t.cmd(0x0F), OK);
        t.reset();
        t.path("\\moved.txt");
        t.set16(8, 0xFFFF);
        assert_eq!(t.cmd(0x11), OK);
        assert_eq!(
            (t.get16(0x1C), t.get32(0xC), t.get32(0x10)),
            (0x21, 0x1234_5678, 5)
        );
        assert_eq!(t.cmd(0x10), OK);
        assert_ne!(t.cmd(0x11), OK);
        // 空でないディレクトリは消せない
        t.path("\\Dir");
        assert_ne!(t.cmd(0x0D), OK);
    }

    #[test]
    fn state_round_trip() {
        let mut t = T::new();
        t.reset();
        t.path("\\b.txt");
        assert_eq!(t.cmd(6), OK);
        let mut w = crate::snapshot::Writer::new(Vec::new(), "t", "").unwrap();
        w.chunk("deshare", DeShare::STATE_VERSION, |e| t.d.save_state(e))
            .unwrap();
        let bytes = w.finish().unwrap();
        let mut r = crate::snapshot::Reader::new(&bytes[..]).unwrap();
        let c = r.expect("deshare").unwrap();
        let mut dec = c.decoder(DeShare::STATE_VERSION).unwrap();
        let back = DeShare::load_state(&mut dec).unwrap();
        dec.finish().unwrap();
        assert_eq!(back, t.d);
    }
}
