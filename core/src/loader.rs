//! Windows CE のカーネルイメージ（nk.bin 等）を読み込み、形式に依存しない
//! 中間表現 [`Image`] に変換する。
//!
//! 対応形式:
//!   - Windows CE BIN 形式（"B000FF" 署名 + レコード列）: Platform Builder の出力
//!   - .nb0 生形式（ヘッダなしのフラットイメージ）
//!   - フラッシュのイメージ（WM6 の Device Emulator 用。ヘッダなし、先頭がブートローダ）
//!   - .words（命令語のテキスト。合成プログラム用で、実イメージの形式ではない）
//!
//! コアはファイルに触れないので、読み込みはバイト列から行う（ファイルを読むのは
//! cli・web の責務）。壊れた入力でも panic せずエラーを返す。

use std::fmt;

/// イメージ内の連続したデータ片。`addr` はイメージファイルに記載されたアドレスで、
/// 通常は CE カーネルの仮想アドレス（0x80000000 台）。物理アドレスへの変換は
/// machine 側の責務。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub addr: u32,
    pub data: Vec<u8>,
}

/// イメージの形式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Bin,
    Nb0,
    /// バンク0 の NOR フラッシュの中身（segs は物理アドレス 0 から 1 つ）
    Flash,
    Words,
}

impl fmt::Display for Format {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Format::Bin => "bin",
            Format::Nb0 => "nb0",
            Format::Flash => "flash",
            Format::Words => "words",
        })
    }
}

/// 形式共通の中間表現。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub format: Format,
    /// イメージ全体の開始アドレス
    pub start: u32,
    /// イメージ全体の長さ（バイト）
    pub length: u32,
    /// エントリポイント
    pub entry: u32,
    /// ロードすべきデータ片（アドレス順とは限らない）
    pub segs: Vec<Segment>,
    /// BIN 形式のレコード（info 表示用。終端レコードは含まない。他の形式では空）
    pub records: Vec<BinRecord>,
}

/// BIN 形式の 1 レコード（チェックサム等の表示用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinRecord {
    pub addr: u32,
    pub len: u32,
    pub checksum: u32,
}

/// 読み込みのエラー。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadError(pub String);

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "loader: {}", self.0)
    }
}

impl std::error::Error for LoadError {}

fn err<T>(msg: String) -> Result<T, LoadError> {
    Err(LoadError(msg))
}

/// BIN 形式の署名。
pub const BIN_MAGIC: &[u8] = b"B000FF\x0A";

/// イメージを読み込む。形式は先頭の署名で判別し、署名がなければファイル名
/// `name` の拡張子で .nb0（生形式）か .words（命令語のテキスト）を選ぶ。
/// `nb0_base` は .nb0 のときのロード先アドレス（他の形式では無視される）。
pub fn load(data: &[u8], name: &str, nb0_base: u32) -> Result<Image, LoadError> {
    if data.starts_with(BIN_MAGIC) {
        return load_bin(data);
    }
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".nb0") {
        return load_nb0(data, nb0_base);
    }
    if lower.ends_with(".words") {
        return load_words(data);
    }
    if is_flash(data) {
        return load_flash(data);
    }
    err(format!(
        "{name}: unknown image format (no B000FF magic, not a flash image, not .nb0/.words)"
    ))
}

/// バイト列を先頭から読む小さな読み手（範囲外は None。panic しない）。
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn u32(&mut self) -> Option<u32> {
        let b = self.take(4)?;
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
}

