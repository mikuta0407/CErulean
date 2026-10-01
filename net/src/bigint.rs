//! 多倍長の非負整数と RSA（ゲストの TLS を終端するための鍵と署名・復号）。
//!
//! 一次資料: RFC 8017（PKCS #1 v2.2: RSA の鍵・RSAES-PKCS1-v1_5・RSASSA-PKCS1-v1_5・CRT）。
//! 算法（Montgomery 乗算・Knuth の除算・Miller-Rabin）は教科書どおり。
//! 用途はエミュレータの中の TLS の中継（ゲストとの間だけ）なので、定数時間の実装にはしない。

use std::cmp::Ordering;

use crate::crypto::Drbg;

/// 32 ビットの語の列（下位の語が先）。末尾の 0 の語は持たない。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Big(Vec<u32>);

impl Big {
    pub fn zero() -> Big {
        Big(Vec::new())
    }

    pub fn from_u32(v: u32) -> Big {
        let mut b = Big(vec![v]);
        b.trim();
        b
    }

    fn trim(&mut self) {
        while self.0.last() == Some(&0) {
            self.0.pop();
        }
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_empty()
    }

    pub fn is_odd(&self) -> bool {
        self.0.first().is_some_and(|w| w & 1 == 1)
    }

    pub fn from_be(b: &[u8]) -> Big {
        let mut v = Vec::with_capacity(b.len().div_ceil(4));
        let mut i = b.len();
        while i > 0 {
            let lo = i.saturating_sub(4);
            let mut w = 0u32;
            for &x in &b[lo..i] {
                w = w << 8 | x as u32;
            }
            v.push(w);
            i = lo;
        }
        let mut r = Big(v);
        r.trim();
        r
    }

    /// len バイトの big-endian（足りない上位は 0。入らなければ下位 len バイト）。
    pub fn to_be(&self, len: usize) -> Vec<u8> {
        let mut out = vec![0u8; len];
        for i in 0..len {
            let w = i / 4;
            let byte = self.0.get(w).map_or(0, |x| (x >> (8 * (i % 4))) as u8);
            out[len - 1 - i] = byte;
        }
        out
    }

    /// 最小の長さの big-endian（0 なら空）。
    pub fn to_be_min(&self) -> Vec<u8> {
        let n = self.bits().div_ceil(8);
        self.to_be(n)
    }

    pub fn bits(&self) -> usize {
        match self.0.last() {
            None => 0,
            Some(&top) => (self.0.len() - 1) * 32 + (32 - top.leading_zeros() as usize),
        }
    }

    fn bit(&self, i: usize) -> bool {
        self.0.get(i / 32).is_some_and(|w| w >> (i % 32) & 1 == 1)
    }

    pub fn add(&self, o: &Big) -> Big {
        let n = self.0.len().max(o.0.len());
        let mut v = Vec::with_capacity(n + 1);
        let mut c = 0u64;
        for i in 0..n {
            let s = *self.0.get(i).unwrap_or(&0) as u64 + *o.0.get(i).unwrap_or(&0) as u64 + c;
            v.push(s as u32);
            c = s >> 32;
        }
        v.push(c as u32);
        let mut r = Big(v);
        r.trim();
        r
    }

    /// self − o（self ≥ o であること）。
    pub fn sub(&self, o: &Big) -> Big {
        let mut v = Vec::with_capacity(self.0.len());
        let mut borrow = 0i64;
        for i in 0..self.0.len() {
            let d = self.0[i] as i64 - *o.0.get(i).unwrap_or(&0) as i64 - borrow;
            v.push(d as u32);
            borrow = (d < 0) as i64;
        }
        let mut r = Big(v);
        r.trim();
        r
    }

    pub fn mul(&self, o: &Big) -> Big {
        if self.is_zero() || o.is_zero() {
            return Big::zero();
        }
        let mut v = vec![0u32; self.0.len() + o.0.len()];
        for (i, &a) in self.0.iter().enumerate() {
            let mut c = 0u64;
            for (j, &b) in o.0.iter().enumerate() {
                let t = v[i + j] as u64 + a as u64 * b as u64 + c;
                v[i + j] = t as u32;
                c = t >> 32;
            }
            v[i + o.0.len()] = c as u32;
        }
        let mut r = Big(v);
        r.trim();
        r
    }

