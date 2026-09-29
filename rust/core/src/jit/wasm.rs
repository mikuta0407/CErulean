//! 小さな wasm のエンコーダ（段階5。自作。2026-09-29 ユーザー確認済み）。
//!
//! JIT が生成するモジュールに要る範囲だけを持つ（根拠は WebAssembly Core
//! Specification のバイナリ形式）。モジュールの形は 1 種類に固定する:
//!   - 型: `(i32) -> i32` の 1 つだけ（生成関数はすべて `(ctx) -> 実行結果`）
//!   - import: 本体（コア）の線形メモリ `e.m`
//!   - 関数: n 個。export 名は "0".."n-1"（ホストがこの順で番号を振る）
//!
//! 命令は MVP ＋ 符号拡張だけを使う（段階5 の設計案 §7。Safari の古い版を切らない）。

/// 生成中の 1 関数（ローカル変数は i32 だけ。0 番は引数 ctx）。
pub struct Func {
    /// 引数を除く i32 のローカル変数の数
    locals: u32,
    code: Vec<u8>,
}

/// ローカル変数の番号。
pub type Local = u32;

/// ブロック型（値を持たない block/loop/if）。
const BLOCK_EMPTY: u8 = 0x40;

impl Func {
    /// locals 個の i32 ローカル（番号は引数の後から。生成関数なら 1..=locals）を持つ関数。
    pub fn new(locals: u32) -> Func {
        Func {
            locals,
            code: Vec::with_capacity(256),
        }
    }

    /// 命令列のバイト数。
    pub fn len(&self) -> usize {
        self.code.len()
    }

    fn op(&mut self, b: u8) -> &mut Self {
        self.code.push(b);
        self
    }

    fn memarg(&mut self, align: u32, offset: u32) -> &mut Self {
        uleb(&mut self.code, align);
        uleb(&mut self.code, offset);
        self
    }

    // ---- 制御 ----
    pub fn block(&mut self) -> &mut Self {
        self.op(0x02).op(BLOCK_EMPTY)
    }
    pub fn loop_(&mut self) -> &mut Self {
        self.op(0x03).op(BLOCK_EMPTY)
    }
    /// depth 番目（0 が最も内側）の block の終わり・loop の始めへ分岐する。
    pub fn br(&mut self, depth: u32) -> &mut Self {
        self.op(0x0C);
        uleb(&mut self.code, depth);
        self
    }
    pub fn br_if(&mut self, depth: u32) -> &mut Self {
        self.op(0x0D);
        uleb(&mut self.code, depth);
        self
    }
    /// スタックの値 i で targets[i]（範囲外なら default）の深さへ分岐する。
    pub fn br_table(&mut self, targets: &[u32], default: u32) -> &mut Self {
        self.op(0x0E);
        uleb(&mut self.code, targets.len() as u32);
        for &t in targets {
            uleb(&mut self.code, t);
        }
        uleb(&mut self.code, default);
        self
    }
    pub fn if_(&mut self) -> &mut Self {
        self.op(0x04).op(BLOCK_EMPTY)
    }
    pub fn else_(&mut self) -> &mut Self {
        self.op(0x05)
    }
    pub fn end(&mut self) -> &mut Self {
        self.op(0x0B)
    }
    pub fn ret(&mut self) -> &mut Self {
        self.op(0x0F)
    }
    pub fn select(&mut self) -> &mut Self {
        self.op(0x1B)
    }

    /// 関数番号 func（モジュールの中の番号。補助関数は 0 から）を呼ぶ。
    pub fn call(&mut self, func: u32) -> &mut Self {
        self.op(0x10);
        uleb(&mut self.code, func);
        self
    }

    // ---- 変数 ----
    pub fn get(&mut self, l: Local) -> &mut Self {
        self.op(0x20);
        uleb(&mut self.code, l);
        self
    }
    pub fn set(&mut self, l: Local) -> &mut Self {
        self.op(0x21);
        uleb(&mut self.code, l);
        self
    }
    pub fn tee(&mut self, l: Local) -> &mut Self {
        self.op(0x22);
        uleb(&mut self.code, l);
        self
    }

