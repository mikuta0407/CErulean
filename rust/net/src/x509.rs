//! 証明書（X.509 の DER）: ゲストに入れてもらう自前の CA と、接続先の名前ごとの
//! サーバー証明書を作る。
//!
//! 一次資料: RFC 5280（証明書と拡張）、X.690（DER）、RFC 8017（鍵の形と署名）、
//! RFC 3279（sha1WithRSAEncryption・rsaEncryption の OID）。
//! 署名は SHA-1（WM5 の CryptoAPI が SHA-2 の署名を扱えるか分からないため。この CA は
//! エミュレータの中の中継にだけ使う）。有効期間はゲストの時計に依らないよう 2000〜2049 年。

use crate::bigint::{Big, RsaKey};
use crate::crypto::{Drbg, sha1};

/// CA の名前（証明書の発行者・主体）。
pub const CA_NAME: &str = "CErulean Local CA";

// ---- DER（X.690）----

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let n = content.len();
    if n < 0x80 {
        v.push(n as u8);
    } else {
        let lb: Vec<u8> = (n as u32)
            .to_be_bytes()
            .into_iter()
            .skip_while(|&b| b == 0)
            .collect();
        v.push(0x80 | lb.len() as u8);
        v.extend_from_slice(&lb);
    }
    v.extend_from_slice(content);
    v
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

/// INTEGER（非負。最上位ビットが立てば 0 を前に置く）。
fn int(b: &[u8]) -> Vec<u8> {
    let mut v: Vec<u8> = b.iter().copied().skip_while(|&x| x == 0).collect();
    if v.is_empty() || v[0] & 0x80 != 0 {
        v.insert(0, 0);
    }
    tlv(0x02, &v)
}

fn oid(bytes: &[u8]) -> Vec<u8> {
    tlv(0x06, bytes)
}

fn bit_string(b: &[u8]) -> Vec<u8> {
    let mut v = vec![0u8];
    v.extend_from_slice(b);
    tlv(0x03, &v)
}

// OID（DER の中身）
const OID_SHA1_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x05];
const OID_RSA: &[u8] = &[0x2A, 0x86, 0x48, 0x86, 0xF7, 0x0D, 0x01, 0x01, 0x01];
const OID_CN: &[u8] = &[0x55, 0x04, 0x03];
const OID_O: &[u8] = &[0x55, 0x04, 0x0A];
const OID_BASIC: &[u8] = &[0x55, 0x1D, 0x13];
const OID_KEY_USAGE: &[u8] = &[0x55, 0x1D, 0x0F];
const OID_EXT_KEY_USAGE: &[u8] = &[0x55, 0x1D, 0x25];
const OID_SAN: &[u8] = &[0x55, 0x1D, 0x11];
const OID_SKI: &[u8] = &[0x55, 0x1D, 0x0E];
const OID_AKI: &[u8] = &[0x55, 0x1D, 0x23];
const OID_SERVER_AUTH: &[u8] = &[0x2B, 0x06, 0x01, 0x05, 0x05, 0x07, 0x03, 0x01];

/// SHA-1 の DigestInfo の前置き（RFC 8017 9.2 の注 1）。
pub const SHA1_DIGEST_INFO: &[u8] = &[
    0x30, 0x21, 0x30, 0x09, 0x06, 0x05, 0x2B, 0x0E, 0x03, 0x02, 0x1A, 0x05, 0x00, 0x04, 0x14,
];

fn sig_alg() -> Vec<u8> {
    seq(&[oid(OID_SHA1_RSA), vec![0x05, 0x00]])
}

/// 文字列の属性（PrintableString で書ける文字だけなら PrintableString、他は UTF8String）。
fn dir_string(s: &str) -> Vec<u8> {
    let printable = s
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b" '()+,-./:=?".contains(&c));
    tlv(if printable { 0x13 } else { 0x0C }, s.as_bytes())
}

fn name(cn: &str, org: Option<&str>) -> Vec<u8> {
    let mut rdns = Vec::new();
    if let Some(o) = org {
        rdns.push(tlv(0x31, &seq(&[oid(OID_O), dir_string(o)])));
    }
    rdns.push(tlv(0x31, &seq(&[oid(OID_CN), dir_string(cn)])));
    seq(&rdns)
}

