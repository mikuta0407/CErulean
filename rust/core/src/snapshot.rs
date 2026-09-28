//! 全状態の保存形式（Rust 版。計画書 §4.3。Go 版の形式とは互換にしない）。
//!
//! ```text
//! 先頭    署名 "CRLNSNAP"(8) | 形式の版数 u32 | マシン名 str | イメージ ID str
//! チャンク 名前 str | 版数 u16 | 長さ u64 | 本体[長さ] | CRC-32(本体) u32
//!         …（マシンが決めた順）
//! 終端    名前 "end" | 版数 1 | 長さ 8 | チャンク数 u64 | CRC-32
//! ```
//!
//! - すべてリトルエンディアン。str は u16 の長さ＋UTF-8。
//! - 各チャンクの**前に**名前・版数・長さを置く（Go 版は長さを末尾に置いていた
//!   ため、スキーマを知らないとツールで分割できなかった）。本体の CRC-32 で
//!   壊れたファイルを検出する（ブラウザでは壊れた自動保存を除外して 1 つ前の
//!   世代に戻るのに使う。計画書 §7.3）。
//! - コアは圧縮しない（無圧縮のストリームを出す）。圧縮は CLI・ブラウザの側で行う
//!   （方式は段階2 の計測の後に決める）。
//! - 形式の版数とチャンクの版数は、未知・非対応なら必ずエラーにする。段階3 で
//!   公開した後は旧版の読み込みを残す。
//! - 読み込みは壊れたファイル・細工されたファイルでも panic せずエラーを返す
//!   （長さには上限を設ける。ブラウザでは panic で wasm のインスタンスが
//!   使えなくなるため）。
//! - 同じ状態からは同じバイト列になる（回帰テストに使う）。
//!
//! 各部品は「保存する状態」をエンコーダに書き、デコーダから読む。保存では
//! 状態の構造体を `..` なしで全フィールド分解し、フィールドを足して保存を
//! 忘れるとコンパイルエラーになるようにする（Go の CheckFields の代わり）。
//! 派生情報のフィールドは分解で `_` と明示する。

use std::fmt;
use std::io::{Read, Write};

/// ファイル先頭の署名。
pub const MAGIC: &[u8; 8] = b"CRLNSNAP";
/// 形式の版数（コンテナの書式を変えたら上げる）。
pub const FORMAT_VERSION: u32 = 1;
/// 1 チャンクの本体の上限（RAM 128MB に余裕を持たせた値。細工された長さで
/// 巨大な確保をしないため）。
pub const MAX_CHUNK: u64 = 512 << 20;

/// スナップショットの読み書きのエラー。
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// 形式の誤り（署名・版数・長さ・CRC・中身の不整合）
    Format(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(e) => write!(f, "snapshot: {e}"),
            Error::Format(m) => write!(f, "snapshot: {m}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub fn format_err<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error::Format(msg.into()))
}

// ---- CRC-32（IEEE 802.3。多項式 0xEDB88320 の表引き）----

static CRC_TABLE: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB88320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
};

/// CRC-32 を続きから計算する（crc は前回の戻り値。最初は 0）。
pub fn crc32(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

// ---- エンコーダ（チャンク本体）----

/// チャンク本体を組み立てる。
#[derive(Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32s(&mut self, v: &[u32]) {
        for &x in v {
            self.u32(x);
        }
    }
    /// 長さ（u64）つきのバイト列。
    pub fn bytes(&mut self, v: &[u8]) {
        self.u64(v.len() as u64);
        self.buf.extend_from_slice(v);
    }
}

// ---- デコーダ（チャンク本体）----

/// チャンク本体を読む。足りなければエラー（panic しない）。
pub struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
    name: &'a str,
}

impl<'a> Decoder<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        match self.buf.get(self.pos..self.pos.saturating_add(n)) {
            Some(s) if self.pos.checked_add(n).is_some() => {
                self.pos += n;
                Ok(s)
            }
            _ => format_err(format!("{}: truncated", self.name)),
        }
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let s = self.take(N)?;
        let mut a = [0; N];
        a.copy_from_slice(s);
        Ok(a)
    }
    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.arr::<1>()?[0])
    }
    pub fn bool(&mut self) -> Result<bool, Error> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            v => format_err(format!("{}: bad bool {v}", self.name)),
        }
    }
    pub fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    pub fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    pub fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    pub fn i64(&mut self) -> Result<i64, Error> {
        Ok(i64::from_le_bytes(self.arr()?))
    }
    pub fn u32s<const N: usize>(&mut self) -> Result<[u32; N], Error> {
        let mut a = [0; N];
        for x in a.iter_mut() {
            *x = self.u32()?;
        }
        Ok(a)
    }
    /// 長さつきのバイト列（残りの長さを超える長さはエラー）。
    pub fn bytes(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u64()?;
        let n = usize::try_from(n)
            .map_err(|_| Error::Format(format!("{}: length {n} too large", self.name)))?;
        self.take(n)
    }
    /// 要素数（上限つき）。
    pub fn count(&mut self, max: u64) -> Result<u64, Error> {
        let n = self.u64()?;
        if n > max {
            return format_err(format!("{}: count {n} exceeds {max}", self.name));
        }
        Ok(n)
    }
    /// 読み残しがないこと（書き手と読み手の食い違いの検出）。
    pub fn finish(self) -> Result<(), Error> {
        if self.pos != self.buf.len() {
            return format_err(format!(
                "{}: {} bytes left over",
                self.name,
                self.buf.len() - self.pos
            ));
        }
        Ok(())
    }
    pub fn err<T>(&self, msg: impl fmt::Display) -> Result<T, Error> {
        format_err(format!("{}: {msg}", self.name))
    }
}

