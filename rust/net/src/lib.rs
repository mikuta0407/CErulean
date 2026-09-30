//! ゲストのイーサネットを外へつなぐユーザー空間の NAT（cerulean-net）。
//!
//! ゲスト（WM5 の NE2000）から見ると、同じイーサネットにゲートウェイ（DHCP・DNS の
//! サーバーを兼ねる）が 1 台いるように振る舞う。ゲストの TCP はここで終端し、
//! 中身のバイト列だけを呼び出し側（フロントエンド）に渡す。外への接続は呼び出し側が
//! 作る（CLI は OS のソケット、ブラウザは中継サーバー経由）。コアとは別のクレートで、
//! コアからは使わない。std のみ・プラットフォーム非依存（ソケット・時計に触れない。
//! 時刻は呼び出し側が渡す）。
//!
//! 決定論性: ここで作ったフレームは外から来た入力として命令数つきで記録する（呼び出し
//! 側の責務）。このスタック自体は壁時計や外の応答の時刻に依存してよい。
//!
//! 名前解決: DNS の問い合わせには名前ごとに割り当てた仮の IPv4 アドレス
//! （198.18.0.0/15。RFC 2544 のベンチマーク用の範囲で、インターネットでは使われない）を
//! すぐに返し、そのアドレスへの接続は名前で外に作る（外の名前解決は接続する側が行う）。
//! 名前が存在しないときは、DNS ではなく接続の失敗（RST）としてゲストに見える。
//!
//! 一次資料: RFC 826（ARP）、RFC 791（IPv4）、RFC 792（ICMP）、RFC 768（UDP）、
//! RFC 9293（TCP）、RFC 2131・2132（DHCP）、RFC 1035（DNS）。
//!
//! 対応しないもの: IPv6（ゲストは送ってくるが捨てる）、DNS・DHCP 以外の UDP、外への
//! ICMP、IP の断片、TCP の受け（外からゲストへの接続）。

pub mod bigint;
pub mod crypto;
pub mod tls;
pub mod wire;
pub mod x509;

use std::collections::{BTreeMap, VecDeque};

use crypto::Drbg;
use tls::TlsServer;
use wire::*;
use x509::Ca;

/// ゲストに渡す IPv4 アドレス（DHCP）。
pub const GUEST_IP: [u8; 4] = [10, 0, 2, 15];
/// ゲートウェイ（DHCP サーバーを兼ねる）。
pub const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
/// DNS サーバー。
pub const DNS_IP: [u8; 4] = [10, 0, 2, 3];
pub const NETMASK: [u8; 4] = [255, 255, 255, 0];
/// ゲートウェイの MAC（ローカル管理のユニキャスト。"CRLN" を入れた独自の値）。
pub const GATEWAY_MAC: [u8; 6] = [0x02, 0x43, 0x52, 0x4C, 0x4E, 0xFE];
/// 仮のアドレスの範囲（198.18.0.0/15）。
const FAKE_BASE: u32 = 0xC612_0000;
const FAKE_COUNT: u32 = 1 << 17;
/// DHCP のリース時間（秒）。
const LEASE_SECS: u32 = 86400;
/// DNS の応答の TTL（秒）。仮のアドレスは変わらないので長めでよい。
const DNS_TTL: u32 = 3600;

/// ゲストに送るセグメントの最大長（イーサネットの MTU 1500 − IP 20 − TCP 20）。
const OUR_MSS: u16 = 1460;
/// こちらが広告する受信ウィンドウ（外への送信はすぐ呼び出し側に渡すので固定）。
const OUR_WINDOW: u16 = 32768;
/// ゲストへの送信で、確認応答を待たずに出してよいバイト数の上限。NE2000 の受信
/// リング（ne2000.dll の設定で 52 ページ ≒ 13KB）を溢れさせないための値。
const MAX_IN_FLIGHT: u32 = 8 * 1024;
/// 外から受け取ってまだゲストに確認されていないバイトの上限（呼び出し側はこれを
/// 超えて recv しないこと。tx_room で残りが分かる）。
pub const MAX_BUFFERED: usize = 256 * 1024;
/// 再送の時間（ミリ秒）。初回と上限。
const RTO_MIN: u64 = 500;
const RTO_MAX: u64 = 8000;
/// 再送の回数の上限（超えたら接続を捨てる）。
const MAX_RETRIES: u32 = 10;

/// 外への接続先。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// ゲストが IP アドレスで接続した
    Ip([u8; 4]),
    /// DNS で引いた名前（仮のアドレス）で接続した
    Host(String),
}