    // ---- メモリ（offset は即値のオフセット。align は 2 の対数の目安）----
    pub fn load(&mut self, offset: u32) -> &mut Self {
        self.op(0x28).memarg(2, offset)
    }
    pub fn i64_load(&mut self, offset: u32) -> &mut Self {
        self.op(0x29).memarg(3, offset)
    }
    pub fn load8_u(&mut self, offset: u32) -> &mut Self {
        self.op(0x2D).memarg(0, offset)
    }
    pub fn load8_s(&mut self, offset: u32) -> &mut Self {
        self.op(0x2C).memarg(0, offset)
    }
    pub fn load16_s(&mut self, offset: u32) -> &mut Self {
        self.op(0x2E).memarg(1, offset)
    }
    pub fn load16_u(&mut self, offset: u32) -> &mut Self {
        self.op(0x2F).memarg(1, offset)
    }
    pub fn store16(&mut self, offset: u32) -> &mut Self {
        self.op(0x3B).memarg(1, offset)
    }
    pub fn store(&mut self, offset: u32) -> &mut Self {
        self.op(0x36).memarg(2, offset)
    }
    pub fn store8(&mut self, offset: u32) -> &mut Self {
        self.op(0x3A).memarg(0, offset)
    }

    // ---- 整数 ----
    pub fn i64(&mut self, v: i64) -> &mut Self {
        self.op(0x42);
        sleb64(&mut self.code, v);
        self
    }
    pub fn i64_extend_u(&mut self) -> &mut Self {
        self.op(0xAD)
    }
    pub fn i64_extend_s(&mut self) -> &mut Self {
        self.op(0xAC)
    }
    pub fn i64_eq(&mut self) -> &mut Self {
        self.op(0x51)
    }
    pub fn i64_add(&mut self) -> &mut Self {
        self.op(0x7C)
    }
    pub fn i64_mul(&mut self) -> &mut Self {
        self.op(0x7E)
    }
    pub fn i64_or(&mut self) -> &mut Self {
        self.op(0x84)
    }
    pub fn i64_shl(&mut self) -> &mut Self {
        self.op(0x86)
    }
    pub fn i64_shr_u(&mut self) -> &mut Self {
        self.op(0x88)
    }
    pub fn wrap(&mut self) -> &mut Self {
        self.op(0xA7)
    }
    pub fn i32(&mut self, v: u32) -> &mut Self {
        self.op(0x41);
        sleb(&mut self.code, v as i32);
        self
    }
    pub fn eqz(&mut self) -> &mut Self {
        self.op(0x45)
    }
    pub fn eq(&mut self) -> &mut Self {
        self.op(0x46)
    }
    pub fn ne(&mut self) -> &mut Self {
        self.op(0x47)
    }
    pub fn lt_u(&mut self) -> &mut Self {
        self.op(0x49)
    }
    pub fn gt_u(&mut self) -> &mut Self {
        self.op(0x4B)
    }
    pub fn ge_u(&mut self) -> &mut Self {
        self.op(0x4F)
    }
    pub fn add(&mut self) -> &mut Self {
        self.op(0x6A)
    }
    pub fn sub(&mut self) -> &mut Self {
        self.op(0x6B)
    }
    pub fn mul(&mut self) -> &mut Self {
        self.op(0x6C)
    }
    pub fn and(&mut self) -> &mut Self {
        self.op(0x71)
    }
    pub fn or(&mut self) -> &mut Self {
        self.op(0x72)
    }
    pub fn xor(&mut self) -> &mut Self {
        self.op(0x73)
    }
    pub fn shl(&mut self) -> &mut Self {
        self.op(0x74)
    }
    pub fn shr_s(&mut self) -> &mut Self {
        self.op(0x75)
    }
    pub fn shr_u(&mut self) -> &mut Self {
        self.op(0x76)
    }
    pub fn rotr(&mut self) -> &mut Self {
        self.op(0x78)
    }

    /// 関数本体のバイト列（ローカル宣言＋命令列＋end）。
    fn body(&self) -> Vec<u8> {
        let mut b = vec![];
        if self.locals == 0 {
            uleb(&mut b, 0);
        } else {
            uleb(&mut b, 1); // 宣言 1 組: locals 個の i32
            uleb(&mut b, self.locals);
            b.push(I32);
        }
        b.extend_from_slice(&self.code);
        b.push(0x0B);
        b
    }
}

const I32: u8 = 0x7F;