/// Windows CE BIN 形式を読み込む。
///
/// ```text
/// 署名     "B000FF\x0A"（7 バイト）
/// ヘッダ   start u32LE, length u32LE  … イメージ全体の範囲
/// レコード addr u32LE, len u32LE, checksum u32LE, data[len]
/// 終端     addr == 0 のレコード。len フィールドがエントリポイント
///          （checksum は 0、データなし）。実イメージ（WM5 SDK の
///          PPC_USA.bin）で確認済みの規則。
/// ```
///
/// checksum は data の全バイトの単純和（u32、桁あふれは無視）。チェックサム
/// 不一致はエラーにする（壊れたイメージを黙って実行しても原因究明が困難に
/// なるだけのため）。
pub fn load_bin(data: &[u8]) -> Result<Image, LoadError> {
    let mut r = Reader { data, pos: 0 };
    match r.take(BIN_MAGIC.len()) {
        Some(m) if m == BIN_MAGIC => {}
        Some(m) => return err(format!("bad BIN magic {:?}", String::from_utf8_lossy(m))),
        None => return err("reading BIN magic: unexpected EOF".into()),
    }
    let Some(start) = r.u32() else {
        return err("reading image start: unexpected EOF".into());
    };
    let Some(length) = r.u32() else {
        return err("reading image length: unexpected EOF".into());
    };
    let mut img = Image {
        format: Format::Bin,
        start,
        length,
        entry: 0,
        segs: vec![],
        records: vec![],
    };
    for i in 0.. {
        let (Some(addr), Some(rlen), Some(sum)) = (r.u32(), r.u32(), r.u32()) else {
            return err(format!("record {i}: reading header: unexpected EOF"));
        };
        if addr == 0 {
            // 終端レコード: len フィールドがエントリポイント。
            img.entry = rlen;
            return Ok(img);
        }
        if rlen == 0 {
            // データなしレコード。一部のツールは {addr=entry, len=0} を
            // 終端として書くという情報もあるため、同様に終端として扱う。
            img.entry = addr;
            return Ok(img);
        }
        // 長さはファイルの残りと比べてから確保する（細工された巨大な長さで
        // メモリを取らないため）。
        let Some(body) = r.take(rlen as usize) else {
            return err(format!(
                "record {i} (addr={addr:08X} len={rlen}): reading data: unexpected EOF"
            ));
        };
        let got = byte_sum(body);
        if got != sum {
            return err(format!(
                "record {i} (addr={addr:08X} len={rlen}): checksum mismatch: file says {sum:08X}, computed {got:08X}"
            ));
        }
        img.records.push(BinRecord {
            addr,
            len: rlen,
            checksum: sum,
        });
        img.segs.push(Segment {
            addr,
            data: body.to_vec(),
        });
    }
    unreachable!()
}

/// BIN 形式のチェックサム（データ全バイトの和、mod 2^32）。
fn byte_sum(data: &[u8]) -> u32 {
    data.iter().fold(0u32, |s, &b| s.wrapping_add(b as u32))
}

/// ヘッダなしの生イメージを読み込む。`base` にそのまま配置され、エントリ
/// ポイントは先頭アドレスと仮定する。
/// TODO: 実イメージで「エントリ = 先頭」の仮定を確認する（イメージ先頭が
/// スタートアップコードへのジャンプになっているのが通例のはずだが未検証）。
pub fn load_nb0(data: &[u8], base: u32) -> Result<Image, LoadError> {
    if data.is_empty() {
        return err("empty nb0 image".into());
    }
    let Ok(length) = u32::try_from(data.len()) else {
        return err("nb0 image larger than 4GB".into());
    };
    Ok(Image {
        format: Format::Nb0,
        start: base,
        length,
        entry: base,
        segs: vec![Segment {
            addr: base,
            data: data.to_vec(),
        }],
        records: vec![],
    })
}

/// フラッシュのイメージとして受け付ける大きさの上限（128MB。載せられるかは
/// マシンの構成が決める）。
pub const FLASH_MAX: u32 = 0x08000000;

/// フラッシュのイメージか。根拠（2026-09-30。WM6 の JPN 版（Professional Images の msi）の
/// PPC_JPN.bin）: 96MB ちょうどのヘッダなしのイメージで、先頭が分岐命令（リセット
/// ベクタ。`b` で 0x1000 へ）、+0x40 に 'CECE' の署名（ブートローダの ROMHDR への
/// ポインタが続く）。OEMAddressTable が VA 0x88000000 → PA 0 を 96MB として持ち、
/// リセットで PA 0 から IPL が走る。
fn is_flash(data: &[u8]) -> bool {
    let n = data.len();
    n >= 0x1000
        && n as u64 <= FLASH_MAX as u64
        && n.is_multiple_of(0x1000)
        && data[3] == 0xEA // 条件 AL の B
        && &data[0x40..0x44] == b"ECEC"
}