    fn shl_bits(&self, s: u32) -> Vec<u32> {
        // s < 32。語を 1 つ足して返す
        let mut v = Vec::with_capacity(self.0.len() + 1);
        let mut carry = 0u32;
        for &w in &self.0 {
            if s == 0 {
                v.push(w);
            } else {
                v.push(w << s | carry);
                carry = w >> (32 - s);
            }
        }
        v.push(carry);
        v
    }

    /// (商, 余り)。d は 0 でないこと（Knuth の算法 D）。
    pub fn divrem(&self, d: &Big) -> (Big, Big) {
        assert!(!d.is_zero(), "division by zero");
        if self.cmp(d) == Ordering::Less {
            return (Big::zero(), self.clone());
        }
        if d.0.len() == 1 {
            let dv = d.0[0] as u64;
            let mut q = vec![0u32; self.0.len()];
            let mut r = 0u64;
            for i in (0..self.0.len()).rev() {
                let cur = r << 32 | self.0[i] as u64;
                q[i] = (cur / dv) as u32;
                r = cur % dv;
            }
            let mut q = Big(q);
            q.trim();
            return (q, Big::from_u32(r as u32));
        }
        let s = d.0.last().copied().unwrap_or(1).leading_zeros();
        let dn: Vec<u32> = {
            let mut t = d.shl_bits(s);
            t.pop();
            t
        };
        let mut un = self.shl_bits(s);
        let n = dn.len();
        let m = un.len() - 1 - n;
        let mut q = vec![0u32; m + 1];
        let b = 1u64 << 32;
        for j in (0..=m).rev() {
            let num = (un[j + n] as u64) << 32 | un[j + n - 1] as u64;
            let mut qhat = num / dn[n - 1] as u64;
            let mut rhat = num % dn[n - 1] as u64;
            while qhat >= b || qhat * dn[n - 2] as u64 > (rhat << 32 | un[j + n - 2] as u64) {
                qhat -= 1;
                rhat += dn[n - 1] as u64;
                if rhat >= b {
                    break;
                }
            }
            // un[j..j+n+1] -= qhat * dn
            let mut borrow = 0i64;
            let mut carry = 0u64;
            for i in 0..n {
                let p = qhat * dn[i] as u64 + carry;
                carry = p >> 32;
                let t = un[i + j] as i64 - borrow - (p & 0xFFFF_FFFF) as i64;
                un[i + j] = t as u32;
                borrow = if t < 0 { 1 } else { 0 };
            }
            let t = un[j + n] as i64 - borrow - carry as i64;
            un[j + n] = t as u32;
            if t < 0 {
                // 引きすぎた: 1 回足し戻す
                qhat -= 1;
                let mut c = 0u64;
                for i in 0..n {
                    let s2 = un[i + j] as u64 + dn[i] as u64 + c;
                    un[i + j] = s2 as u32;
                    c = s2 >> 32;
                }
                un[j + n] = un[j + n].wrapping_add(c as u32);
            }
            q[j] = qhat as u32;
        }
        // 余りを戻す
        let mut r = vec![0u32; n];
        for i in 0..n {
            r[i] = if s == 0 {
                un[i]
            } else {
                un[i] >> s | un[i + 1] << (32 - s)
            };
        }
        let (mut q, mut r) = (Big(q), Big(r));
        q.trim();
        r.trim();
        (q, r)
    }

    pub fn rem(&self, d: &Big) -> Big {
        self.divrem(d).1
    }

    /// self^e mod m（m は奇数）。Montgomery 乗算で計算する。
    pub fn modpow(&self, e: &Big, m: &Big) -> Big {
        let mont = Mont::new(m);
        let base = mont.enter(&self.rem(m));
        let mut acc = mont.enter(&Big::from_u32(1));
        for i in (0..e.bits()).rev() {
            acc = mont.mul(&acc, &acc);
            if e.bit(i) {
                acc = mont.mul(&acc, &base);
            }
        }
        mont.leave(&acc)
    }

