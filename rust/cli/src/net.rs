//! ネットワーク（イーサネットカード）の CLI 側: フレームの pcap への書き出しと、
//! OS のソケットで外へつなぐ経路（cerulean-net のスタックを使う）。

use std::collections::{BTreeMap, VecDeque};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::mpsc;
use std::time::Duration;

use cerulean_core::smdk2410::INSTRUCTIONS_PER_SECOND;
use cerulean_net::{Request, Stack, Target};

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

/// 外への接続を作る時間の上限。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

enum HostEvent {
    Connected(u32, Option<TcpStream>),
    Data(u32, Vec<u8>),
    Eof(u32),
    Reset(u32),
}

struct HostConn {
    /// 書き込み側（接続できるまで None）
    stream: Option<TcpStream>,
    /// スタックが受け取れずに待たせている外からのデータ
    pending: VecDeque<Vec<u8>>,
    eof: bool,
}

/// OS のソケットで直接外へつなぐ（CLI 用。中継サーバーを使わない）。接続・読み出しは
/// 接続ごとのスレッドで行い、結果はチャネルで受け取る。
/// 外とのやり取りの時刻は壁時計に依存するので決定論的ではない。ゲストへ渡した
/// フレームは入力として記録し、記録の再生で同じ状態にする（--net-record）。
pub struct DirectNet {
    stack: Stack,
    conns: BTreeMap<u32, HostConn>,
    tx: mpsc::Sender<HostEvent>,
    rx: mpsc::Receiver<HostEvent>,
    verbose: bool,
}

impl DirectNet {
    pub fn new(verbose: bool) -> DirectNet {
        let (tx, rx) = mpsc::channel();
        DirectNet {
            stack: Stack::new(),
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
                Request::Connect { id, target, port } => {
                    let host = match &target {
                        Target::Ip(ip) => format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3]),
                        Target::Host(h) => h.clone(),
                    };
                    if self.verbose {
                        eprintln!("cerulean: net: connect #{id} {host}:{port}");
                    }
                    self.conns.insert(
                        id,
                        HostConn {
                            stream: None,
                            pending: VecDeque::new(),
                            eof: false,
                        },
                    );
                    let tx = self.tx.clone();
                    std::thread::spawn(move || connect_thread(id, host, port, tx));
                }
                Request::Send { id, data } => {
                    if let Some(s) = self.conns.get_mut(&id).and_then(|c| c.stream.as_mut()) {
                        // TODO: 書き込みは相手が受け取るまで止まり得る（開発用の CLI なので
                        // 簡単にしている）
                        if s.write_all(&data).is_err() {
                            self.stack.remote_reset(id);
                        }
                    }
                }
                Request::Shutdown { id } => {
                    if let Some(s) = self.conns.get(&id).and_then(|c| c.stream.as_ref()) {
                        let _ = s.shutdown(Shutdown::Write);
                    }
                }
                Request::Close { id } => {
                    if let Some(c) = self.conns.remove(&id)
                        && let Some(s) = c.stream
                    {
                        let _ = s.shutdown(Shutdown::Both);
                    }
                }
            }
        }
    }

    fn handle_event(&mut self, ev: HostEvent) {
        match ev {
            HostEvent::Connected(id, s) => {
                let Some(c) = self.conns.get_mut(&id) else {
                    if let Some(s) = s {
                        let _ = s.shutdown(Shutdown::Both);
                    }
                    return;
                };
                let ok = s.is_some();
                c.stream = s;
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

fn connect_thread(id: u32, host: String, port: u16, tx: mpsc::Sender<HostEvent>) {
    let addrs: Vec<_> = match (host.as_str(), port).to_socket_addrs() {
        Ok(a) => a.filter(|a| a.is_ipv4()).collect(),
        Err(_) => Vec::new(),
    };
    let stream = addrs
        .iter()
        .find_map(|a| TcpStream::connect_timeout(a, CONNECT_TIMEOUT).ok());
    let Some(stream) = stream else {
        let _ = tx.send(HostEvent::Connected(id, None));
        return;
    };
    let _ = stream.set_nodelay(true);
    let mut reader = match stream.try_clone() {
        Ok(r) => r,
        Err(_) => {
            let _ = tx.send(HostEvent::Connected(id, None));
            return;
        }
    };
    if tx.send(HostEvent::Connected(id, Some(stream))).is_err() {
        return;
    }
    let mut buf = vec![0u8; 16 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(HostEvent::Eof(id));
                return;
            }
            Ok(n) => {
                if tx.send(HostEvent::Data(id, buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(_) => {
                let _ = tx.send(HostEvent::Reset(id));
                return;
            }
        }
    }
}
