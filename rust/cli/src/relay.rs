//! 中継サーバー（`cerulean relay`）: ブラウザ版のネットワークを外へつなぐ。
//!
//! ブラウザは生の TCP を使えないので、ゲストの TCP はブラウザ内のスタック
//! （cerulean-net）で終端し、外への TCP のバイト列だけを 1 本の WebSocket に多重化して
//! ここへ送る。ここは接続先へ TCP をつなぎ、バイト列を中継するだけ（ゲストのフレームは
//! 見ない）。WebSocket・SHA-1・Base64 は自前。HTTPS の中継（ゲストの TLS はブラウザ内の
//! スタックが終端する）では、ここが今の TLS でサイトにつなぐ（rustls。upstream.rs）。
//!
//! 一次資料: RFC 6455（WebSocket）、RFC 3174（SHA-1）、RFC 4648（Base64）。
//!
//! 安全: 開いたプロキシにならないよう、接続の最初にトークンを確かめる（一致しなければ
//! 切る）。トークンを指定しなければ起動時に乱数で作って表示する。既定では私的アドレス
//! （LAN・localhost 等）へは中継しない（--allow-private で許す）。既定は 127.0.0.1 で
//! 待ち受ける（他の端末から使うときは --listen で変え、TLS は前段のリバースプロキシで
//! 付ける。https のページからは wss:// でないとつなげない）。`cerulean serve --with-relay`
//! はアプリの配信と同じポートの /relay でこれを動かす（serve.rs）。
//!
//! 中継の約束（WebSocket のバイナリメッセージ。整数はリトルエンディアン）:
//! ```text
//! ブラウザ → 中継
//!   01 HELLO    トークン（UTF-8）            最初に 1 回。違えば 86 を返して切る
//!   02 CONNECT  番号 u32・ポート u16・フラグ u8（bit0 = TLS でつなぐ）・接続先（名前か
//!               IPv4 の文字列）。TLS なら DATA は TLS の中の平文
//!   03 DATA     番号 u32・バイト列
//!   04 SHUTDOWN 番号 u32                      送信の終わり（相手へ FIN）
//!   05 CLOSE    番号 u32                      接続を捨てる
//!   06 CREDIT   番号 u32・バイト数 u32         この分だけ受け取れるようになった
//! 中継 → ブラウザ
//!   81 READY    版 u16（今は 2）
//!   82 CONNECTED 番号 u32・成否 u8
//!   83 DATA     番号 u32・バイト列（CREDIT の残りを超えては送らない）
//!   84 EOF      番号 u32
//!   85 RESET    番号 u32
//!   86 ERROR    文言（UTF-8）
//! ```

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::upstream::{self, Event, Upstream};

const USAGE: &str =
    "usage: cerulean relay [--listen ADDR:PORT | --port N] [--token TOKEN] [--allow-private]
  --listen A       待ち受けるアドレス（既定 127.0.0.1:8765）
  --port N         待ち受けるポート（アドレスは 127.0.0.1）
  --token T        接続に要るトークン（省くと起動時に乱数で作って表示する）
  --allow-private  私的アドレス（LAN・localhost 等）への接続も中継する（既定は断る）";

/// 中継の設定。
pub struct RelayOpts {
    pub token: String,
    pub allow_private: bool,
}

/// 中継の約束の版（2: CONNECT に TLS のフラグを足した。2026-09-30）。
const PROTOCOL_VERSION: u16 = 2;
/// 接続ごとの最初の CREDIT（ブラウザのスタックが溜められる量と同じ。
/// cerulean_net::MAX_BUFFERED）。
const INITIAL_CREDIT: usize = cerulean_net::MAX_BUFFERED;
/// 受け取る WebSocket のメッセージの上限（ブラウザは 1 回に 1 セグメント程度しか送らない）。
const MAX_MESSAGE: usize = 1 << 20;
/// 1 クライアントの同時接続の上限。
const MAX_STREAMS: usize = 256;