fn public_key_info(k: &RsaKey) -> Vec<u8> {
    let rsa_pub = seq(&[int(&k.n.to_be_min()), int(&k.e.to_be_min())]);
    seq(&[seq(&[oid(OID_RSA), vec![0x05, 0x00]]), bit_string(&rsa_pub)])
}

/// 鍵の識別子（RFC 5280 4.2.1.2 の方法 1: 公開鍵の BIT STRING の SHA-1）。
fn key_id(k: &RsaKey) -> [u8; 20] {
    sha1(&seq(&[int(&k.n.to_be_min()), int(&k.e.to_be_min())]))
}

fn ext(id: &[u8], critical: bool, value: Vec<u8>) -> Vec<u8> {
    let mut parts = vec![oid(id)];
    if critical {
        parts.push(vec![0x01, 0x01, 0xFF]);
    }
    parts.push(tlv(0x04, &value));
    seq(&parts)
}

/// UTCTime（YYMMDDHHMMSSZ）。
fn utc(s: &str) -> Vec<u8> {
    tlv(0x17, s.as_bytes())
}

#[allow(clippy::too_many_arguments)]
fn build_cert(
    serial: &[u8],
    issuer: &[u8],
    subject: &[u8],
    subject_key: &RsaKey,
    extensions: Vec<Vec<u8>>,
    signer: &RsaKey,
) -> Vec<u8> {
    let tbs = seq(&[
        tlv(0xA0, &int(&[2])), // v3
        int(serial),
        sig_alg(),
        issuer.to_vec(),
        seq(&[utc("000101000000Z"), utc("491231235959Z")]),
        subject.to_vec(),
        public_key_info(subject_key),
        tlv(0xA3, &seq(&extensions)),
    ]);
    let mut di = SHA1_DIGEST_INFO.to_vec();
    di.extend_from_slice(&sha1(&tbs));
    let sig = signer.sign_pkcs1(&di);
    seq(&[tbs, sig_alg(), bit_string(&sig)])
}

fn serial(rng: &mut Drbg) -> Vec<u8> {
    let mut s = rng.bytes(16);
    s[0] &= 0x7F;
    s[0] |= 0x01;
    s
}

/// 自前の CA（鍵と自己署名の証明書）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ca {
    pub key: RsaKey,
    /// 証明書（DER）
    pub cert: Vec<u8>,
}

impl Ca {
    /// 新しい CA を作る（RSA 2048 ビット。数秒かかることがある）。
    pub fn generate(rng: &mut Drbg) -> Ca {
        let key = RsaKey::generate(rng, 2048);
        let n = name(CA_NAME, Some("CErulean"));
        let exts = vec![
            ext(OID_BASIC, true, seq(&[vec![0x01, 0x01, 0xFF]])),
            // keyCertSign・cRLSign（bit 5・6）
            ext(OID_KEY_USAGE, true, vec![0x03, 0x02, 0x01, 0x06]),
            ext(OID_SKI, false, tlv(0x04, &key_id(&key))),
        ];
        let cert = build_cert(&serial(rng), &n, &n, &key, exts, &key);
        Ca { key, cert }
    }

    /// host（接続先の名前）のサーバー証明書を leaf の鍵で作り、この CA で署名する。
    pub fn issue(&self, rng: &mut Drbg, host: &str, leaf: &RsaKey) -> Vec<u8> {
        let issuer = name(CA_NAME, Some("CErulean"));
        let subject = name(host, None);
        let ip: Option<[u8; 4]> = {
            let p: Vec<u8> = host.split('.').filter_map(|x| x.parse().ok()).collect();
            (p.len() == 4 && host.split('.').count() == 4).then(|| [p[0], p[1], p[2], p[3]])
        };
        let san = match ip {
            Some(a) => tlv(0x87, &a),           // iPAddress
            None => tlv(0x82, host.as_bytes()), // dNSName
        };
        let exts = vec![
            ext(OID_BASIC, false, seq(&[])),
            // digitalSignature・keyEncipherment（bit 0・2）
            ext(OID_KEY_USAGE, true, vec![0x03, 0x02, 0x05, 0xA0]),
            ext(OID_EXT_KEY_USAGE, false, seq(&[oid(OID_SERVER_AUTH)])),
            ext(OID_SAN, false, seq(&[san])),
            ext(OID_AKI, false, seq(&[tlv(0x80, &key_id(&self.key))])),
            ext(OID_SKI, false, tlv(0x04, &key_id(leaf))),
        ];
        build_cert(&serial(rng), &issuer, &subject, leaf, exts, &self.key)
    }

