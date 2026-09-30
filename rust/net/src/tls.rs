//! ゲストの TLS を終端するサーバー（TLS 1.0。sans-IO）。
//!
//! WM5 の IE Mobile は SSL 2.0 互換の ClientHello で TLS 1.0 を求め、RC4_128_MD5・
//! RC4_128_SHA・3DES_EDE_CBC_SHA と輸出用の弱い暗号だけを出す（AES・拡張・SNI なし。
//! 2026-09-30 観察）。ここでは RSA の鍵交換と RC4_128_SHA（なければ RC4_128_MD5）だけを
//! 受ける。外（本物のサイト）へは中継側が今の TLS でつなぎ直すので、この古い暗号が使われるのは
//! エミュレータの中（ゲストとこのスタックの間）だけ。
//!
//! 一次資料: RFC 2246（TLS 1.0。E.1 が SSL 2.0 互換の ClientHello）。
//! セッションの再開・再ネゴシエーション・圧縮は受けない（セッション ID は空で返す）。

use crate::bigint::RsaKey;
use crate::crypto::{Drbg, Rc4, hmac, md5, md5v, sha1, sha1v};

const TLS10: [u8; 2] = [3, 1];
const CT_CCS: u8 = 20;
const CT_ALERT: u8 = 21;
const CT_HANDSHAKE: u8 = 22;
const CT_APP: u8 = 23;
const HS_CLIENT_HELLO: u8 = 1;
const HS_SERVER_HELLO: u8 = 2;
const HS_CERTIFICATE: u8 = 11;
const HS_SERVER_DONE: u8 = 14;
const HS_CLIENT_KX: u8 = 16;
const HS_FINISHED: u8 = 20;
const RC4_128_MD5: u16 = 0x0004;
const RC4_128_SHA: u16 = 0x0005;
/// 1 レコードの平文の上限（RFC 2246 6.2.1）。
const MAX_FRAGMENT: usize = 16384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Hello,
    KeyExchange,
    ChangeCipher,
    Finished,
    Open,
    Closed,
}

struct Cipher {
    rc4: Rc4,
    mac_key: Vec<u8>,
    sha: bool,
    seq: u64,
}

impl Cipher {
    fn mac(&self, ct: u8, data: &[u8]) -> Vec<u8> {
        let mut m = self.seq.to_be_bytes().to_vec();
        m.push(ct);
        m.extend_from_slice(&TLS10);
        m.extend_from_slice(&(data.len() as u16).to_be_bytes());
        m.extend_from_slice(data);
        hmac(if self.sha { sha1v } else { md5v }, &self.mac_key, &m)
    }

    fn mac_len(&self) -> usize {
        if self.sha { 20 } else { 16 }
    }
}

/// TLS の失敗（ゲストへは alert を送った後）。
#[derive(Debug, PartialEq, Eq)]
pub struct TlsError(pub &'static str);

pub struct TlsServer {
    state: State,
    chain: Vec<Vec<u8>>,
    key: RsaKey,
    inbuf: Vec<u8>,
    hsbuf: Vec<u8>,
    transcript: Vec<u8>,
    client_random: [u8; 32],
    server_random: [u8; 32],
    suite: u16,
    master: Vec<u8>,
    pending: Option<(Cipher, Cipher)>,
    /// ゲストの ChangeCipherSpec の後、こちらの ChangeCipherSpec まで待たせる書き込みの鍵
    pending_write: Option<Cipher>,
    read: Option<Cipher>,
    write: Option<Cipher>,
    out: Vec<u8>,
    peer_closed: bool,
}

/// TLS 1.0 の PRF（RFC 2246 5）。
fn prf(secret: &[u8], label: &[u8], seed: &[u8], len: usize) -> Vec<u8> {
    let half = secret.len().div_ceil(2);
    let (s1, s2) = (&secret[..half], &secret[secret.len() - half..]);
    let mut ls = label.to_vec();
    ls.extend_from_slice(seed);
    let a = p_hash(md5v, s1, &ls, len);
    let b = p_hash(sha1v, s2, &ls, len);
    a.iter().zip(&b).map(|(x, y)| x ^ y).collect()
}

fn p_hash(h: fn(&[u8]) -> Vec<u8>, secret: &[u8], seed: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 20);
    let mut a = hmac(h, secret, seed);
    while out.len() < len {
        let mut x = a.clone();
        x.extend_from_slice(seed);
        out.extend_from_slice(&hmac(h, secret, &x));
        a = hmac(h, secret, &a);
    }
    out.truncate(len);
    out
}