/// 補助関数（(引数の数, 関数)。生成関数から call で呼ぶ）と生成関数の列から
/// モジュールのバイト列を作る。
pub fn module(helpers: &[(u32, Func)], funcs: &[Func]) -> Vec<u8> {
    let (h, n) = (helpers.len() as u32, funcs.len() as u32);
    let mut m = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];

    // type: 0 = (i32) -> i32、1+i = 補助関数 i の型
    section(&mut m, 1, |s| {
        uleb(s, 1 + h);
        s.extend_from_slice(&[0x60, 1, I32, 1, I32]);
        for (params, _) in helpers {
            s.push(0x60);
            uleb(s, *params);
            s.extend(std::iter::repeat_n(I32, *params as usize));
            s.extend_from_slice(&[1, I32]);
        }
    });
    // import: e.m（メモリ、最小 0 ページ・最大なし。本体のメモリの大きさに依らず合う）
    section(&mut m, 2, |s| {
        uleb(s, 1);
        name(s, "e");
        name(s, "m");
        s.extend_from_slice(&[0x02, 0x00, 0x00]);
    });
    // function: 補助関数、生成関数（型 0）の順
    section(&mut m, 3, |s| {
        uleb(s, h + n);
        for i in 0..h {
            uleb(s, 1 + i);
        }
        for _ in 0..n {
            uleb(s, 0);
        }
    });
    // export: 生成関数を "0".."n-1"
    section(&mut m, 7, |s| {
        uleb(s, n);
        for i in 0..n {
            name(s, &i.to_string());
            s.push(0x00);
            uleb(s, h + i);
        }
    });
    // code
    section(&mut m, 10, |s| {
        uleb(s, h + n);
        for f in helpers.iter().map(|(_, f)| f).chain(funcs) {
            let b = f.body();
            uleb(s, b.len() as u32);
            s.extend_from_slice(&b);
        }
    });
    m
}

fn section(m: &mut Vec<u8>, id: u8, f: impl FnOnce(&mut Vec<u8>)) {
    let mut s = vec![];
    f(&mut s);
    m.push(id);
    uleb(m, s.len() as u32);
    m.extend_from_slice(&s);
}

fn name(s: &mut Vec<u8>, n: &str) {
    uleb(s, n.len() as u32);
    s.extend_from_slice(n.as_bytes());
}

/// 符号なし LEB128。
pub(crate) fn uleb(b: &mut Vec<u8>, mut v: u32) {
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            b.push(byte);
            return;
        }
        b.push(byte | 0x80);
    }
}

/// 符号つき LEB128（i64.const の即値）。
pub(crate) fn sleb64(b: &mut Vec<u8>, mut v: i64) {
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        let done = (v == 0 && byte & 0x40 == 0) || (v == -1 && byte & 0x40 != 0);
        if done {
            b.push(byte);
            return;
        }
        b.push(byte | 0x80);
    }
}

/// 符号つき LEB128（i32.const の即値）。
pub(crate) fn sleb(b: &mut Vec<u8>, mut v: i32) {
    loop {
        let byte = (v & 0x7F) as u8;
        v >>= 7; // 算術シフト
        let done = (v == 0 && byte & 0x40 == 0) || (v == -1 && byte & 0x40 != 0);
        if done {
            b.push(byte);
            return;
        }
        b.push(byte | 0x80);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leb128() {
        let enc = |v: u32| {
            let mut b = vec![];
            uleb(&mut b, v);
            b
        };
        assert_eq!(enc(0), [0]);
        assert_eq!(enc(127), [0x7F]);
        assert_eq!(enc(128), [0x80, 0x01]);
        assert_eq!(enc(624485), [0xE5, 0x8E, 0x26]);
        assert_eq!(enc(u32::MAX), [0xFF, 0xFF, 0xFF, 0xFF, 0x0F]);
        let senc = |v: i32| {
            let mut b = vec![];
            sleb(&mut b, v);
            b
        };
        assert_eq!(senc(0), [0]);
        assert_eq!(senc(63), [0x3F]);
        assert_eq!(senc(64), [0xC0, 0x00]);
        assert_eq!(senc(-1), [0x7F]);
        assert_eq!(senc(-64), [0x40]);
        assert_eq!(senc(-65), [0xBF, 0x7F]);
        assert_eq!(senc(-123456), [0xC0, 0xBB, 0x78]);
        assert_eq!(senc(i32::MIN), [0x80, 0x80, 0x80, 0x80, 0x78]);
        assert_eq!(senc(i32::MAX), [0xFF, 0xFF, 0xFF, 0xFF, 0x07]);
    }

    /// 試作（tmp/proto-jit）で Node が受け付けたモジュールと同じ形になること。
    #[test]
    fn module_layout() {
        let mut f = Func::new(0);
        f.get(0).get(0).load(0).i32(1).add().store(0).i32(0);
        let m = module(&[], &[f]);
        let want: &[u8] = &[
            0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00, // magic, version
            0x01, 0x06, 0x01, 0x60, 0x01, 0x7F, 0x01, 0x7F, // type
            0x02, 0x08, 0x01, 0x01, b'e', 0x01, b'm', 0x02, 0x00, 0x00, // import
            0x03, 0x02, 0x01, 0x00, // function
            0x07, 0x05, 0x01, 0x01, b'0', 0x00, 0x00, // export
            0x0A, 0x13, 0x01, 0x11, 0x00, 0x20, 0x00, 0x20, 0x00, 0x28, 0x02, 0x00, 0x41, 0x01,
            0x6A, 0x36, 0x02, 0x00, 0x41, 0x00, 0x0B, // code
        ];
        assert_eq!(m, want);
    }
}