/// 呼び出し側への依頼（take_requests で取り出す）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// 外への TCP 接続を作る。結果は connected で返す。tls なら外へは TLS でつなぎ
    /// （接続先の名前で証明書を確かめる）、Send・recv は TLS の中の平文になる
    Connect {
        id: u32,
        target: Target,
        port: u16,
        tls: bool,
    },
    /// 外へ送る
    Send { id: u32, data: Vec<u8> },
    /// ゲストが送り終えた（FIN）。外の接続の送信側を閉じる
    Shutdown { id: u32 },
    /// 接続を捨てる（両方向とも終わった・ゲストのリセット・再送の失敗）
    Close { id: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// ゲストの SYN を受け、外への接続の結果を待っている
    Connecting,
    /// SYN-ACK を送り、ゲストの ACK を待っている
    SynAckSent,
    Established,
}

struct Conn {
    gport: u16,
    rip: [u8; 4],
    rport: u16,
    state: State,
    /// ゲストから次に受け取るシーケンス番号
    rcv_nxt: u32,
    guest_fin: bool,
    /// こちらの初期シーケンス番号（SYN の番号）
    iss: u32,
    /// buf[0] のシーケンス番号（ゲストが確認した位置）
    base: u32,
    /// 次に送るシーケンス番号
    snd_nxt: u32,
    /// 外から受け取り、ゲストがまだ確認していないバイト
    buf: VecDeque<u8>,
    remote_eof: bool,
    fin_sent: bool,
    fin_acked: bool,
    guest_wnd: u32,
    guest_mss: u16,
    /// 再送の期限（None = 待つものがない）
    rto_at: Option<u64>,
    rto: u64,
    retries: u32,
    /// ゲストの TLS をここで終端している（HTTPS の中継）
    tls: Option<Box<TlsServer>>,
    /// ハンドシェイクが済む前に外から届いた平文
    early: Vec<u8>,
    /// このスタック自身が答える HTTP（ゲートウェイの 80 番。CA の配布）の要求の受け
    local: Option<Vec<u8>>,
}

impl Conn {
    fn in_flight(&self) -> u32 {
        self.snd_nxt.wrapping_sub(self.base)
    }

    fn fin_seq(&self) -> u32 {
        self.base.wrapping_add(self.buf.len() as u32)
    }
}

/// スタック本体。
pub struct Stack {
    guest_mac: Option<[u8; 6]>,
    names: Vec<String>,
    by_name: BTreeMap<String, u32>,
    conns: BTreeMap<u32, Conn>,
    /// (ゲストのポート, 相手の IP, 相手のポート) → 接続の番号
    by_tuple: BTreeMap<(u16, [u8; 4], u16), u32>,
    next_id: u32,
    ip_id: u16,
    isn: u32,
    now: u64,
    frames: Vec<Vec<u8>>,
    requests: Vec<Request>,
    https: Option<Https>,
}

/// HTTPS の中継（ゲストの TLS をこちらで終端する）のための CA・証明書。
struct Https {
    ca: Ca,
    rng: Drbg,
    /// サーバー証明書の鍵（全部の名前で共有する。最初の接続で作る）
    leaf: Option<crate::bigint::RsaKey>,
    certs: BTreeMap<String, Vec<u8>>,
}

/// サーバー証明書の鍵の大きさ（ゲストの接続ごとに RSA の復号をするので小さめ）。
const LEAF_BITS: usize = 1024;

impl Default for Stack {
    fn default() -> Self {
        Stack::new()
    }
}

impl Stack {
    pub fn new() -> Stack {
        Stack {
            guest_mac: None,
            names: Vec::new(),
            by_name: BTreeMap::new(),
            conns: BTreeMap::new(),
            by_tuple: BTreeMap::new(),
            next_id: 1,
            ip_id: 0,
            isn: 0x1000_0000,
            now: 0,
            frames: Vec::new(),
            requests: Vec::new(),
            https: None,
        }
    }

    /// HTTPS の中継を有効にする（ca をゲストに信頼させておくこと。seed は呼び出し側の乱数）。
    /// 以後、443 番への接続はゲストの TLS をここで終端し、外へは TLS でつなぎ直す。
    pub fn set_https(&mut self, ca: Ca, seed: &[u8]) {
        self.https = Some(Https {
            ca,
            rng: Drbg::new(seed),
            leaf: None,
            certs: BTreeMap::new(),
        });
    }

    /// 乱数の種を足す（呼び出し側がときどき渡す）。
    pub fn add_entropy(&mut self, extra: &[u8]) {
        if let Some(h) = &mut self.https {
            h.rng.reseed(extra);
        }
    }

    /// ゲストの TLS を受けるサーバーを作る（名前の証明書は作って覚えておく）。
    fn tls_server(&mut self, host: &str) -> Option<Box<TlsServer>> {
        let h = self.https.as_mut()?;
        if h.leaf.is_none() {
            h.leaf = Some(crate::bigint::RsaKey::generate(&mut h.rng, LEAF_BITS));
        }
        let leaf = h.leaf.clone()?;
        if !h.certs.contains_key(host) {
            let c = h.ca.issue(&mut h.rng, host, &leaf);
            h.certs.insert(host.to_string(), c);
        }
        let chain = vec![h.certs[host].clone(), h.ca.cert.clone()];
        Some(Box::new(TlsServer::new(chain, leaf)))
    }

