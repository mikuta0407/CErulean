//! フレーム・パケットの読み書き（イーサネット II・IPv4・UDP・TCP）とチェックサム。
//!
//! 一次資料: RFC 791（IPv4）、RFC 768（UDP）、RFC 9293（TCP）、RFC 1071（インター
//! ネットのチェックサム）、RFC 826（ARP）。

pub const ETH_IPV4: u16 = 0x0800;
pub const ETH_ARP: u16 = 0x0806;
pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;

pub fn be16(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([b[i], b[i + 1]])
}

pub fn be32(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

pub fn ip4(b: &[u8], i: usize) -> [u8; 4] {
    [b[i], b[i + 1], b[i + 2], b[i + 3]]
}

/// 1 の補数の和（RFC 1071）。畳み込む前の 32 ビットの和を返す。
fn sum16(data: &[u8], mut acc: u32) -> u32 {
    let mut i = 0;
    while i + 1 < data.len() {
        acc += be16(data, i) as u32;
        i += 2;
    }
    if i < data.len() {
        acc += (data[i] as u32) << 8;
    }
    acc
}

fn fold(mut acc: u32) -> u16 {
    while acc >> 16 != 0 {
        acc = (acc & 0xFFFF) + (acc >> 16);
    }
    !(acc as u16)
}

pub fn checksum(data: &[u8]) -> u16 {
    fold(sum16(data, 0))
}

/// TCP・UDP のチェックサム（疑似ヘッダ込み。RFC 768・RFC 9293 3.1）。
pub fn l4_checksum(src: [u8; 4], dst: [u8; 4], proto: u8, seg: &[u8]) -> u16 {
    let mut acc = sum16(&src, 0);
    acc = sum16(&dst, acc);
    acc += proto as u32;
    acc += seg.len() as u32;
    fold(sum16(seg, acc))
}

/// 受け取った IPv4 パケット。
pub struct Ipv4<'a> {
    pub src: [u8; 4],
    pub dst: [u8; 4],
    pub proto: u8,
    pub payload: &'a [u8],
}

/// IPv4 のヘッダを読む。壊れたもの・断片（フラグメント）は None。
/// TODO: 断片の再組み立ては未実装（MTU 1500 のイーサネットでゲストが断片を送るのは
/// 1472 バイトを超える UDP だけで、DNS・DHCP では起きない）。
pub fn parse_ipv4(p: &[u8]) -> Option<Ipv4<'_>> {
    if p.len() < 20 || p[0] >> 4 != 4 {
        return None;
    }
    let ihl = (p[0] & 0x0F) as usize * 4;
    let total = be16(p, 2) as usize;
    if ihl < 20 || total < ihl || total > p.len() {
        return None;
    }
    if checksum(&p[..ihl]) != 0 {
        return None;
    }
    let frag = be16(p, 6);
    if frag & 0x2000 != 0 || frag & 0x1FFF != 0 {
        return None; // MF または断片のオフセット
    }
    Some(Ipv4 {
        src: ip4(p, 12),
        dst: ip4(p, 16),
        proto: p[9],
        payload: &p[ihl..total],
    })
}