    /// self^-1 mod m（互いに素でなければ None）。拡張ユークリッド。
    pub fn modinv(&self, m: &Big) -> Option<Big> {
        // 係数は m を法として非負に保つ（t の符号を別に持つ）
        let (mut r0, mut r1) = (m.clone(), self.rem(m));
        let (mut t0, mut t1) = (Big::zero(), Big::from_u32(1));
        let (mut s0, mut s1) = (false, false); // 負か
        while !r1.is_zero() {
            let (q, r) = r0.divrem(&r1);
            // t2 = t0 - q*t1
            let qt = q.mul(&t1);
            let (t2, s2) = signed_sub(&t0, s0, &qt, s1);
            r0 = r1;
            r1 = r;
            t0 = t1;
            s0 = s1;
            t1 = t2;
            s1 = s2;
        }
        if r0 != Big::from_u32(1) {
            return None;
        }
        Some(if s0 {
            m.sub(&t0.rem(m)).rem(m)
        } else {
            t0.rem(m)
        })
    }
}

impl Ord for Big {
    fn cmp(&self, o: &Big) -> Ordering {
        self.0
            .len()
            .cmp(&o.0.len())
            .then_with(|| self.0.iter().rev().cmp(o.0.iter().rev()))
    }
}

impl PartialOrd for Big {
    fn partial_cmp(&self, o: &Big) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// (a, 負か) − (b, 負か)。
fn signed_sub(a: &Big, sa: bool, b: &Big, sb: bool) -> (Big, bool) {
    if sa != sb {
        // a − (−b) = a + b（符号は a）
        return (a.add(b), sa);
    }
    match a.cmp(b) {
        Ordering::Less => (b.sub(a), !sa),
        _ => (a.sub(b), sa),
    }
}

/// Montgomery 表現（R = 2^(32n)）。
struct Mont {
    m: Vec<u32>,
    minv: u32, // −m^-1 mod 2^32
    r2: Big,   // R^2 mod m
    mb: Big,
}

impl Mont {
    fn new(m: &Big) -> Mont {
        let m0 = m.0[0];
        // ニュートン法で m0^-1 mod 2^32
        let mut inv = 1u32;
        for _ in 0..5 {
            inv = inv.wrapping_mul(2u32.wrapping_sub(m0.wrapping_mul(inv)));
        }
        let n = m.0.len();
        let mut r2 = vec![0u32; 2 * n + 1];
        r2[2 * n] = 1;
        let mut r2 = Big(r2);
        r2.trim();
        Mont {
            m: m.0.clone(),
            minv: inv.wrapping_neg(),
            r2: r2.rem(m),
            mb: m.clone(),
        }
    }

    #[allow(clippy::needless_range_loop)]
    fn mul(&self, a: &Big, b: &Big) -> Big {
        let n = self.m.len();
        let mut t = vec![0u32; n + 2];
        for i in 0..n {
            let ai = *a.0.get(i).unwrap_or(&0) as u64;
            let mut c = 0u64;
            for j in 0..n {
                let s = t[j] as u64 + ai * *b.0.get(j).unwrap_or(&0) as u64 + c;
                t[j] = s as u32;
                c = s >> 32;
            }
            let s = t[n] as u64 + c;
            t[n] = s as u32;
            t[n + 1] = (s >> 32) as u32;
            let u = t[0].wrapping_mul(self.minv) as u64;
            let mut c = (t[0] as u64 + u * self.m[0] as u64) >> 32;
            for j in 1..n {
                let s = t[j] as u64 + u * self.m[j] as u64 + c;
                t[j - 1] = s as u32;
                c = s >> 32;
            }
            let s = t[n] as u64 + c;
            t[n - 1] = s as u32;
            t[n] = t[n + 1] + (s >> 32) as u32;
            t[n + 1] = 0;
        }
        t.truncate(n + 1);
        let mut r = Big(t);
        r.trim();
        if r.cmp(&self.mb) != Ordering::Less {
            r = r.sub(&self.mb);
        }
        r
    }

    fn enter(&self, a: &Big) -> Big {
        self.mul(a, &self.r2)
    }