/// フラッシュのイメージを読み込む（物理アドレス 0 に置き、リセットで 0 から実行）。
pub fn load_flash(data: &[u8]) -> Result<Image, LoadError> {
    if !is_flash(data) {
        return err("not a flash image".into());
    }
    Ok(Image {
        format: Format::Flash,
        start: 0,
        length: data.len() as u32,
        entry: 0,
        segs: vec![Segment {
            addr: 0,
            data: data.to_vec(),
        }],
        records: vec![],
    })
}

/// 命令語のテキスト（拡張子 .words）を読み込む。合成プログラム（テスト・一致
/// 確認の基準シナリオ synthetic-*）を、コアのテストと CLI で共有する形式。
///
/// 書式（1 行 1 項目、"#" 以降はコメント、16 進は 0x なしでも可）:
///
/// ```text
/// entry <アドレス>   エントリポイント（CE 仮想アドレス。1 回だけ）
/// org <アドレス>     以降の語を置くアドレス（新しいセグメントを始める。4 の倍数）
/// <語>               32 ビットの語（8 桁以下の 16 進）。LE で置き、アドレスを 4 進める
/// ```
///
/// 置かれなかった場所は RAM の初期値（0）のまま。
pub fn load_words(data: &[u8]) -> Result<Image, LoadError> {
    let Ok(text) = std::str::from_utf8(data) else {
        return err("words: not UTF-8".into());
    };
    let mut img = Image {
        format: Format::Words,
        start: 0,
        length: 0,
        entry: 0,
        segs: vec![],
        records: vec![],
    };
    let mut entry = None;
    // 次の語を置くアドレス（32 ビットを越えたら誤り）と、置いた範囲 [lo, hi)。
    let mut addr: u64 = 0;
    let (mut lo, mut hi) = (1u64 << 32, 0u64);
    for (i, raw) in text.lines().enumerate() {
        let line_no = i + 1;
        let line = raw.split('#').next().unwrap_or("");
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.is_empty() {
            continue;
        }
        let bad = |msg: String| err(format!("words line {line_no}: {msg}"));
        match f[0] {
            "entry" | "org" => {
                if f.len() != 2 {
                    return bad(format!("{} wants one address", f[0]));
                }
                let v = match parse_hex32(f[1]) {
                    Ok(v) => v,
                    Err(e) => return bad(e),
                };
                if f[0] == "entry" {
                    if entry.is_some() {
                        return bad("duplicate entry".into());
                    }
                    entry = Some(v);
                    continue;
                }
                if v % 4 != 0 {
                    return bad(format!("org {v:08X} is not word aligned"));
                }
                img.segs.push(Segment {
                    addr: v,
                    data: vec![],
                });
                addr = v as u64;
            }
            _ => {
                let Some(seg) = img.segs.last_mut() else {
                    return bad("word before the first org".into());
                };
                if f.len() != 1 {
                    return bad("one word per line".into());
                }
                let w = match parse_hex32(f[0]) {
                    Ok(v) => v,
                    Err(e) => return bad(e),
                };
                if addr + 4 > 1 << 32 {
                    return bad("address overflows 32 bits".into());
                }
                seg.data.extend_from_slice(&w.to_le_bytes());
                lo = lo.min(addr);
                hi = hi.max(addr + 4);
                addr += 4;
            }
        }
    }
    let Some(entry) = entry else {
        return err("words: no entry".into());
    };
    if hi == 0 {
        return err("words: no words".into());
    }
    img.entry = entry;
    img.start = lo as u32;
    img.length = (hi - lo) as u32;
    Ok(img)
}

