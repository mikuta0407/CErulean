//! ネットワーク（イーサネットカード）の CLI 側: フレームの pcap への書き出しと、
//! OS のソケットで外へつなぐ経路（cerulean-net のスタックを使う）。

use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::sync::mpsc;

use cerulean_core::smdk2410::INSTRUCTIONS_PER_SECOND;
use cerulean_net::x509::Ca;
use cerulean_net::{Request, Stack, Target};

use crate::upstream::{self, Event, Upstream};

/// pcap（libpcap の古典形式、リンク層はイーサネット）に書く。時刻は仮想時間
/// （命令数から）なので、同じ入力なら同じファイルになる。
pub struct Pcap {
    out: std::io::BufWriter<std::fs::File>,
}

impl Pcap {
    pub fn create(path: &str) -> Result<Pcap, String> {
        let f = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
        let mut out = std::io::BufWriter::new(f);
        let mut h = Vec::new();
        h.extend_from_slice(&0xA1B2_C3D4u32.to_le_bytes()); // マイクロ秒の時刻
        h.extend_from_slice(&2u16.to_le_bytes());
        h.extend_from_slice(&4u16.to_le_bytes());
        h.extend_from_slice(&0i32.to_le_bytes());
        h.extend_from_slice(&0u32.to_le_bytes());
        h.extend_from_slice(&65535u32.to_le_bytes());
        h.extend_from_slice(&1u32.to_le_bytes()); // LINKTYPE_ETHERNET
        out.write_all(&h).map_err(|e| e.to_string())?;
        Ok(Pcap { out })
    }

    pub fn write(&mut self, steps: u64, frame: &[u8]) -> Result<(), String> {
        let us = (steps as u128 * 1_000_000 / INSTRUCTIONS_PER_SECOND as u128) as u64;
        let mut h = Vec::with_capacity(16);
        h.extend_from_slice(&((us / 1_000_000) as u32).to_le_bytes());
        h.extend_from_slice(&((us % 1_000_000) as u32).to_le_bytes());
        h.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        h.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        self.out.write_all(&h).map_err(|e| e.to_string())?;
        self.out.write_all(frame).map_err(|e| e.to_string())
    }

    pub fn flush(&mut self) -> Result<(), String> {
        self.out.flush().map_err(|e| e.to_string())
    }
}

enum HostEvent {
    Connected(u32, Option<Upstream>),
    Data(u32, Vec<u8>),
    Eof(u32),
    Reset(u32),
}

struct HostConn {
    /// 書き込み側（接続できるまで None）
    up: Option<Upstream>,
    /// スタックが受け取れずに待たせている外からのデータ
    pending: VecDeque<Vec<u8>>,
    eof: bool,
}

/// OS のソケットで直接外へつなぐ（CLI 用。中継サーバーを使わない）。接続・読み出しは
/// 接続ごとのスレッドで行い（upstream.rs）、結果はチャネルで受け取る。
/// 外とのやり取りの時刻は壁時計に依存するので決定論的ではない。ゲストへ渡した
/// フレームは入力として記録し、記録の再生で同じ状態にする（--net-record）。
pub struct DirectNet {
    stack: Stack,
    conns: BTreeMap<u32, HostConn>,
    tx: mpsc::Sender<HostEvent>,
    rx: mpsc::Receiver<HostEvent>,
    verbose: bool,
}

/// OS に依らない乱数の種（std の HashMap の乱数の種と時刻。暗号用の乱数源ではないが、
/// エミュレータの中の TLS の中継にだけ使う）。
pub fn entropy() -> Vec<u8> {
    use std::hash::{BuildHasher, Hasher};
    let mut out = Vec::new();
    for i in 0..4u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        out.extend_from_slice(&h.finish().to_le_bytes());
    }
    out
}

/// CA のファイルを読む（なければ作って書く）。
pub fn load_or_create_ca(path: &str) -> Result<Ca, String> {
    if let Ok(b) = std::fs::read(path) {
        return Ca::from_bytes(&b).map_err(|e| format!("{path}: {e}"));
    }
    eprintln!("cerulean: creating a new CA in {path} (RSA 2048, may take a few seconds)");
    let ca = Ca::generate(&mut cerulean_net::crypto::Drbg::new(&entropy()));
    std::fs::write(path, ca.to_bytes()).map_err(|e| format!("{path}: {e}"))?;
    let cer = format!("{path}.cer");
    std::fs::write(&cer, &ca.cert).map_err(|e| format!("{cer}: {e}"))?;
    eprintln!("cerulean: wrote the CA certificate to {cer} (install it in WM5)");
    Ok(ca)
}