    /// 保存用の形（署名 "CRLNCA1\0" と、長さ u32 つきの証明書・鍵の各値）。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut v = b"CRLNCA1\0".to_vec();
        let k = &self.key;
        for part in [
            self.cert.clone(),
            k.n.to_be_min(),
            k.e.to_be_min(),
            k.d.to_be_min(),
            k.p.to_be_min(),
            k.q.to_be_min(),
            k.dp.to_be_min(),
            k.dq.to_be_min(),
            k.qinv.to_be_min(),
        ] {
            v.extend_from_slice(&(part.len() as u32).to_le_bytes());
            v.extend_from_slice(&part);
        }
        v
    }

    pub fn from_bytes(b: &[u8]) -> Result<Ca, String> {
        let bad = || "not a CErulean CA file".to_string();
        let mut rest = b.strip_prefix(b"CRLNCA1\0").ok_or_else(bad)?;
        let mut parts = Vec::new();
        for _ in 0..9 {
            if rest.len() < 4 {
                return Err(bad());
            }
            let n = u32::from_le_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let body = rest.get(4..4 + n).ok_or_else(bad)?;
            parts.push(body.to_vec());
            rest = &rest[4 + n..];
        }
        let b = |i: usize| Big::from_be(&parts[i]);
        let key = RsaKey {
            n: b(1),
            e: b(2),
            d: b(3),
            p: b(4),
            q: b(5),
            dp: b(6),
            dq: b(7),
            qinv: b(8),
        };
        if key.n.bits() < 1024 || key.p.mul(&key.q) != key.n {
            return Err(bad());
        }
        Ok(Ca {
            key,
            cert: parts[0].clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn der_lengths() {
        assert_eq!(tlv(0x04, &[1, 2]), vec![0x04, 2, 1, 2]);
        let long = tlv(0x04, &[0u8; 300]);
        assert_eq!(&long[..4], &[0x04, 0x82, 0x01, 0x2C]);
        assert_eq!(int(&[0x80]), vec![0x02, 2, 0, 0x80]);
        assert_eq!(int(&[0, 0, 5]), vec![0x02, 1, 5]);
    }

    /// 証明書の署名を CA の公開鍵で戻すと、tbs の SHA-1 の DigestInfo になる。
    #[test]
    fn issue_and_round_trip() {
        let mut rng = Drbg::new(b"x509 test");
        // 試験を速くするため小さな鍵で CA を組み立てる
        let ca_key = RsaKey::generate(&mut rng, 1024);
        let n = name(CA_NAME, Some("CErulean"));
        let cert = build_cert(&serial(&mut rng), &n, &n, &ca_key, vec![], &ca_key);
        let ca = Ca { key: ca_key, cert };
        let leaf = RsaKey::generate(&mut rng, 512);
        let c = ca.issue(&mut rng, "example.com", &leaf);
        // 外側の SEQUENCE の中の tbs を取り出す
        let inner = &c[4..]; // 30 82 LL LL の後
        let tlen = u16::from_be_bytes([inner[2], inner[3]]) as usize + 4;
        let tbs = &inner[..tlen];
        let sig_bits = &c[c.len() - ca.key.size()..];
        let em = ca.key.public(sig_bits);
        let mut di = SHA1_DIGEST_INFO.to_vec();
        di.extend_from_slice(&sha1(tbs));
        assert_eq!(&em[em.len() - di.len()..], &di[..]);
        assert!(c.windows(11).any(|w| w == b"example.com"));
        let back = Ca::from_bytes(&ca.to_bytes()).unwrap();
        assert_eq!(back, ca);
    }
}