fn u24(n: usize) -> [u8; 3] {
    [(n >> 16) as u8, (n >> 8) as u8, n as u8]
}

fn hs_msg(ty: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![ty];
    m.extend_from_slice(&u24(body.len()));
    m.extend_from_slice(body);
    m
}

impl TlsServer {
    /// chain は先頭がサーバー証明書（key の公開鍵）、以後は発行者の証明書。
    pub fn new(chain: Vec<Vec<u8>>, key: RsaKey) -> TlsServer {
        TlsServer {
            state: State::Hello,
            chain,
            key,
            inbuf: Vec::new(),
            hsbuf: Vec::new(),
            transcript: Vec::new(),
            client_random: [0; 32],
            server_random: [0; 32],
            suite: 0,
            master: Vec::new(),
            pending: None,
            pending_write: None,
            read: None,
            write: None,
            out: Vec::new(),
            peer_closed: false,
        }
    }

    /// ハンドシェイクが済んでアプリケーションのデータを送れるか。
    pub fn is_open(&self) -> bool {
        self.state == State::Open
    }

    /// ゲストが close_notify を送ってきたか。
    pub fn peer_closed(&self) -> bool {
        self.peer_closed
    }

    /// ゲストへ送るバイト（TLS のレコード）。
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.out)
    }

    fn record(&mut self, ct: u8, data: &[u8]) {
        for frag in data
            .chunks(MAX_FRAGMENT)
            .chain(data.is_empty().then_some(&[][..]))
        {
            let body = match &mut self.write {
                Some(c) => {
                    let mut b = frag.to_vec();
                    b.extend_from_slice(&c.mac(ct, frag));
                    c.rc4.apply(&mut b);
                    c.seq += 1;
                    b
                }
                None => frag.to_vec(),
            };
            self.out.push(ct);
            self.out.extend_from_slice(&TLS10);
            self.out
                .extend_from_slice(&(body.len() as u16).to_be_bytes());
            self.out.extend_from_slice(&body);
        }
    }

    fn fail(&mut self, desc: u8, why: &'static str) -> TlsError {
        if self.state != State::Closed {
            self.record(CT_ALERT, &[2, desc]);
            self.state = State::Closed;
        }
        TlsError(why)
    }

    /// 外から届いた平文をゲストへ送る（ハンドシェイクの後だけ）。
    pub fn send(&mut self, data: &[u8]) {
        if self.state == State::Open && !data.is_empty() {
            self.record(CT_APP, data);
        }
    }

    /// 送信の終わり（close_notify）。
    pub fn close(&mut self) {
        if self.state == State::Open {
            self.record(CT_ALERT, &[1, 0]);
        }
    }

    /// ゲストから届いたバイトを処理し、アプリケーションの平文を返す。
    pub fn input(&mut self, data: &[u8], rng: &mut Drbg) -> Result<Vec<u8>, TlsError> {
        if self.state == State::Closed {
            return Ok(Vec::new());
        }
        self.inbuf.extend_from_slice(data);
        let mut plain = Vec::new();
        loop {
            // SSL 2.0 互換の ClientHello（RFC 2246 E.1）
            if self.state == State::Hello
                && self.hsbuf.is_empty()
                && self.inbuf.first().is_some_and(|b| b & 0x80 != 0)
            {
                if self.inbuf.len() < 2 {
                    break;
                }
                let len = ((self.inbuf[0] & 0x7F) as usize) << 8 | self.inbuf[1] as usize;
                if self.inbuf.len() < 2 + len {
                    break;
                }
                let msg: Vec<u8> = self.inbuf[2..2 + len].to_vec();
                self.inbuf.drain(..2 + len);
                self.v2_hello(&msg, rng)?;
                continue;
            }
            if self.inbuf.len() < 5 {
                break;
            }
            let ct = self.inbuf[0];
            let len = u16::from_be_bytes([self.inbuf[3], self.inbuf[4]]) as usize;
            if len > MAX_FRAGMENT + 2048 {
                return Err(self.fail(22, "record too long"));
            }
            if self.inbuf.len() < 5 + len {
                break;
            }
            let mut body: Vec<u8> = self.inbuf[5..5 + len].to_vec();
            self.inbuf.drain(..5 + len);
            if let Some(c) = &mut self.read {
                c.rc4.apply(&mut body);
                let ml = c.mac_len();
                if body.len() < ml {
                    return Err(self.fail(20, "short record"));
                }
                let (d, m) = body.split_at(body.len() - ml);
                let want = c.mac(ct, d);
                c.seq += 1;
                if want != m {
                    return Err(self.fail(20, "bad record MAC"));
                }
                body.truncate(body.len() - ml);
            }
            match ct {
                CT_HANDSHAKE => {
                    self.hsbuf.extend_from_slice(&body);
                    while self.hsbuf.len() >= 4 {
                        let n = (self.hsbuf[1] as usize) << 16
                            | (self.hsbuf[2] as usize) << 8
                            | self.hsbuf[3] as usize;
                        if self.hsbuf.len() < 4 + n {
                            break;
                        }
                        let msg: Vec<u8> = self.hsbuf.drain(..4 + n).collect();
                        self.handshake(&msg, rng)?;
                    }
                }
                CT_CCS => {
                    if self.state != State::ChangeCipher || body != [1] {
                        return Err(self.fail(10, "unexpected ChangeCipherSpec"));
                    }
                    let (r, w) = self.pending.take().ok_or(TlsError("no keys"))?;
                    self.read = Some(r);
                    self.pending_write = Some(w);
                    self.state = State::Finished;
                }
                CT_ALERT => {
                    // close_notify（desc 0）には close_notify を返して閉じる。致命的な
                    // alert（level 2）でも閉じる。警告は無視する
                    let close = body.get(1) == Some(&0);
                    if close {
                        self.peer_closed = true;
                        if self.state == State::Open {
                            self.record(CT_ALERT, &[1, 0]);
                        }
                    }
                    if close || body.first() == Some(&2) {
                        self.state = State::Closed;
                        return Ok(plain);
                    }
                }
                CT_APP => {
                    if self.state != State::Open {
                        return Err(self.fail(10, "application data before handshake"));
                    }
                    plain.extend_from_slice(&body);
                }
                _ => return Err(self.fail(10, "unknown record type")),
            }
        }
        Ok(plain)
    }

    fn choose_suite(&mut self, suites: &[u16]) -> Result<(), TlsError> {
        self.suite = if suites.contains(&RC4_128_SHA) {
            RC4_128_SHA
        } else if suites.contains(&RC4_128_MD5) {
            RC4_128_MD5
        } else {
            return Err(self.fail(40, "no common cipher suite"));
        };
        Ok(())
    }

    fn v2_hello(&mut self, msg: &[u8], rng: &mut Drbg) -> Result<(), TlsError> {
        if msg.len() < 9 || msg[0] != HS_CLIENT_HELLO || msg[1] != 3 || msg[2] < 1 {
            return Err(self.fail(40, "unsupported SSL 2.0 hello"));
        }
        let cs = u16::from_be_bytes([msg[3], msg[4]]) as usize;
        let sid = u16::from_be_bytes([msg[5], msg[6]]) as usize;
        let ch = u16::from_be_bytes([msg[7], msg[8]]) as usize;
        if msg.len() != 9 + cs + sid + ch || !cs.is_multiple_of(3) || !(16..=32).contains(&ch) {
            return Err(self.fail(40, "bad SSL 2.0 hello"));
        }
        let suites: Vec<u16> = msg[9..9 + cs]
            .chunks(3)
            .filter(|s| s[0] == 0)
            .map(|s| u16::from_be_bytes([s[1], s[2]]))
            .collect();
        let challenge = &msg[9 + cs + sid..];
        self.client_random = [0; 32];
        self.client_random[32 - ch..].copy_from_slice(challenge);
        self.transcript.extend_from_slice(msg);
        self.choose_suite(&suites)?;
        self.server_hello(rng);
        Ok(())
    }

    fn handshake(&mut self, msg: &[u8], rng: &mut Drbg) -> Result<(), TlsError> {
        let body = &msg[4..];
        match (self.state, msg[0]) {
            (State::Hello, HS_CLIENT_HELLO) => {
                if body.len() < 38 || body[0] != 3 || body[1] < 1 {
                    return Err(self.fail(70, "unsupported version"));
                }
                self.client_random.copy_from_slice(&body[2..34]);
                let sid = body[34] as usize;
                let p = 35 + sid;
                let n = body
                    .get(p..p + 2)
                    .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
                    .ok_or(TlsError("short hello"))?;
                let list = body.get(p + 2..p + 2 + n).ok_or(TlsError("short hello"))?;
                let suites: Vec<u16> = list
                    .chunks(2)
                    .filter(|c| c.len() == 2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                self.transcript.extend_from_slice(msg);
                self.choose_suite(&suites)?;
                self.server_hello(rng);
            }
            (State::KeyExchange, HS_CLIENT_KX) => {
                let k = self.key.size();
                // TLS 1.0 は長さ 2 バイトを前置きする（RFC 2246 7.4.7.1）。SSL 3.0 式の
                // 前置きなしも受ける
                let enc = if body.len() == k + 2
                    && u16::from_be_bytes([body[0], body[1]]) as usize == k
                {
                    &body[2..]
                } else {
                    body
                };
                // 復号に失敗しても乱数の premaster で続け、Finished で失敗させる
                // （RFC 2246 7.4.7.1 の注: 復号の成否を見せない）
                let pms = match self.key.decrypt_pkcs1(enc) {
                    Some(p) if p.len() == 48 => p,
                    _ => rng.bytes(48),
                };
                self.transcript.extend_from_slice(msg);
                let mut seed = self.client_random.to_vec();
                seed.extend_from_slice(&self.server_random);
                self.master = prf(&pms, b"master secret", &seed, 48);
                let mut seed2 = self.server_random.to_vec();
                seed2.extend_from_slice(&self.client_random);
                let mac_len = if self.suite == RC4_128_SHA { 20 } else { 16 };
                let kb = prf(&self.master, b"key expansion", &seed2, 2 * mac_len + 32);
                let (cm, rest) = kb.split_at(mac_len);
                let (sm, rest) = rest.split_at(mac_len);
                let (ck, sk) = rest.split_at(16);
                let sha = self.suite == RC4_128_SHA;
                let mk = |mac: &[u8], key: &[u8]| Cipher {
                    rc4: Rc4::new(key),
                    mac_key: mac.to_vec(),
                    sha,
                    seq: 0,
                };
                self.pending = Some((mk(cm, ck), mk(sm, sk)));
                self.state = State::ChangeCipher;
            }
            (State::Finished, HS_FINISHED) => {
                let want = self.finished(b"client finished");
                if body != want.as_slice() {
                    return Err(self.fail(51, "bad client Finished"));
                }
                self.transcript.extend_from_slice(msg);
                self.record(CT_CCS, &[1]);
                self.write = self.pending_write.take();
                let fin = hs_msg(HS_FINISHED, &self.finished(b"server finished"));
                self.record(CT_HANDSHAKE, &fin);
                self.state = State::Open;
            }
            _ => return Err(self.fail(10, "unexpected handshake message")),
        }
        Ok(())
    }

    fn finished(&self, label: &[u8]) -> Vec<u8> {
        let mut seed = md5(&self.transcript).to_vec();
        seed.extend_from_slice(&sha1(&self.transcript));
        prf(&self.master, label, &seed, 12)
    }

    fn server_hello(&mut self, rng: &mut Drbg) {
        self.server_random.copy_from_slice(&rng.bytes(32));
        let mut sh = TLS10.to_vec();
        sh.extend_from_slice(&self.server_random);
        sh.push(0); // セッション ID なし（再開しない）
        sh.extend_from_slice(&self.suite.to_be_bytes());
        sh.push(0); // 圧縮なし
        let mut certs = Vec::new();
        for c in &self.chain {
            certs.extend_from_slice(&u24(c.len()));
            certs.extend_from_slice(c);
        }
        let mut cb = u24(certs.len()).to_vec();
        cb.extend_from_slice(&certs);
        let mut flight = hs_msg(HS_SERVER_HELLO, &sh);
        flight.extend_from_slice(&hs_msg(HS_CERTIFICATE, &cb));
        flight.extend_from_slice(&hs_msg(HS_SERVER_DONE, &[]));
        self.transcript.extend_from_slice(&flight);
        self.record(CT_HANDSHAKE, &flight);
        self.state = State::KeyExchange;
    }
}

#[cfg(test)]
#[path = "tls/tests.rs"]
mod tests;