/// イーサネット II のフレームを作る。
pub fn eth(dst: [u8; 6], src: [u8; 6], ty: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(14 + payload.len());
    f.extend_from_slice(&dst);
    f.extend_from_slice(&src);
    f.extend_from_slice(&ty.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

/// IPv4 のパケットを作る（オプションなし・DF なし・TTL 64）。
pub fn ipv4(src: [u8; 4], dst: [u8; 4], proto: u8, id: u16, payload: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(20 + payload.len());
    p.push(0x45);
    p.push(0);
    p.extend_from_slice(&((20 + payload.len()) as u16).to_be_bytes());
    p.extend_from_slice(&id.to_be_bytes());
    p.extend_from_slice(&[0, 0, 64, proto, 0, 0]);
    p.extend_from_slice(&src);
    p.extend_from_slice(&dst);
    let c = checksum(&p[..20]);
    p[10..12].copy_from_slice(&c.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

/// UDP のデータグラムを作る（チェックサム付き）。
pub fn udp(src: [u8; 4], sport: u16, dst: [u8; 4], dport: u16, data: &[u8]) -> Vec<u8> {
    let mut u = Vec::with_capacity(8 + data.len());
    u.extend_from_slice(&sport.to_be_bytes());
    u.extend_from_slice(&dport.to_be_bytes());
    u.extend_from_slice(&((8 + data.len()) as u16).to_be_bytes());
    u.extend_from_slice(&[0, 0]);
    u.extend_from_slice(data);
    let mut c = l4_checksum(src, dst, PROTO_UDP, &u);
    if c == 0 {
        c = 0xFFFF; // 0 は「チェックサムなし」の意味になるので全 1 で送る（RFC 768）
    }
    u[6..8].copy_from_slice(&c.to_be_bytes());
    u
}

// TCP のフラグ（RFC 9293 3.1）
pub const FIN: u8 = 0x01;
pub const SYN: u8 = 0x02;
pub const RST: u8 = 0x04;
pub const PSH: u8 = 0x08;
pub const ACK: u8 = 0x10;

/// 受け取った TCP のセグメント。
pub struct Tcp<'a> {
    pub sport: u16,
    pub dport: u16,
    pub seq: u32,
    pub ack: u32,
    pub flags: u8,
    pub window: u16,
    /// SYN の MSS オプション
    pub mss: Option<u16>,
    pub data: &'a [u8],
}

pub fn parse_tcp<'a>(ip: &Ipv4<'a>) -> Option<Tcp<'a>> {
    let s = ip.payload;
    if s.len() < 20 || l4_checksum(ip.src, ip.dst, PROTO_TCP, s) != 0 {
        return None;
    }
    let off = (s[12] >> 4) as usize * 4;
    if off < 20 || off > s.len() {
        return None;
    }
    // オプション（RFC 9293 3.1: 0 = 終わり、1 = NOP、他は種類・長さ・値）
    let mut mss = None;
    let mut i = 20;
    while i < off {
        match s[i] {
            0 => break,
            1 => i += 1,
            kind => {
                if i + 1 >= off {
                    break;
                }
                let len = s[i + 1] as usize;
                if len < 2 || i + len > off {
                    break;
                }
                if kind == 2 && len == 4 {
                    mss = Some(be16(s, i + 2));
                }
                i += len;
            }
        }
    }
    Some(Tcp {
        sport: be16(s, 0),
        dport: be16(s, 2),
        seq: be32(s, 4),
        ack: be32(s, 8),
        flags: s[13],
        window: be16(s, 14),
        mss,
        data: &s[off..],
    })
}

/// TCP のセグメントを作る（チェックサム付き）。mss を渡すと MSS オプションを付ける。
#[allow(clippy::too_many_arguments)]
pub fn tcp(
    src: [u8; 4],
    sport: u16,
    dst: [u8; 4],
    dport: u16,
    seq: u32,
    ack: u32,
    flags: u8,
    window: u16,
    mss: Option<u16>,
    data: &[u8],
) -> Vec<u8> {
    let hlen = if mss.is_some() { 24 } else { 20 };
    let mut t = Vec::with_capacity(hlen + data.len());
    t.extend_from_slice(&sport.to_be_bytes());
    t.extend_from_slice(&dport.to_be_bytes());
    t.extend_from_slice(&seq.to_be_bytes());
    t.extend_from_slice(&ack.to_be_bytes());
    t.push(((hlen / 4) as u8) << 4);
    t.push(flags);
    t.extend_from_slice(&window.to_be_bytes());
    t.extend_from_slice(&[0, 0, 0, 0]);
    if let Some(m) = mss {
        t.extend_from_slice(&[2, 4]);
        t.extend_from_slice(&m.to_be_bytes());
    }
    t.extend_from_slice(data);
    let c = l4_checksum(src, dst, PROTO_TCP, &t);
    t[16..18].copy_from_slice(&c.to_be_bytes());
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 1071 の例（00 01 f2 03 f4 f5 f6 f7 の和は ddf2、補数は 220d）。
    #[test]
    fn checksum_rfc1071_example() {
        assert_eq!(
            checksum(&[0x00, 0x01, 0xF2, 0x03, 0xF4, 0xF5, 0xF6, 0xF7]),
            0x220D
        );
    }

    #[test]
    fn ipv4_and_tcp_round_trip() {
        let a = [10, 0, 2, 15];
        let b = [198, 18, 0, 1];
        let seg = tcp(a, 1234, b, 80, 100, 200, SYN | ACK, 4096, Some(1460), b"hi");
        let pkt = ipv4(a, b, PROTO_TCP, 7, &seg);
        let ip = parse_ipv4(&pkt).unwrap();
        assert_eq!((ip.src, ip.dst, ip.proto), (a, b, PROTO_TCP));
        let t = parse_tcp(&ip).unwrap();
        assert_eq!((t.sport, t.dport, t.seq, t.ack), (1234, 80, 100, 200));
        assert_eq!(
            (t.flags, t.window, t.mss, t.data),
            (SYN | ACK, 4096, Some(1460), &b"hi"[..])
        );
    }
}