impl DirectNet {
    pub fn new(verbose: bool, ca: Option<Ca>) -> DirectNet {
        let (tx, rx) = mpsc::channel();
        let mut stack = Stack::new();
        if let Some(ca) = ca {
            stack.set_https(ca, &entropy());
        }
        DirectNet {
            stack,
            conns: BTreeMap::new(),
            tx,
            rx,
            verbose,
        }
    }

    /// ゲストが送ったフレームを渡し、外とのやり取りを進め、ゲストに渡すフレームを返す。
    /// now は仮想時間のミリ秒。
    pub fn step(&mut self, now: u64, from_guest: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        for f in &from_guest {
            self.stack.input(now, f);
        }
        self.stack.poll(now);
        loop {
            self.handle_requests();
            let mut any = false;
            while let Ok(ev) = self.rx.try_recv() {
                any = true;
                self.handle_event(ev);
            }
            self.feed_pending();
            if !any {
                break;
            }
        }
        self.handle_requests();
        self.stack.take_frames()
    }

    fn handle_requests(&mut self) {
        for r in self.stack.take_requests() {
            match r {
                Request::Connect {
                    id,
                    target,
                    port,
                    tls,
                } => {
                    let host = match &target {
                        Target::Ip(ip) => format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
                        Target::Host(h) => h.clone(),
                    };
                    if self.verbose {
                        let t = if tls { " (tls)" } else { "" };
                        eprintln!("cerulean: net: connect #{id} {host}:{port}{t}");
                    }
                    self.conns.insert(
                        id,
                        HostConn {
                            up: None,
                            pending: VecDeque::new(),
                            eof: false,
                        },
                    );
                    let tx = self.tx.clone();
                    upstream::spawn(host, port, tls, None, move |ev| {
                        let _ = tx.send(match ev {
                            Event::Connected(u) => HostEvent::Connected(id, u),
                            Event::Data(d) => HostEvent::Data(id, d),
                            Event::Eof => HostEvent::Eof(id),
                            Event::Reset => HostEvent::Reset(id),
                        });
                    });
                }
                Request::Send { id, data } => {
                    if let Some(u) = self.conns.get(&id).and_then(|c| c.up.as_ref())
                        && u.write(&data).is_err()
                    {
                        u.close();
                        self.conns.remove(&id);
                        self.stack.remote_reset(id);
                    }
                }
                Request::Shutdown { id } => {
                    if let Some(u) = self.conns.get(&id).and_then(|c| c.up.as_ref()) {
                        u.shutdown_write();
                    }
                }
                Request::Close { id } => {
                    if let Some(c) = self.conns.remove(&id)
                        && let Some(u) = c.up
                    {
                        u.close();
                    }
                }
            }
        }
    }

    fn handle_event(&mut self, ev: HostEvent) {
        match ev {
            HostEvent::Connected(id, u) => {
                let Some(c) = self.conns.get_mut(&id) else {
                    if let Some(u) = u {
                        u.close();
                    }
                    return;
                };
                let ok = u.is_some();
                c.up = u;
                if self.verbose {
                    eprintln!(
                        "cerulean: net: #{id} {}",
                        if ok { "connected" } else { "failed" }
                    );
                }
                self.stack.connected(id, ok);
                if !ok {
                    self.conns.remove(&id);
                }
            }
            HostEvent::Data(id, d) => {
                if let Some(c) = self.conns.get_mut(&id) {
                    c.pending.push_back(d);
                }
            }
            HostEvent::Eof(id) => {
                if let Some(c) = self.conns.get_mut(&id) {
                    c.eof = true;
                }
            }
            HostEvent::Reset(id) => {
                if self.conns.remove(&id).is_some() {
                    self.stack.remote_reset(id);
                }
            }
        }
    }

    /// 待たせている外からのデータを、スタックが受け取れるだけ渡す。
    fn feed_pending(&mut self) {
        for (&id, c) in self.conns.iter_mut() {
            while let Some(d) = c.pending.front_mut() {
                let room = self.stack.tx_room(id);
                if room == 0 {
                    break;
                }
                if d.len() <= room {
                    self.stack.recv(id, d);
                    c.pending.pop_front();
                } else {
                    self.stack.recv(id, &d[..room]);
                    d.drain(..room);
                }
            }
            if c.eof && c.pending.is_empty() {
                c.eof = false;
                self.stack.remote_closed(id);
            }
        }
    }
}