    fn leave(&self, a: &Big) -> Big {
        self.mul(a, &Big::from_u32(1))
    }
}

// ---- 素数・RSA ----

const SMALL_PRIMES: [u32; 53] = [
    3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37, 41, 43, 47, 53, 59, 61, 67, 71, 73, 79, 83, 89, 97,
    101, 103, 107, 109, 113, 127, 131, 137, 139, 149, 151, 157, 163, 167, 173, 179, 181, 191, 193,
    197, 199, 211, 223, 227, 229, 233, 239, 241, 251,
];

fn random_below(rng: &mut Drbg, n: &Big) -> Big {
    let bytes = n.bits().div_ceil(8);
    loop {
        let mut b = rng.bytes(bytes);
        let extra = bytes * 8 - n.bits();
        b[0] &= 0xFFu8 >> extra;
        let v = Big::from_be(&b);
        if v.cmp(n) == Ordering::Less && v.bits() > 1 {
            return v;
        }
    }
}

/// Miller-Rabin（rounds 回）。
fn probably_prime(n: &Big, rng: &mut Drbg, rounds: u32) -> bool {
    for &p in &SMALL_PRIMES {
        if n.rem(&Big::from_u32(p)).is_zero() {
            return n == &Big::from_u32(p);
        }
    }
    let one = Big::from_u32(1);
    let nm1 = n.sub(&one);
    let mut s = 0;
    while !nm1.bit(s) {
        s += 1;
    }
    let d = shr(&nm1, s);
    'outer: for _ in 0..rounds {
        let a = random_below(rng, &nm1);
        let mut x = a.modpow(&d, n);
        if x == one || x == nm1 {
            continue;
        }
        for _ in 1..s {
            x = x.mul(&x).rem(n);
            if x == nm1 {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

fn shr(a: &Big, s: usize) -> Big {
    let (w, b) = (s / 32, (s % 32) as u32);
    let mut v = Vec::with_capacity(a.0.len());
    for i in w..a.0.len() {
        let lo = a.0[i] >> b;
        let hi = if b == 0 {
            0
        } else {
            a.0.get(i + 1).map_or(0, |x| x << (32 - b))
        };
        v.push(lo | hi);
    }
    let mut r = Big(v);
    r.trim();
    r
}

/// bits ビットの素数（上位 2 ビットを立てる。n = pq がちょうど 2×bits ビットになるように）。
fn random_prime(rng: &mut Drbg, bits: usize, e: &Big) -> Big {
    loop {
        let mut b = rng.bytes(bits / 8);
        b[0] |= 0xC0;
        let last = b.len() - 1;
        b[last] |= 1;
        let p = Big::from_be(&b);
        let pm1 = p.sub(&Big::from_u32(1));
        if pm1.rem(e).is_zero() {
            continue; // gcd(e, p−1) ≠ 1（e は素数 65537）
        }
        if probably_prime(&p, rng, 20) {
            return p;
        }
    }
}

/// RSA の秘密鍵（CRT の値つき。RFC 8017 3.2）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RsaKey {
    pub n: Big,
    pub e: Big,
    pub d: Big,
    pub p: Big,
    pub q: Big,
    pub dp: Big,
    pub dq: Big,
    pub qinv: Big,
}

impl RsaKey {
    /// bits ビット（2 の倍数・256 以上）の鍵を作る。e = 65537。
    pub fn generate(rng: &mut Drbg, bits: usize) -> RsaKey {
        let e = Big::from_u32(65537);
        loop {
            let p = random_prime(rng, bits / 2, &e);
            let q = random_prime(rng, bits / 2, &e);
            if p == q {
                continue;
            }
            let (p, q) = if p.cmp(&q) == Ordering::Greater {
                (p, q)
            } else {
                (q, p)
            };
            let n = p.mul(&q);
            if n.bits() != bits {
                continue;
            }
            let one = Big::from_u32(1);
            let (pm1, qm1) = (p.sub(&one), q.sub(&one));
            let phi = pm1.mul(&qm1);
            let Some(d) = e.modinv(&phi) else { continue };
            let Some(qinv) = q.modinv(&p) else { continue };
            return RsaKey {
                dp: d.rem(&pm1),
                dq: d.rem(&qm1),
                n,
                e,
                d,
                p,
                q,
                qinv,
            };
        }
    }

    pub fn size(&self) -> usize {
        self.n.bits().div_ceil(8)
    }

    /// 秘密鍵の演算 c^d mod n（CRT。RFC 8017 5.1.2）。
    fn private(&self, c: &Big) -> Big {
        let m1 = c.modpow(&self.dp, &self.p);
        let m2 = c.modpow(&self.dq, &self.q);
        let m2p = m2.rem(&self.p);
        let diff = if m1.cmp(&m2p) == Ordering::Less {
            m1.add(&self.p).sub(&m2p)
        } else {
            m1.sub(&m2p)
        };
        let h = self.qinv.mul(&diff).rem(&self.p);
        m2.add(&h.mul(&self.q))
    }

    /// RSASSA-PKCS1-v1_5 の署名（digest_info は DigestInfo の DER。RFC 8017 8.2・9.2）。
    pub fn sign_pkcs1(&self, digest_info: &[u8]) -> Vec<u8> {
        let k = self.size();
        let mut em = vec![0u8; k];
        em[1] = 1;
        let ps_end = k - digest_info.len() - 1;
        for b in &mut em[2..ps_end] {
            *b = 0xFF;
        }
        em[k - digest_info.len()..].copy_from_slice(digest_info);
        self.private(&Big::from_be(&em)).to_be(k)
    }

    /// RSAES-PKCS1-v1_5 の復号（RFC 8017 7.2.2）。形が違えば None。
    pub fn decrypt_pkcs1(&self, c: &[u8]) -> Option<Vec<u8>> {
        let k = self.size();
        if c.len() != k {
            return None;
        }
        let cb = Big::from_be(c);
        if cb.cmp(&self.n) != Ordering::Less {
            return None;
        }
        let em = self.private(&cb).to_be(k);
        if em[0] != 0 || em[1] != 2 {
            return None;
        }
        let sep = em[2..].iter().position(|&b| b == 0)? + 2;
        if sep < 10 {
            return None; // PS は 8 バイト以上
        }
        Some(em[sep + 1..].to_vec())
    }

    /// 公開鍵の演算（試験用）: m^e mod n。
    pub fn public(&self, m: &[u8]) -> Vec<u8> {
        Big::from_be(m).modpow(&self.e, &self.n).to_be(self.size())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big(s: &str) -> Big {
        // 16 進
        let s = if s.len() % 2 == 1 {
            format!("0{s}")
        } else {
            s.to_string()
        };
        let b: Vec<u8> = (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect();
        Big::from_be(&b)
    }

    #[test]
    fn arithmetic() {
        let a = big("123456789abcdef0fedcba9876543210");
        let b = big("fedcba98765432100123456789");
        let (q, r) = a.divrem(&b);
        assert_eq!(q.mul(&b).add(&r), a);
        assert_eq!(r.cmp(&b), Ordering::Less);
        let p = big("ffffffffffffffffffffffffffffff61"); // 2^128 − 159（素数）
        let x = big("1234567");
        // フェルマーの小定理
        assert_eq!(x.modpow(&p.sub(&Big::from_u32(1)), &p), Big::from_u32(1));
        let inv = x.modinv(&p).unwrap();
        assert_eq!(inv.mul(&x).rem(&p), Big::from_u32(1));
        assert_eq!(Big::from_be(&a.to_be(16)), a);
    }

    #[test]
    fn rsa_roundtrip() {
        let mut rng = Drbg::new(b"rsa test seed");
        let k = RsaKey::generate(&mut rng, 512);
        assert_eq!(k.n.bits(), 512);
        // 暗号化（公開鍵）→ 復号
        let msg = b"premaster secret 48 bytes..........";
        let mut em = vec![0u8, 2];
        em.extend(std::iter::repeat_n(0x5Au8, k.size() - 3 - msg.len()));
        em.push(0);
        em.extend_from_slice(msg);
        let c = k.public(&em);
        assert_eq!(k.decrypt_pkcs1(&c).unwrap(), msg);
        // 署名を公開鍵で戻すと元の形
        let s = k.sign_pkcs1(b"digest");
        let back = k.public(&s);
        assert_eq!(&back[back.len() - 6..], b"digest");
        assert_eq!(&back[..2], &[0, 1]);
    }
}