    /// ゲストが送ったフレーム（宛先〜データ）を処理する。now はミリ秒の時刻
    /// （単調増加であればよい）。
    pub fn input(&mut self, now: u64, frame: &[u8]) {
        self.now = now;
        if frame.len() < 14 {
            return;
        }
        let src: [u8; 6] = frame[6..12].try_into().unwrap_or([0; 6]);
        if src[0] & 1 == 0 {
            self.guest_mac = Some(src);
        }
        let payload = &frame[14..];
        match be16(frame, 12) {
            ETH_ARP => self.arp(payload),
            ETH_IPV4 => {
                if let Some(ip) = parse_ipv4(payload) {
                    self.ip(&ip);
                }
            }
            _ => {} // IPv6 等は捨てる
        }
    }

    /// 時間の経過（再送）。now はミリ秒の時刻。
    pub fn poll(&mut self, now: u64) {
        self.now = now;
        let due: Vec<u32> = self
            .conns
            .iter()
            .filter(|(_, c)| c.rto_at.is_some_and(|t| t <= now))
            .map(|(&id, _)| id)
            .collect();
        for id in due {
            self.retransmit(id);
        }
    }

    /// ゲストに渡すフレーム（作った順）。
    pub fn take_frames(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.frames)
    }

    /// 呼び出し側への依頼（出した順）。
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    /// 外への接続の結果。
    pub fn connected(&mut self, id: u32, ok: bool) {
        let Some(c) = self.conns.get_mut(&id) else {
            return;
        };
        if c.state != State::Connecting {
            return;
        }
        if !ok {
            let (rip, rport, gport, ack) = (c.rip, c.rport, c.gport, c.rcv_nxt);
            self.send_tcp(rip, rport, gport, 0, ack, RST | ACK, &[], false);
            self.drop_conn(id, false);
            return;
        }
        c.state = State::SynAckSent;
        self.send_synack(id);
    }

    /// 外から届いたバイト列（ゲストへ送る）。
    pub fn recv(&mut self, id: u32, data: &[u8]) {
        if let Some(c) = self.conns.get_mut(&id)
            && !c.remote_eof
        {
            match &mut c.tls {
                Some(t) if t.is_open() => {
                    t.send(data);
                    c.buf.extend(t.take_output());
                }
                Some(_) => c.early.extend_from_slice(data),
                None => c.buf.extend(data),
            }
            self.push(id);
        }
    }

    /// 外の接続が送り終えた（EOF）。送り残しをゲストに送った後に FIN を送る。
    pub fn remote_closed(&mut self, id: u32) {
        if let Some(c) = self.conns.get_mut(&id) {
            if c.state == State::Connecting {
                self.connected(id, false);
                return;
            }
            if let Some(t) = &mut c.tls {
                t.close();
                c.buf.extend(t.take_output());
            }
            c.remote_eof = true;
            self.push(id);
        }
    }

    /// 外の接続が失敗・切断した（リセット）。
    pub fn remote_reset(&mut self, id: u32) {
        let Some(c) = self.conns.get(&id) else {
            return;
        };
        if c.state == State::Connecting {
            self.connected(id, false);
            return;
        }
        let (rip, rport, gport, seq, ack) = (c.rip, c.rport, c.gport, c.snd_nxt, c.rcv_nxt);
        self.send_tcp(rip, rport, gport, seq, ack, RST | ACK, &[], false);
        self.drop_conn(id, false);
    }

    /// その接続にあと何バイト recv してよいか（0 なら外からの読み出しを止める）。
    pub fn tx_room(&self, id: u32) -> usize {
        self.conns
            .get(&id)
            .map_or(0, |c| MAX_BUFFERED.saturating_sub(c.buf.len()))
    }

    /// 生きている接続の数。
    pub fn connections(&self) -> usize {
        self.conns.len()
    }

    /// 仮のアドレスの名前（呼び出し側の表示用）。
    pub fn name_of(&self, ip: [u8; 4]) -> Option<&str> {
        fake_index(ip).and_then(|i| self.names.get(i as usize).map(String::as_str))
    }

    // ---- ARP（RFC 826）----

    fn arp(&mut self, p: &[u8]) {
        // ハードウェア種別 1（イーサネット）・プロトコル 0800・長さ 6/4・要求（1）
        if p.len() < 28 || be16(p, 0) != 1 || be16(p, 2) != ETH_IPV4 || p[4] != 6 || p[5] != 4 {
            return;
        }
        if be16(p, 6) != 1 {
            return;
        }
        let tpa = ip4(p, 24);
        if tpa != GATEWAY_IP && tpa != DNS_IP {
            return; // ゲスト自身（重複の確認）や他のアドレスには答えない
        }
        let sha: [u8; 6] = p[8..14].try_into().unwrap_or([0; 6]);
        let spa = ip4(p, 14);
        let mut r = Vec::with_capacity(28);
        r.extend_from_slice(&[0, 1, 0x08, 0x00, 6, 4, 0, 2]);
        r.extend_from_slice(&GATEWAY_MAC);
        r.extend_from_slice(&tpa);
        r.extend_from_slice(&sha);
        r.extend_from_slice(&spa);
        self.frames.push(eth(sha, GATEWAY_MAC, ETH_ARP, &r));
    }

    // ---- IPv4 ----

    fn ip(&mut self, ip: &Ipv4<'_>) {
        match ip.proto {
            PROTO_UDP => self.udp(ip),
            PROTO_TCP => {
                if let Some(t) = parse_tcp(ip) {
                    self.tcp(ip.src, ip.dst, &t);
                }
            }
            PROTO_ICMP => self.icmp(ip),
            _ => {}
        }
    }

    fn send_ip(&mut self, src: [u8; 4], dst: [u8; 4], proto: u8, payload: &[u8]) {
        let Some(mac) = self.guest_mac else {
            return;
        };
        self.ip_id = self.ip_id.wrapping_add(1);
        let p = ipv4(src, dst, proto, self.ip_id, payload);
        self.frames.push(eth(mac, GATEWAY_MAC, ETH_IPV4, &p));
    }

    /// ICMP のエコー要求（RFC 792）: ゲートウェイと DNS サーバーだけが答える。
    fn icmp(&mut self, ip: &Ipv4<'_>) {
        let m = ip.payload;
        if m.len() < 8 || m[0] != 8 || checksum(m) != 0 {
            return;
        }
        if ip.dst != GATEWAY_IP && ip.dst != DNS_IP {
            return;
        }
        let mut r = m.to_vec();
        r[0] = 0;
        r[2] = 0;
        r[3] = 0;
        let c = checksum(&r);
        r[2..4].copy_from_slice(&c.to_be_bytes());
        self.send_ip(ip.dst, ip.src, PROTO_ICMP, &r);
    }

    // ---- UDP: DHCP・DNS ----

    fn udp(&mut self, ip: &Ipv4<'_>) {
        let u = ip.payload;
        if u.len() < 8 {
            return;
        }
        let len = be16(u, 4) as usize;
        if len < 8 || len > u.len() {
            return;
        }
        if be16(u, 6) != 0 && l4_checksum(ip.src, ip.dst, PROTO_UDP, &u[..len]) != 0 {
            return;
        }
        let (sport, dport, data) = (be16(u, 0), be16(u, 2), &u[8..len]);
        if dport == 67 {
            self.dhcp(data);
        } else if dport == 53
            && ip.dst == DNS_IP
            && let Some(r) = self.dns(data)
        {
            let d = udp(DNS_IP, 53, ip.src, sport, &r);
            self.send_ip(DNS_IP, ip.src, PROTO_UDP, &d);
        }
    }

    /// DHCP（RFC 2131・2132）: DISCOVER に OFFER、REQUEST に ACK を返す。
    fn dhcp(&mut self, m: &[u8]) {
        // 固定部 236 バイト＋マジッククッキー（RFC 2131 図 1・3 章）
        if m.len() < 240 || m[0] != 1 || m[1] != 1 || m[2] != 6 || m[236..240] != [99, 130, 83, 99]
        {
            return;
        }
        let mut msg_type = 0;
        let mut i = 240;
        while i < m.len() {
            match m[i] {
                0 => i += 1,
                255 => break,
                code => {
                    if i + 1 >= m.len() {
                        break;
                    }
                    let len = m[i + 1] as usize;
                    if i + 2 + len > m.len() {
                        break;
                    }
                    if code == 53 && len == 1 {
                        msg_type = m[i + 2];
                    }
                    i += 2 + len;
                }
            }
        }
        let reply = match msg_type {
            1 => 2, // DISCOVER → OFFER
            3 => 5, // REQUEST → ACK
            8 => 5, // INFORM → ACK
            _ => return,
        };
        let mut r = vec![0u8; 240];
        r[0] = 2; // BOOTREPLY
        r[1] = 1;
        r[2] = 6;
        r[4..8].copy_from_slice(&m[4..8]); // xid
        r[10..12].copy_from_slice(&m[10..12]); // flags
        r[12..16].copy_from_slice(&m[12..16]); // ciaddr
        if msg_type != 8 {
            r[16..20].copy_from_slice(&GUEST_IP); // yiaddr
        }
        r[20..24].copy_from_slice(&GATEWAY_IP); // siaddr
        r[28..44].copy_from_slice(&m[28..44]); // chaddr
        r[236..240].copy_from_slice(&[99, 130, 83, 99]);
        let mut opt = |code: u8, v: &[u8]| {
            r.push(code);
            r.push(v.len() as u8);
            r.extend_from_slice(v);
        };
        opt(53, &[reply]);
        opt(54, &GATEWAY_IP);
        if msg_type != 8 {
            opt(51, &LEASE_SECS.to_be_bytes());
            opt(58, &(LEASE_SECS / 2).to_be_bytes());
            opt(59, &(LEASE_SECS / 8 * 7).to_be_bytes());
        }
        opt(1, &NETMASK);
        opt(3, &GATEWAY_IP);
        opt(6, &DNS_IP);
        r.push(255);
        // BOOTP の最小長（300 バイト）に満たなければ詰める（RFC 2131 2 章・RFC 951）
        if r.len() < 300 {
            r.resize(300, 0);
        }
        // 宛先: ブロードキャストのフラグが立っていればブロードキャスト、でなければ
        // クライアントのハードウェアアドレスと割り当てるアドレス（RFC 2131 4.1）
        let chaddr: [u8; 6] = m[28..34].try_into().unwrap_or([0xFF; 6]);
        let (dmac, dip) = if m[10] & 0x80 != 0 {
            ([0xFF; 6], [255; 4])
        } else if msg_type == 8 {
            (chaddr, ip4(m, 12))
        } else {
            (chaddr, GUEST_IP)
        };
        let d = udp(GATEWAY_IP, 67, dip, 68, &r);
        self.ip_id = self.ip_id.wrapping_add(1);
        let p = ipv4(GATEWAY_IP, dip, PROTO_UDP, self.ip_id, &d);
        self.frames.push(eth(dmac, GATEWAY_MAC, ETH_IPV4, &p));
    }

    /// DNS（RFC 1035）: A の問い合わせに仮のアドレスを返す。他の種類は答えなし
    /// （NOERROR・回答 0 件）。
    fn dns(&mut self, q: &[u8]) -> Option<Vec<u8>> {
        if q.len() < 12 || q[2] & 0x80 != 0 {
            return None; // 応答は無視
        }
        let opcode = (q[2] >> 3) & 0x0F;
        let qdcount = be16(q, 4);
        let mut r = Vec::with_capacity(q.len() + 16);
        r.extend_from_slice(&q[0..2]);
        // QR=1・同じ OPCODE・RD を写す・RA=1
        r.push(0x80 | opcode << 3 | q[2] & 0x01);
        r.push(0x80);
        if opcode != 0 || qdcount != 1 {
            r[3] |= 4; // NOTIMP
            r.extend_from_slice(&[0; 8]);
            return Some(r);
        }
        // 問い合わせの名前（圧縮なし）
        let mut i = 12;
        let mut labels = Vec::new();
        loop {
            let len = *q.get(i)? as usize;
            if len == 0 {
                i += 1;
                break;
            }
            if len > 63 {
                return None;
            }
            let l = q.get(i + 1..i + 1 + len)?;
            labels.push(String::from_utf8_lossy(l).to_ascii_lowercase());
            i += 1 + len;
        }
        let qend = i + 4;
        let question = q.get(12..qend)?;
        let (qtype, qclass) = (be16(q, i), be16(q, i + 2));
        let name = labels.join(".");
        // 点を含まない名前（isatap・wpad 等。WM5 が起動時・接続時に引く）は存在しない
        // （NXDOMAIN）と答える。仮のアドレスを返すと、ゲストが ISATAP のトンネルや
        // プロキシの自動検出に使おうとする（2026-09-30 観察）。
        if !name.contains('.') {
            r[3] |= 3;
        }
        let answer = if qtype == 1 && qclass == 1 && name.contains('.') {
            self.fake_ip(&name)
        } else {
            None
        };
        r.extend_from_slice(&1u16.to_be_bytes());
        r.extend_from_slice(&(answer.is_some() as u16).to_be_bytes());
        r.extend_from_slice(&[0, 0, 0, 0]);
        r.extend_from_slice(question);
        if let Some(a) = answer {
            r.extend_from_slice(&[0xC0, 12]); // 問い合わせの名前への圧縮ポインタ
            r.extend_from_slice(&[0, 1, 0, 1]);
            r.extend_from_slice(&DNS_TTL.to_be_bytes());
            r.extend_from_slice(&[0, 4]);
            r.extend_from_slice(&a);
        }
        Some(r)
    }

    /// 名前に仮のアドレスを割り当てる（同じ名前には同じアドレス）。
    fn fake_ip(&mut self, name: &str) -> Option<[u8; 4]> {
        let idx = match self.by_name.get(name) {
            Some(&i) => i,
            None => {
                let i = self.names.len() as u32;
                if i >= FAKE_COUNT - 2 {
                    return None;
                }
                self.names.push(name.to_string());
                self.by_name.insert(name.to_string(), i);
                i
            }
        };
        Some((FAKE_BASE + 1 + idx).to_be_bytes())
    }

    // ---- TCP（RFC 9293）----

    fn tcp(&mut self, gip: [u8; 4], rip: [u8; 4], t: &Tcp<'_>) {
        if gip != GUEST_IP {
            return;
        }
        let key = (t.sport, rip, t.dport);
        let Some(&id) = self.by_tuple.get(&key) else {
            self.tcp_new(rip, t);
            return;
        };
        if t.flags & RST != 0 {
            self.drop_conn(id, true);
            return;
        }
        let c = self
            .conns
            .get_mut(&id)
            .expect("tuple points to a live conn");
        if t.flags & SYN != 0 {
            // SYN の再送: SYN-ACK を送り直す（接続の結果待ちなら何もしない）
            if c.state == State::SynAckSent {
                self.send_synack(id);
            }
            return;
        }
        if t.flags & ACK == 0 {
            return;
        }
        // 確認応答
        c.guest_wnd = t.window as u32;
        if c.state == State::SynAckSent && t.ack == c.iss.wrapping_add(1) {
            c.state = State::Established;
            c.rto_at = None;
            c.retries = 0;
            c.rto = RTO_MIN;
        }
        if c.state == State::Established {
            let acked = t.ack.wrapping_sub(c.base);
            let limit = c.in_flight() + if c.fin_sent { 1 } else { 0 };
            if acked > 0 && acked <= limit {
                let n = (acked as usize).min(c.buf.len());
                c.buf.drain(..n);
                c.base = c.base.wrapping_add(n as u32);
                if c.fin_sent && t.ack == c.fin_seq().wrapping_add(1) {
                    c.fin_acked = true;
                }
                c.retries = 0;
                c.rto = RTO_MIN;
                c.rto_at = None;
            }
        }
        if c.state != State::Established {
            return;
        }
        // データ・FIN（順番どおりのものだけ受け取る。順番外は確認応答だけ返す）
        let mut ack_needed = !t.data.is_empty() || t.flags & FIN != 0;
        if t.seq == c.rcv_nxt && !c.guest_fin {
            if !t.data.is_empty() {
                c.rcv_nxt = c.rcv_nxt.wrapping_add(t.data.len() as u32);
                if !self.guest_data(id, t.data) {
                    return;
                }
            }
            let c = self.conns.get_mut(&id).expect("live");
            if t.flags & FIN != 0 {
                c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
                c.guest_fin = true;
                self.requests.push(Request::Shutdown { id });
            }
        } else if t.data.is_empty() && t.flags & FIN == 0 {
            ack_needed = false;
        }
        if ack_needed {
            self.send_ack(id);
        }
        self.push(id);
        self.maybe_finish(id);
    }

    /// 順番どおりに届いたゲストのデータ。false なら接続を捨てた。
    fn guest_data(&mut self, id: u32, data: &[u8]) -> bool {
        let https = &mut self.https;
        let c = self.conns.get_mut(&id).expect("live");
        if let Some(req) = &mut c.local {
            req.extend_from_slice(data);
            if req.windows(4).any(|w| w == b"\r\n\r\n") || req.len() > 8192 {
                let resp = local_http(req, https.as_ref().map(|h| &h.ca));
                c.local = Some(Vec::new());
                c.buf.extend(resp);
                c.remote_eof = true;
            }
            return true;
        }
        let Some(tls) = &mut c.tls else {
            self.requests.push(Request::Send {
                id,
                data: data.to_vec(),
            });
            return true;
        };
        let Some(h) = https else {
            return true;
        };
        let was_open = tls.is_open();
        let r = tls.input(data, &mut h.rng);
        c.buf.extend(tls.take_output());
        match r {
            Ok(plain) => {
                if !was_open && tls.is_open() && !c.early.is_empty() {
                    let e = std::mem::take(&mut c.early);
                    tls.send(&e);
                    c.buf.extend(tls.take_output());
                }
                if !plain.is_empty() {
                    self.requests.push(Request::Send { id, data: plain });
                }
                true
            }
            Err(_) => {
                // alert は送った（buf に入れた）。送ってから切るのは省き、リセットする
                let (rip, rport, gport, seq, ack) = (c.rip, c.rport, c.gport, c.snd_nxt, c.rcv_nxt);
                self.send_tcp(rip, rport, gport, seq, ack, RST | ACK, &[], false);
                self.drop_conn(id, true);
                false
            }
        }
    }

    fn tcp_new(&mut self, rip: [u8; 4], t: &Tcp<'_>) {
        if t.flags & RST != 0 {
            return;
        }
        if t.flags & (SYN | ACK) != SYN {
            // 知らない接続: リセットを返す（RFC 9293 3.10.7.1）
            let (seq, ack, fl) = if t.flags & ACK != 0 {
                (t.ack, 0, RST)
            } else {
                let len = t.data.len() as u32 + (t.flags & FIN != 0) as u32;
                (0, t.seq.wrapping_add(len), RST | ACK)
            };
            self.send_tcp(rip, t.dport, t.sport, seq, ack, fl, &[], false);
            return;
        }
        // ゲートウェイの 80 番はこのスタック自身が答える（CA の配布）
        let local = rip == GATEWAY_IP && t.dport == 80;
        let target = match fake_index(rip) {
            Some(i) => match self.names.get(i as usize) {
                Some(n) => Target::Host(n.clone()),
                None => {
                    let ack = t.seq.wrapping_add(1);
                    self.send_tcp(rip, t.dport, t.sport, 0, ack, RST | ACK, &[], false);
                    return;
                }
            },
            None => Target::Ip(rip),
        };
        let tls = if t.dport == 443 && !local {
            let host = match &target {
                Target::Host(h) => h.clone(),
                Target::Ip(a) => format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]),
            };
            self.tls_server(&host)
        } else {
            None
        };
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.isn = self
            .isn
            .wrapping_add(0x0001_0000 + (self.now as u32 & 0xFFFF));
        let iss = self.isn;
        self.conns.insert(
            id,
            Conn {
                gport: t.sport,
                rip,
                rport: t.dport,
                state: State::Connecting,
                rcv_nxt: t.seq.wrapping_add(1),
                guest_fin: false,
                iss,
                base: iss.wrapping_add(1),
                snd_nxt: iss.wrapping_add(1),
                buf: VecDeque::new(),
                remote_eof: false,
                fin_sent: false,
                fin_acked: false,
                guest_wnd: t.window as u32,
                guest_mss: t.mss.unwrap_or(536).clamp(64, OUR_MSS),
                rto_at: None,
                rto: RTO_MIN,
                retries: 0,
                tls: None,
                early: Vec::new(),
                local: local.then(Vec::new),
            },
        );
        self.by_tuple.insert((t.sport, rip, t.dport), id);
        if local {
            self.connected(id, true);
            return;
        }
        let tls_on = tls.is_some();
        self.conns.get_mut(&id).expect("live").tls = tls;
        self.requests.push(Request::Connect {
            id,
            target,
            port: t.dport,
            tls: tls_on,
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn send_tcp(
        &mut self,
        rip: [u8; 4],
        rport: u16,
        gport: u16,
        seq: u32,
        ack: u32,
        flags: u8,
        data: &[u8],
        syn_mss: bool,
    ) {
        let mss = syn_mss.then_some(OUR_MSS);
        let seg = tcp(
            rip, rport, GUEST_IP, gport, seq, ack, flags, OUR_WINDOW, mss, data,
        );
        self.send_ip(rip, GUEST_IP, PROTO_TCP, &seg);
    }

    fn send_synack(&mut self, id: u32) {
        let c = &self.conns[&id];
        let (rip, rport, gport, iss, ack) = (c.rip, c.rport, c.gport, c.iss, c.rcv_nxt);
        self.send_tcp(rip, rport, gport, iss, ack, SYN | ACK, &[], true);
        let now = self.now;
        let c = self.conns.get_mut(&id).expect("live");
        c.rto_at = Some(now + c.rto);
    }

    fn send_ack(&mut self, id: u32) {
        let c = &self.conns[&id];
        let (rip, rport, gport, seq, ack) = (c.rip, c.rport, c.gport, c.snd_nxt, c.rcv_nxt);
        let seq = if c.fin_sent { seq.wrapping_add(1) } else { seq };
        self.send_tcp(rip, rport, gport, seq, ack, ACK, &[], false);
    }

    /// 送れるだけ送る（ゲストのウィンドウ・MAX_IN_FLIGHT の範囲で）。送り終えて外が
    /// EOF なら FIN を送る。
    fn push(&mut self, id: u32) {
        loop {
            let Some(c) = self.conns.get(&id) else {
                return;
            };
            if c.state != State::Established {
                return;
            }
            let sent = c.in_flight() as usize;
            let window = c.guest_wnd.min(MAX_IN_FLIGHT) as usize;
            if sent < c.buf.len() && sent < window {
                let n = (c.buf.len() - sent)
                    .min(c.guest_mss as usize)
                    .min(window - sent);
                let data: Vec<u8> = c.buf.range(sent..sent + n).copied().collect();
                let (rip, rport, gport, seq, ack) = (c.rip, c.rport, c.gport, c.snd_nxt, c.rcv_nxt);
                let last = sent + n == c.buf.len();
                let fl = ACK | if last { PSH } else { 0 };
                self.send_tcp(rip, rport, gport, seq, ack, fl, &data, false);
                let now = self.now;
                let c = self.conns.get_mut(&id).expect("live");
                c.snd_nxt = c.snd_nxt.wrapping_add(n as u32);
                if c.rto_at.is_none() {
                    c.rto_at = Some(now + c.rto);
                }
                continue;
            }
            if c.remote_eof && !c.fin_sent && sent == c.buf.len() {
                let (rip, rport, gport, seq, ack) = (c.rip, c.rport, c.gport, c.snd_nxt, c.rcv_nxt);
                self.send_tcp(rip, rport, gport, seq, ack, FIN | ACK, &[], false);
                let now = self.now;
                let c = self.conns.get_mut(&id).expect("live");
                c.fin_sent = true;
                if c.rto_at.is_none() {
                    c.rto_at = Some(now + c.rto);
                }
            }
            return;
        }
    }

    fn retransmit(&mut self, id: u32) {
        let Some(c) = self.conns.get_mut(&id) else {
            return;
        };
        c.retries += 1;
        if c.retries > MAX_RETRIES {
            let (rip, rport, gport, seq, ack) = (c.rip, c.rport, c.gport, c.snd_nxt, c.rcv_nxt);
            self.send_tcp(rip, rport, gport, seq, ack, RST | ACK, &[], false);
            self.drop_conn(id, true);
            return;
        }
        c.rto = (c.rto * 2).min(RTO_MAX);
        c.rto_at = None;
        match c.state {
            State::Connecting => {}
            State::SynAckSent => self.send_synack(id),
            State::Established => {
                // 確認されていないところから送り直す（go-back-N）
                c.snd_nxt = c.base;
                c.fin_sent = false;
                self.push(id);
                if self.conns.get(&id).is_some_and(|c| c.rto_at.is_none()) {
                    // 送るものがなかった（ウィンドウ 0）: ウィンドウの確認を続ける
                    let now = self.now;
                    let c = self.conns.get_mut(&id).expect("live");
                    if !c.buf.is_empty() || c.remote_eof && !c.fin_acked {
                        c.rto_at = Some(now + c.rto);
                        let (rip, rport, gport, seq, ack) =
                            (c.rip, c.rport, c.gport, c.snd_nxt, c.rcv_nxt);
                        self.send_tcp(rip, rport, gport, seq, ack, ACK, &[], false);
                    }
                }
            }
        }
    }

    /// 両方向が終わった接続を片付ける。
    fn maybe_finish(&mut self, id: u32) {
        if let Some(c) = self.conns.get(&id)
            && c.guest_fin
            && c.fin_acked
        {
            self.drop_conn(id, true);
        }
    }

    fn drop_conn(&mut self, id: u32, tell: bool) {
        if let Some(c) = self.conns.remove(&id) {
            self.by_tuple.remove(&(c.gport, c.rip, c.rport));
            if tell && c.local.is_none() {
                self.requests.push(Request::Close { id });
            }
        }
    }
}

/// ゲートウェイの 80 番の応答（HTTP/1.0）: CA の証明書の配布と説明のページ。
fn local_http(req: &[u8], ca: Option<&Ca>) -> Vec<u8> {
    let line = req.split(|&b| b == b'\r').next().unwrap_or(&[]);
    let path = line.split(|&b| b == b' ').nth(1).unwrap_or(b"/");
    let (status, ctype, body): (&str, &str, Vec<u8>) = match (path, ca) {
        (b"/cerulean-ca.cer", Some(ca)) => {
            ("200 OK", "application/x-x509-ca-cert", ca.cert.clone())
        }
        (b"/", _) => {
            let msg = if ca.is_some() {
                "<p><a href=\"/cerulean-ca.cer\">cerulean-ca.cer</a></p>\
                 <p>HTTPS のサイトを開くには、この証明書（CErulean Local CA）を開いて\
                 インストールしてください（最初の 1 回だけ）。</p>\
                 <p>To open HTTPS sites, open and install this certificate once.</p>\
                 <p>確認用 (test): <a href=\"https://example.com/\">https://example.com/</a></p>"
            } else {
                "<p>HTTPS の中継は無効です（HTTPS relay is off）。</p>"
            };
            (
                "200 OK",
                "text/html; charset=utf-8",
                format!(
                    "<html><head><title>CErulean</title></head><body><h3>CErulean</h3>{msg}</body></html>"
                )
                .into_bytes(),
            )
        }
        _ => ("404 Not Found", "text/plain", b"not found".to_vec()),
    };
    let mut r = format!(
        "HTTP/1.0 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    r.extend_from_slice(&body);
    r
}

fn fake_index(ip: [u8; 4]) -> Option<u32> {
    let v = u32::from_be_bytes(ip);
    (v > FAKE_BASE && v < FAKE_BASE + FAKE_COUNT).then(|| v - FAKE_BASE - 1)
}

#[cfg(test)]
mod tests;
