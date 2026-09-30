//! 外への 1 本の接続（TCP か、今の TLS）。中継サーバー（relay.rs）と `run --net`（net.rs）が
//! 使う。TLS は rustls（webpki-roots の Mozilla のルートで接続先の名前を確かめる）。
//!
//! 接続・ハンドシェイク・読み出しは接続ごとのスレッドで行い、結果は on_event で知らせる。
//! 書き込みは呼び出し側のスレッドから（TLS の状態は Mutex で守る）。
//! 読み出しは credit の残りの分だけ行う（中継ではブラウザが受け取れる量。None なら無制限）。

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

pub enum Event {
    /// 接続（TLS ならハンドシェイクと証明書の確認まで）の結果
    Connected(Option<Upstream>),
    Data(Vec<u8>),
    Eof,
    Reset,
}

struct Inner {
    sock: TcpStream,
    tls: Option<Mutex<rustls::ClientConnection>>,
    /// あと何バイト読んでよいか（None = 無制限）
    credit: Mutex<Option<usize>>,
    cv: Condvar,
    closed: Mutex<bool>,
}

/// 接続の書き込み側の取っ手（複製してよい）。
#[derive(Clone)]
pub struct Upstream(Arc<Inner>);

fn tls_config() -> Arc<rustls::ClientConfig> {
    static CFG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();
    CFG.get_or_init(|| {
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let cfg = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring supports the default TLS versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(cfg)
    })
    .clone()
}

impl Upstream {
    /// TLS なら暗号化して送る。
    pub fn write(&self, data: &[u8]) -> std::io::Result<()> {
        let mut s = &self.0.sock;
        match &self.0.tls {
            None => s.write_all(data),
            Some(t) => {
                let mut c = t.lock().unwrap_or_else(|e| e.into_inner());
                c.writer().write_all(data)?;
                while c.wants_write() {
                    c.write_tls(&mut s)?;
                }
                Ok(())
            }
        }
    }

    /// 送信の終わり（TLS なら close_notify を送ってから）。
    pub fn shutdown_write(&self) {
        if let Some(t) = &self.0.tls {
            let mut c = t.lock().unwrap_or_else(|e| e.into_inner());
            c.send_close_notify();
            let mut s = &self.0.sock;
            while c.wants_write() {
                if c.write_tls(&mut s).is_err() {
                    break;
                }
            }
        }
        let _ = self.0.sock.shutdown(Shutdown::Write);
    }

    pub fn close(&self) {
        *self.0.closed.lock().unwrap_or_else(|e| e.into_inner()) = true;
        let _ = self.0.sock.shutdown(Shutdown::Both);
        self.0.cv.notify_all();
    }

    pub fn add_credit(&self, n: usize, max: usize) {
        let mut c = self.0.credit.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = c.as_mut() {
            *v = (*v + n).min(max);
        }
        self.0.cv.notify_all();
    }

    fn is_closed(&self) -> bool {
        *self.0.closed.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// host:port へつなぐスレッドを起こす。credit は最初に読んでよいバイト数（None = 無制限）。
pub fn spawn(
    host: String,
    port: u16,
    tls: bool,
    credit: Option<usize>,
    mut on_event: impl FnMut(Event) + Send + 'static,
) {
    std::thread::spawn(move || {
        let Some(up) = connect(&host, port, tls, credit) else {
            on_event(Event::Connected(None));
            return;
        };
        on_event(Event::Connected(Some(up.clone())));
        read_loop(&up, &mut on_event);
    });
}

fn connect(host: &str, port: u16, tls: bool, credit: Option<usize>) -> Option<Upstream> {
    let addrs: Vec<_> = (host, port)
        .to_socket_addrs()
        .map(|a| a.filter(|a| a.is_ipv4()).collect())
        .unwrap_or_default();
    let mut sock = addrs
        .iter()
        .find_map(|a| TcpStream::connect_timeout(a, CONNECT_TIMEOUT).ok())?;
    let _ = sock.set_nodelay(true);
    let tls = if tls {
        let name = rustls::pki_types::ServerName::try_from(host.to_string()).ok()?;
        let mut c = rustls::ClientConnection::new(tls_config(), name).ok()?;
        // ハンドシェイク（証明書の確認を含む）を済ませてから接続できたとする
        sock.set_read_timeout(Some(CONNECT_TIMEOUT)).ok()?;
        while c.is_handshaking() {
            if let Err(e) = c.complete_io(&mut sock) {
                eprintln!("cerulean: tls {host}:{port}: {e}");
                return None;
            }
        }
        sock.set_read_timeout(None).ok()?;
        Some(Mutex::new(c))
    } else {
        None
    };
    Some(Upstream(Arc::new(Inner {
        sock,
        tls,
        credit: Mutex::new(credit),
        cv: Condvar::new(),
        closed: Mutex::new(false),
    })))
}

fn read_loop(up: &Upstream, on_event: &mut impl FnMut(Event)) {
    let mut buf = vec![0u8; 16 * 1024];
    let mut sock = match up.0.sock.try_clone() {
        Ok(s) => s,
        Err(_) => {
            on_event(Event::Reset);
            return;
        }
    };
    loop {
        // credit の残りを待つ
        let allowed = {
            let mut c = up.0.credit.lock().unwrap_or_else(|e| e.into_inner());
            while *c == Some(0) && !up.is_closed() {
                c = up.0.cv.wait(c).unwrap_or_else(|e| e.into_inner());
            }
            c.unwrap_or(usize::MAX)
        };
        if up.is_closed() {
            return;
        }
        let lim = buf.len().min(allowed);
        let n = match sock.read(&mut buf[..lim]) {
            Ok(n) => n,
            Err(_) => {
                if !up.is_closed() {
                    on_event(Event::Reset);
                }
                return;
            }
        };
        let (data, eof) = match &up.0.tls {
            None => (buf[..n].to_vec(), n == 0),
            Some(t) => {
                let mut c = t.lock().unwrap_or_else(|e| e.into_inner());
                let mut out = Vec::new();
                let mut eof = n == 0;
                if n > 0 {
                    let mut rd = &buf[..n];
                    while !rd.is_empty() {
                        if c.read_tls(&mut rd).is_err() {
                            break;
                        }
                        match c.process_new_packets() {
                            Ok(st) => {
                                let want = st.plaintext_bytes_to_read();
                                let start = out.len();
                                out.resize(start + want, 0);
                                let _ = c.reader().read_exact(&mut out[start..]);
                                if st.peer_has_closed() {
                                    eof = true;
                                }
                            }
                            Err(_) => {
                                drop(c);
                                on_event(Event::Reset);
                                return;
                            }
                        }
                    }
                    let mut s = &up.0.sock;
                    while c.wants_write() {
                        if c.write_tls(&mut s).is_err() {
                            break;
                        }
                    }
                }
                (out, eof)
            }
        };
        if !data.is_empty() {
            // credit は渡した量で減らす（TLS は平文の量で数えるので、1 回の読みで
            // 残りを少し超えることがある。超えた分は 0 で止まる）
            let mut c = up.0.credit.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(v) = c.as_mut() {
                *v = v.saturating_sub(data.len());
            }
            drop(c);
            on_event(Event::Data(data));
        }
        if eof {
            on_event(Event::Eof);
            return;
        }
    }
}