// ---- コンテナ ----

fn write_str(w: &mut impl Write, s: &str) -> Result<(), Error> {
    let n = u16::try_from(s.len()).map_err(|_| Error::Format("string too long".into()))?;
    w.write_all(&n.to_le_bytes())?;
    w.write_all(s.as_bytes())?;
    Ok(())
}

fn read_exact<const N: usize>(r: &mut impl Read) -> Result<[u8; N], Error> {
    let mut a = [0; N];
    r.read_exact(&mut a).map_err(|e| match e.kind() {
        std::io::ErrorKind::UnexpectedEof => Error::Format("truncated".into()),
        _ => Error::Io(e),
    })?;
    Ok(a)
}

fn read_str(r: &mut impl Read) -> Result<String, Error> {
    let n = u16::from_le_bytes(read_exact(r)?) as usize;
    let mut b = vec![0; n];
    r.read_exact(&mut b)
        .map_err(|_| Error::Format("truncated string".into()))?;
    String::from_utf8(b).map_err(|_| Error::Format("string is not UTF-8".into()))
}

/// スナップショットを書く。
pub struct Writer<W: Write> {
    w: W,
    chunks: u64,
}

impl<W: Write> Writer<W> {
    /// 先頭（署名・形式の版数・マシン名・イメージ ID）を書く。
    pub fn new(mut w: W, machine: &str, image_id: &str) -> Result<Self, Error> {
        w.write_all(MAGIC)?;
        w.write_all(&FORMAT_VERSION.to_le_bytes())?;
        write_str(&mut w, machine)?;
        write_str(&mut w, image_id)?;
        Ok(Writer { w, chunks: 0 })
    }

    /// エンコーダで組み立てた本体を 1 チャンクとして書く。
    pub fn chunk(
        &mut self,
        name: &str,
        version: u16,
        body: impl FnOnce(&mut Encoder),
    ) -> Result<(), Error> {
        let mut e = Encoder::default();
        body(&mut e);
        self.raw_chunk(name, version, &e.buf)
    }

    /// 本体をそのまま 1 チャンクとして書く（RAM のような大きな本体を組み立て
    /// 直さないため）。
    pub fn raw_chunk(&mut self, name: &str, version: u16, body: &[u8]) -> Result<(), Error> {
        write_str(&mut self.w, name)?;
        self.w.write_all(&version.to_le_bytes())?;
        self.w.write_all(&(body.len() as u64).to_le_bytes())?;
        self.w.write_all(body)?;
        self.w.write_all(&crc32(0, body).to_le_bytes())?;
        self.chunks += 1;
        Ok(())
    }

    /// 終端のチャンクを書いて閉じる。
    pub fn finish(mut self) -> Result<W, Error> {
        let n = self.chunks;
        self.raw_chunk("end", 1, &n.to_le_bytes())?;
        Ok(self.w)
    }
}

/// スナップショットの先頭。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub machine: String,
    pub image_id: String,
}

/// スナップショットを読む。
pub struct Reader<R: Read> {
    r: R,
    chunks: u64,
    pub header: Header,
}

/// 読み込んだ 1 チャンク。
pub struct Chunk {
    pub name: String,
    pub version: u16,
    pub body: Vec<u8>,
}

impl Chunk {
    /// 本体のデコーダ（版数が want でなければエラー）。
    pub fn decoder(&self, want: u16) -> Result<Decoder<'_>, Error> {
        if self.version != want {
            return format_err(format!(
                "{}: unsupported version {} (want {want})",
                self.name, self.version
            ));
        }
        Ok(Decoder {
            buf: &self.body,
            pos: 0,
            name: &self.name,
        })
    }
}