pub fn cmd_relay(args: &[String]) -> Result<std::process::ExitCode, String> {
    let mut listen = "127.0.0.1:8765".to_string();
    let mut token = None;
    let mut allow_private = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value"))
        };
        match a.as_str() {
            "--listen" => listen = val()?,
            "--port" => listen = format!("127.0.0.1:{}", val()?),
            "--token" => token = Some(val()?),
            "--allow-private" => allow_private = true,
            _ => return Err(USAGE.into()),
        }
    }
    let opts = Arc::new(RelayOpts {
        token: token_or_random(token)?,
        allow_private,
    });
    let l = TcpListener::bind(&listen).map_err(|e| format!("{listen}: {e}"))?;
    eprintln!("cerulean relay: listening on ws://{listen}/");
    eprintln!("cerulean relay: token {}", opts.token);
    if allow_private {
        eprintln!("cerulean relay: private addresses are allowed (--allow-private)");
    }
    for s in l.incoming() {
        let Ok(mut s) = s else { continue };
        let opts = opts.clone();
        std::thread::spawn(move || {
            let peer = s.peer_addr().map(|a| a.to_string()).unwrap_or_default();
            let r = read_head(&mut s).and_then(|head| serve_ws(s, &head, &opts));
            match r {
                Ok(()) => eprintln!("cerulean relay: {peer}: closed"),
                Err(e) => eprintln!("cerulean relay: {peer}: {e}"),
            }
        });
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// 指定のトークン（空は不可）か、なければ乱数で作ったもの。
pub fn token_or_random(token: Option<String>) -> Result<String, String> {
    match token {
        Some(t) if !t.is_empty() => Ok(t),
        Some(_) => Err("--token must not be empty".into()),
        None => Ok(random_token()),
    }
}

/// 乱数のトークン（std の HashMap の乱数の種を使う。暗号用の乱数源ではないが、
/// 推測されにくい 128 ビットの値として使う）。
fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

// ---- WebSocket（RFC 6455）----

/// WebSocket の送信側（複数のスレッドから書くので Mutex で守る）。
#[derive(Clone)]
struct WsOut(Arc<Mutex<TcpStream>>);

impl WsOut {
    /// サーバーからのフレームはマスクしない（RFC 6455 5.1）。
    fn send(&self, opcode: u8, payload: &[u8]) -> std::io::Result<()> {
        let mut f = Vec::with_capacity(payload.len() + 10);
        f.push(0x80 | opcode);
        let n = payload.len();
        if n < 126 {
            f.push(n as u8);
        } else if n <= 0xFFFF {
            f.push(126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            f.push(127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
        f.extend_from_slice(payload);
        let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
        s.write_all(&f)
    }

    fn msg(&self, kind: u8, id: u32, body: &[u8]) -> std::io::Result<()> {
        let mut m = Vec::with_capacity(5 + body.len());
        m.push(kind);
        m.extend_from_slice(&id.to_le_bytes());
        m.extend_from_slice(body);
        self.send(2, &m)
    }
}

/// HTTP の要求の頭（空行まで）を読む。
pub fn read_head(s: &mut TcpStream) -> Result<String, String> {
    let mut req = Vec::new();
    let mut b = [0u8; 1];
    while !req.ends_with(b"\r\n\r\n") {
        if req.len() > 16 * 1024 {
            return Err("request header too large".into());
        }
        if s.read(&mut b).map_err(|e| e.to_string())? == 0 {
            return Err("closed before the request header".into());
        }
        req.push(b[0]);
    }
    Ok(String::from_utf8_lossy(&req).into_owned())
}

/// 要求の頭が WebSocket へのアップグレードか。
pub fn is_websocket(head: &str) -> bool {
    head.split("\r\n").skip(1).any(|l| {
        l.split_once(':').is_some_and(|(k, v)| {
            k.trim().eq_ignore_ascii_case("upgrade") && v.trim().eq_ignore_ascii_case("websocket")
        })
    })
}

/// HTTP のアップグレード（RFC 6455 4.2）。
fn handshake(s: &mut TcpStream, text: &str) -> Result<(), String> {
    let mut key = None;
    let mut upgrade = false;
    for line in text.split("\r\n").skip(1) {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "sec-websocket-key" {
            key = Some(v.to_string());
        } else if k == "upgrade" && v.eq_ignore_ascii_case("websocket") {
            upgrade = true;
        }
    }
    let (true, Some(key)) = (upgrade, key) else {
        let _ = s.write_all(
            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 38\r\nConnection: close\r\n\r\nCErulean relay: WebSocket only here.\r\n",
        );
        return Err("not a WebSocket request".into());
    };
    let accept = base64(&sha1(
        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
    ));
    let resp = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    s.write_all(resp.as_bytes()).map_err(|e| e.to_string())
}

/// メッセージを 1 つ読む（断片を組み立てる。ping には pong を返す）。None は close。
fn read_message(s: &mut TcpStream, out: &WsOut) -> Result<Option<Vec<u8>>, String> {
    let mut msg = Vec::new();
    loop {
        let mut h = [0u8; 2];
        s.read_exact(&mut h).map_err(|e| e.to_string())?;
        let fin = h[0] & 0x80 != 0;
        let opcode = h[0] & 0x0F;
        if h[1] & 0x80 == 0 {
            return Err("client frame is not masked".into()); // RFC 6455 5.1
        }
        let mut len = (h[1] & 0x7F) as u64;
        if len == 126 {
            let mut b = [0u8; 2];
            s.read_exact(&mut b).map_err(|e| e.to_string())?;
            len = u16::from_be_bytes(b) as u64;
        } else if len == 127 {
            let mut b = [0u8; 8];
            s.read_exact(&mut b).map_err(|e| e.to_string())?;
            len = u64::from_be_bytes(b);
        }
        if len as usize > MAX_MESSAGE || msg.len() + len as usize > MAX_MESSAGE {
            return Err("message too large".into());
        }
        let mut mask = [0u8; 4];
        s.read_exact(&mut mask).map_err(|e| e.to_string())?;
        let mut p = vec![0u8; len as usize];
        s.read_exact(&mut p).map_err(|e| e.to_string())?;
        for (i, x) in p.iter_mut().enumerate() {
            *x ^= mask[i % 4];
        }
        match opcode {
            0x8 => {
                let _ = out.send(0x8, &[]);
                return Ok(None);
            }
            0x9 => {
                out.send(0xA, &p).map_err(|e| e.to_string())?;
                continue;
            }
            0xA => continue,
            0x0..=0x2 => {
                msg.extend_from_slice(&p);
                if fin {
                    return Ok(Some(msg));
                }
            }
            _ => return Err(format!("unknown opcode {opcode}")),
        }
    }
}

// ---- 多重化 ----

/// 読んだ要求の頭で WebSocket に切り替え、中継する（接続が閉じるまで戻らない）。
pub fn serve_ws(mut s: TcpStream, head: &str, opts: &RelayOpts) -> Result<(), String> {
    let token = opts.token.as_str();
    let allow_private = opts.allow_private;
    let _ = s.set_nodelay(true);
    handshake(&mut s, head)?;
    let out = WsOut(Arc::new(Mutex::new(
        s.try_clone().map_err(|e| e.to_string())?,
    )));
    // 最初のメッセージでトークンを確かめる
    s.set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let hello = read_message(&mut s, &out)?.ok_or("closed before HELLO")?;
    if hello.first() != Some(&0x01) || !const_eq(&hello[1..], token.as_bytes()) {
        let mut m = vec![0x86];
        m.extend_from_slice("bad token".as_bytes());
        let _ = out.send(2, &m);
        let _ = out.send(0x8, &[]);
        return Err("bad token".into());
    }
    s.set_read_timeout(None).map_err(|e| e.to_string())?;
    let mut ready = vec![0x81];
    ready.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    out.send(2, &ready).map_err(|e| e.to_string())?;

    // 番号 → 接続（つながるまでは None）
    let streams: Arc<Mutex<BTreeMap<u32, Option<Upstream>>>> = Arc::default();
    let r = (|| -> Result<(), String> {
        while let Some(m) = read_message(&mut s, &out)? {
            if m.len() < 5 {
                return Err("short message".into());
            }
            let id = u32::from_le_bytes([m[1], m[2], m[3], m[4]]);
            let body = &m[5..];
            let get = |id: u32| -> Option<Upstream> {
                streams
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&id)
                    .cloned()
                    .flatten()
            };
            match m[0] {
                0x02 => {
                    if body.len() < 3 {
                        return Err("bad CONNECT".into());
                    }
                    let mut st = streams.lock().unwrap_or_else(|e| e.into_inner());
                    if st.contains_key(&id) {
                        return Err("duplicate CONNECT".into());
                    }
                    if st.len() >= MAX_STREAMS {
                        out.msg(0x82, id, &[0]).map_err(|e| e.to_string())?;
                        continue;
                    }
                    st.insert(id, None);
                    drop(st);
                    let port = u16::from_le_bytes([body[0], body[1]]);
                    let tls = body[2] & 1 != 0;
                    let host = String::from_utf8_lossy(&body[3..]).to_string();
                    let (out, streams) = (out.clone(), streams.clone());
                    let what = format!("#{id} {host}:{port}{}", if tls { " (tls)" } else { "" });
                    upstream::spawn(
                        host,
                        port,
                        tls,
                        Some(INITIAL_CREDIT),
                        allow_private,
                        move |ev| match ev {
                            Event::Connected(up) => {
                                let ok = up.is_some();
                                eprintln!(
                                    "cerulean relay: {what} {}",
                                    if ok { "connected" } else { "failed" }
                                );
                                let mut st = streams.lock().unwrap_or_else(|e| e.into_inner());
                                match (st.get_mut(&id), up) {
                                    (Some(slot), Some(up)) => *slot = Some(up),
                                    // 待つ間に捨てられた
                                    (None, Some(up)) => up.close(),
                                    _ => {
                                        st.remove(&id);
                                    }
                                }
                                drop(st);
                                let _ = out.msg(0x82, id, &[ok as u8]);
                            }
                            Event::Data(d) => {
                                let _ = out.msg(0x83, id, &d);
                            }
                            Event::Eof => {
                                let _ = out.msg(0x84, id, &[]);
                            }
                            Event::Reset => {
                                let _ = out.msg(0x85, id, &[]);
                            }
                        },
                    );
                }
                0x03 => {
                    if let Some(up) = get(id)
                        && up.write(body).is_err()
                    {
                        up.close();
                    }
                }
                0x04 => {
                    if let Some(up) = get(id) {
                        up.shutdown_write();
                    }
                }
                0x05 => {
                    let up = streams
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id)
                        .flatten();
                    if let Some(up) = up {
                        up.close();
                    }
                }
                0x06 => {
                    if body.len() >= 4
                        && let Some(up) = get(id)
                    {
                        let n = u32::from_le_bytes([body[0], body[1], body[2], body[3]]) as usize;
                        up.add_credit(n, INITIAL_CREDIT * 4);
                    }
                }
                k => return Err(format!("unknown message {k:#x}")),
            }
        }
        Ok(())
    })();
    for up in std::mem::take(&mut *streams.lock().unwrap_or_else(|e| e.into_inner()))
        .into_values()
        .flatten()
    {
        up.close();
    }
    r
}

/// 長さに依らない時間で比べる（トークンの比較）。
fn const_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---- SHA-1（RFC 3174）・Base64（RFC 4648）: ハンドシェイクの応答だけに使う ----

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut m = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    m.push(0x80);
    while m.len() % 64 != 56 {
        m.push(0);
    }
    m.extend_from_slice(&bits.to_be_bytes());
    for block in m.chunks(64) {
        let mut w = [0u32; 80];
        for (i, c) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in data.chunks(3) {
        let v = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(v >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 3174 の試験値と、RFC 6455 1.3 の Sec-WebSocket-Accept の例。
    #[test]
    fn sha1_and_accept() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(
            hex(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hex(&sha1(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let acc = base64(&sha1(
            format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
        ));
        assert_eq!(acc, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn base64_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
    }
}