fn parse_hex32(s: &str) -> Result<u32, String> {
    let t = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if t.is_empty() || t.len() > 8 {
        return Err(format!("bad hex value {t:?}"));
    }
    u32::from_str_radix(t, 16).map_err(|_| format!("bad hex value {t:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成 BIN イメージを組み立てる。break_checksum なら最初のレコードの
    /// チェックサムを壊す。
    fn make_bin(
        start: u32,
        length: u32,
        entry: u32,
        recs: &[Segment],
        break_checksum: bool,
    ) -> Vec<u8> {
        let mut b = BIN_MAGIC.to_vec();
        let w = |b: &mut Vec<u8>, v: u32| b.extend_from_slice(&v.to_le_bytes());
        w(&mut b, start);
        w(&mut b, length);
        for (i, r) in recs.iter().enumerate() {
            let mut sum = byte_sum(&r.data);
            if break_checksum && i == 0 {
                sum = sum.wrapping_add(1);
            }
            w(&mut b, r.addr);
            w(&mut b, r.data.len() as u32);
            w(&mut b, sum);
            b.extend_from_slice(&r.data);
        }
        // 終端レコード（実イメージの規則）: addr = 0, len = entry, checksum = 0
        w(&mut b, 0);
        w(&mut b, entry);
        w(&mut b, 0);
        b
    }

    #[test]
    fn bin() {
        let recs = vec![
            Segment {
                addr: 0x80070000,
                data: vec![1, 2, 3, 4],
            },
            Segment {
                addr: 0x80100000,
                data: vec![0xFF; 16],
            },
        ];
        let raw = make_bin(0x80070000, 0x00200000, 0x80071000, &recs, false);
        let img = load_bin(&raw).unwrap();
        assert_eq!(img.format, Format::Bin);
        assert_eq!(
            (img.start, img.length, img.entry),
            (0x80070000, 0x00200000, 0x80071000)
        );
        assert_eq!(img.segs, recs);
        assert_eq!(img.records.len(), 2);
        assert_eq!(img.records[1].checksum, 16 * 0xFF);
        // load も署名で BIN と判別する（名前は関係ない）
        assert_eq!(load(&raw, "x.nb0", 0).unwrap(), img);
    }

    #[test]
    fn bin_checksum_mismatch() {
        let recs = vec![Segment {
            addr: 0x80070000,
            data: vec![1, 2],
        }];
        assert!(load_bin(&make_bin(0x80070000, 0x1000, 0x80070000, &recs, true)).is_err());
    }

    /// {addr=entry, len=0} 形式の終端も受け付ける（別解釈のツール対策）。
    #[test]
    fn bin_len_zero_terminator() {
        let mut b = BIN_MAGIC.to_vec();
        for v in [0x80070000u32, 0x10, 0x80070000, 4, 10] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&[1, 2, 3, 4]); // checksum 10
        for v in [0x80071000u32, 0, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        assert_eq!(load_bin(&b).unwrap().entry, 0x80071000);
    }

    #[test]
    fn bin_bad_magic() {
        assert!(load_bin(b"NOTABIN\x0Axxxxxxxx").is_err());
    }

    #[test]
    fn bin_truncated() {
        let recs = vec![Segment {
            addr: 0x80070000,
            data: vec![1, 2, 3],
        }];
        let raw = make_bin(0x80070000, 0x1000, 0x80070000, &recs, false);
        // 終端レコードの途中で切る。どこで切っても panic せずエラーになる。
        assert!(load_bin(&raw[..raw.len() - 6]).is_err());
        for n in 0..raw.len() {
            assert!(load_bin(&raw[..n]).is_err(), "truncated at {n}");
        }
    }

    /// 細工された巨大な長さでも確保せずにエラーになる。
    #[test]
    fn bin_huge_length() {
        let mut b = BIN_MAGIC.to_vec();
        for v in [0u32, 0, 0x80000000, 0xFFFFFFFF, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        assert!(load_bin(&b).is_err());
    }

    #[test]
    fn nb0() {
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        let img = load_nb0(&data, 0x30000000).unwrap();
        assert_eq!(img.format, Format::Nb0);
        assert_eq!(
            (img.start, img.entry, img.length),
            (0x30000000, 0x30000000, 4)
        );
        assert_eq!(
            img.segs,
            vec![Segment {
                addr: 0x30000000,
                data: data.to_vec()
            }]
        );
        assert_eq!(load(&data, "IMAGE.NB0", 0x30000000).unwrap(), img);
    }

    #[test]
    fn nb0_empty() {
        assert!(load_nb0(&[], 0).is_err());
    }

    #[test]
    fn flash() {
        let mut data = vec![0u8; 0x2000];
        data[0..4].copy_from_slice(&0xEA0003FEu32.to_le_bytes());
        data[0x40..0x44].copy_from_slice(b"ECEC");
        let img = load(&data, "PPC_JPN.bin", 0x30000000).unwrap();
        assert_eq!(img.format, Format::Flash);
        assert_eq!((img.start, img.length, img.entry), (0, 0x2000, 0));
        assert_eq!(
            img.segs,
            vec![Segment {
                addr: 0,
                data: data.clone()
            }]
        );
        // 大きさが 4KB の倍数でない・署名がない・.nb0 の名前なら違う
        assert!(load(&data[..0x1FFC], "x.bin", 0).is_err());
        data[0x40] = 0;
        assert!(load(&data, "x.bin", 0).is_err());
        data[0x40] = b'E';
        assert_eq!(load(&data, "x.nb0", 0).unwrap().format, Format::Nb0);
    }

    #[test]
    fn unknown_format() {
        assert!(load(b"abc", "x.img", 0).is_err());
    }

    #[test]
    fn words() {
        let src = "# comment\nentry 0x80000100\norg 80000000\nEA00003E   # B 0x100\n1\norg 0x80000100\nDEADBEEF\n";
        let img = load(src.as_bytes(), "p.words", 0).unwrap();
        assert_eq!(img.format, Format::Words);
        assert_eq!(
            (img.entry, img.start, img.length),
            (0x80000100, 0x80000000, 0x104)
        );
        assert_eq!(
            img.segs,
            vec![
                Segment {
                    addr: 0x80000000,
                    data: vec![0x3E, 0, 0, 0xEA, 1, 0, 0, 0]
                },
                Segment {
                    addr: 0x80000100,
                    data: vec![0xEF, 0xBE, 0xAD, 0xDE]
                },
            ]
        );
    }

    #[test]
    fn words_errors() {
        for src in [
            "org 0\n1\n",                    // entry なし
            "entry 0\n",                     // 語なし
            "entry 0\n1\n",                  // org の前の語
            "entry 0\nentry 0\norg 0\n1\n",  // entry が 2 回
            "entry 0\norg 2\n1\n",           // 非アライン
            "entry 0\norg 0\n123456789\n",   // 9 桁
            "entry 0\norg 0\nXYZ\n",         // 16 進でない
            "entry 0\norg 0\n1 2\n",         // 1 行 2 語
            "entry 0\norg FFFFFFFC\n1\n2\n", // 32 ビットを越える
            "entry\norg 0\n1\n",             // 引数なし
        ] {
            assert!(load_words(src.as_bytes()).is_err(), "{src:?}: want error");
        }
    }

    /// testdata の合成プログラムが読めること。
    #[test]
    fn synthetic_programs() {
        let idle = include_bytes!("../../testdata/golden/synthetic/idle.words");
        let img = load_words(idle).unwrap();
        assert_eq!(img.entry, 0x80000100);
        let adc = include_bytes!("../../testdata/golden/synthetic/adc-poll.words");
        assert_eq!(load_words(adc).unwrap().entry, 0x80001000);
    }

    /// 実イメージ（CERULEAN_IMAGE があるときだけ）: CLAUDE.md の確認済みの事実。
    #[test]
    fn real_image() {
        let Some(path) = std::env::var_os("CERULEAN_IMAGE") else {
            return;
        };
        let data = std::fs::read(path).unwrap();
        let img = load(&data, "PPC_USA.bin", 0).unwrap();
        assert_eq!(
            (img.start, img.length, img.entry),
            (0x80070000, 0x01421ED0, 0x80076CF0)
        );
        assert_eq!(img.records.len(), 99);
    }
}