impl<R: Read> Reader<R> {
    /// 先頭を読んで検査する。
    pub fn new(mut r: R) -> Result<Self, Error> {
        let magic: [u8; 8] = read_exact(&mut r)?;
        if &magic != MAGIC {
            return format_err("not a CErulean snapshot (bad magic)");
        }
        let v = u32::from_le_bytes(read_exact(&mut r)?);
        if v != FORMAT_VERSION {
            return format_err(format!("unsupported format version {v}"));
        }
        let machine = read_str(&mut r)?;
        let image_id = read_str(&mut r)?;
        Ok(Reader {
            r,
            chunks: 0,
            header: Header { machine, image_id },
        })
    }

    /// 次のチャンクを読む（CRC を検査する）。終端なら None。
    pub fn next_chunk(&mut self) -> Result<Option<Chunk>, Error> {
        let name = read_str(&mut self.r)?;
        let version = u16::from_le_bytes(read_exact(&mut self.r)?);
        let len = u64::from_le_bytes(read_exact(&mut self.r)?);
        if len > MAX_CHUNK {
            return format_err(format!("{name}: chunk length {len} exceeds the limit"));
        }
        let mut body = vec![0; len as usize];
        self.r
            .read_exact(&mut body)
            .map_err(|_| Error::Format(format!("{name}: truncated")))?;
        let crc = u32::from_le_bytes(read_exact(&mut self.r)?);
        if crc32(0, &body) != crc {
            return format_err(format!("{name}: CRC mismatch (corrupted)"));
        }
        if name == "end" {
            let c = Chunk {
                name,
                version,
                body,
            };
            let mut d = c.decoder(1)?;
            let n = d.u64()?;
            d.finish()?;
            if n != self.chunks {
                return format_err(format!("end: {n} chunks recorded, {} read", self.chunks));
            }
            return Ok(None);
        }
        self.chunks += 1;
        Ok(Some(Chunk {
            name,
            version,
            body,
        }))
    }

    /// 次のチャンクが name であることを確かめて読む。
    pub fn expect(&mut self, name: &str) -> Result<Chunk, Error> {
        match self.next_chunk()? {
            Some(c) if c.name == name => Ok(c),
            Some(c) => format_err(format!("expected chunk {name}, found {}", c.name)),
            None => format_err(format!("expected chunk {name}, found end")),
        }
    }

    /// 終端であることを確かめる。
    pub fn expect_end(&mut self) -> Result<(), Error> {
        match self.next_chunk()? {
            None => Ok(()),
            Some(c) => format_err(format!("unexpected chunk {}", c.name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_value() {
        // "123456789" の CRC-32（IEEE）は 0xCBF43926（よく知られた検査値）。
        assert_eq!(crc32(0, b"123456789"), 0xCBF43926);
        assert_eq!(
            crc32(crc32(0, b"1234"), b"56789"),
            0xCBF43926,
            "続きから計算できる"
        );
    }

    fn sample() -> Vec<u8> {
        let mut w = Writer::new(Vec::new(), "m", "id").unwrap();
        w.chunk("a", 2, |e| {
            e.u32(7);
            e.bytes(b"xyz");
            e.bool(true);
        })
        .unwrap();
        w.raw_chunk("b", 1, &[1, 2, 3]).unwrap();
        w.finish().unwrap()
    }

    #[test]
    fn round_trip() {
        let buf = sample();
        let mut r = Reader::new(&buf[..]).unwrap();
        assert_eq!(
            r.header,
            Header {
                machine: "m".into(),
                image_id: "id".into()
            }
        );
        let a = r.expect("a").unwrap();
        assert!(a.decoder(1).is_err(), "版数の食い違いはエラー");
        let mut d = a.decoder(2).unwrap();
        assert_eq!(d.u32().unwrap(), 7);
        assert_eq!(d.bytes().unwrap(), b"xyz");
        assert!(d.bool().unwrap());
        d.finish().unwrap();
        assert_eq!(r.expect("b").unwrap().body, [1, 2, 3]);
        r.expect_end().unwrap();
    }

    /// 壊れた・切り詰めたファイルは panic せずエラーになる（簡易ファジング）。
    #[test]
    fn corrupted_input_is_error_not_panic() {
        let good = sample();
        let parse = |b: &[u8]| -> Result<(), Error> {
            let mut r = Reader::new(b)?;
            while let Some(c) = r.next_chunk()? {
                let mut d = c.decoder(c.version)?;
                while d.u8().is_ok() {}
            }
            Ok(())
        };
        parse(&good).unwrap();
        for n in 0..good.len() {
            assert!(parse(&good[..n]).is_err(), "truncated at {n}");
        }
        for i in 0..good.len() {
            for bit in 0..8 {
                let mut b = good.clone();
                b[i] ^= 1 << bit;
                let _ = parse(&b); // どのビットを反転しても panic しないこと
            }
        }
        // 巨大な長さは確保せずにエラー
        let mut b = good.clone();
        let pos = 8 + 4 + 3 + 4 + 3 + 2; // 最初のチャンクの長さの位置
        b[pos..pos + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(parse(&b).is_err());
    }
}
