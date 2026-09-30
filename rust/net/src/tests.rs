use super::*;

const GMAC: [u8; 6] = [0x02, 0x43, 0x52, 0x4C, 0x4E, 0x01];

fn guest_ip_frame(dst: [u8; 4], proto: u8, payload: &[u8]) -> Vec<u8> {
    eth(
        GATEWAY_MAC,
        GMAC,
        ETH_IPV4,
        &ipv4(GUEST_IP, dst, proto, 1, payload),
    )
}

fn guest_tcp(
    dst: [u8; 4],
    sport: u16,
    dport: u16,
    seq: u32,
    ack: u32,
    fl: u8,
    d: &[u8],
) -> Vec<u8> {
    let seg = tcp(
        GUEST_IP,
        sport,
        dst,
        dport,
        seq,
        ack,
        fl,
        16384,
        (fl & SYN != 0).then_some(1460),
        d,
    );
    guest_ip_frame(dst, PROTO_TCP, &seg)
}

/// ゲストに届いたフレームを TCP として読む。
fn as_tcp(f: &[u8]) -> (u32, u32, u8, Vec<u8>) {
    assert_eq!(&f[0..6], &GMAC);
    let ip = parse_ipv4(&f[14..]).expect("ipv4");
    let t = parse_tcp(&ip).expect("tcp");
    (t.seq, t.ack, t.flags, t.data.to_vec())
}

fn dhcp_msg(ty: u8) -> Vec<u8> {
    let mut m = vec![0u8; 240];
    m[0] = 1;
    m[1] = 1;
    m[2] = 6;
    m[4..8].copy_from_slice(&[1, 2, 3, 4]);
    m[28..34].copy_from_slice(&GMAC);
    m[236..240].copy_from_slice(&[99, 130, 83, 99]);
    m.extend_from_slice(&[53, 1, ty, 255]);
    m
}

#[test]
fn dhcp_offer_and_ack() {
    let mut s = Stack::new();
    for (ty, want) in [(1u8, 2u8), (3, 5)] {
        let d = udp([0; 4], 68, [255; 4], 67, &dhcp_msg(ty));
        let f = eth(
            [0xFF; 6],
            GMAC,
            ETH_IPV4,
            &ipv4([0; 4], [255; 4], PROTO_UDP, 1, &d),
        );
        s.input(0, &f);
        let out = s.take_frames();
        assert_eq!(out.len(), 1);
        let ip = parse_ipv4(&out[0][14..]).unwrap();
        assert_eq!(ip.dst, GUEST_IP);
        let m = &ip.payload[8..];
        assert_eq!(&m[16..20], &GUEST_IP, "yiaddr");
        assert_eq!(&m[4..8], &[1, 2, 3, 4], "xid");
        assert_eq!(&m[240..243], &[53, 1, want]);
        // ルーターと DNS のオプション
        let opts = &m[240..];
        assert!(opts.windows(6).any(|w| w == [3, 4, 10, 0, 2, 2]));
        assert!(opts.windows(6).any(|w| w == [6, 4, 10, 0, 2, 3]));
    }
}

#[test]
fn arp_answers_only_gateway_and_dns() {
    let mut s = Stack::new();
    for (tpa, answered) in [
        (GATEWAY_IP, true),
        (DNS_IP, true),
        (GUEST_IP, false),
        ([10, 0, 2, 9], false),
    ] {
        let mut a = vec![0, 1, 8, 0, 6, 4, 0, 1];
        a.extend_from_slice(&GMAC);
        a.extend_from_slice(&GUEST_IP);
        a.extend_from_slice(&[0; 6]);
        a.extend_from_slice(&tpa);
        s.input(0, &eth([0xFF; 6], GMAC, ETH_ARP, &a));
        let out = s.take_frames();
        assert_eq!(out.len(), answered as usize, "{tpa:?}");
        if answered {
            let r = &out[0][14..];
            assert_eq!(be16(r, 6), 2);
            assert_eq!(&r[8..14], &GATEWAY_MAC);
            assert_eq!(&r[14..18], &tpa);
        }
    }
}

fn dns_query(s: &mut Stack, name: &str, qtype: u16) -> Vec<u8> {
    let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
    for l in name.split('.') {
        q.push(l.len() as u8);
        q.extend_from_slice(l.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&[0, 1]);
    let d = udp(GUEST_IP, 1025, DNS_IP, 53, &q);
    s.input(0, &guest_ip_frame(DNS_IP, PROTO_UDP, &d));
    let out = s.take_frames();
    assert_eq!(out.len(), 1);
    let ip = parse_ipv4(&out[0][14..]).unwrap();
    ip.payload[8..].to_vec()
}

#[test]
fn dns_gives_stable_fake_addresses() {
    let mut s = Stack::new();
    let a = dns_query(&mut s, "Example.COM", 1);
    assert_eq!(&a[0..2], &[0x12, 0x34]);
    assert_eq!(be16(&a, 6), 1, "one answer");
    let ip: [u8; 4] = a[a.len() - 4..].try_into().unwrap();
    assert_eq!(ip, [198, 18, 0, 1]);
    assert_eq!(s.name_of(ip), Some("example.com"));
    let b = dns_query(&mut s, "other.example", 1);
    assert_eq!(&b[b.len() - 4..], &[198, 18, 0, 2]);
    let again = dns_query(&mut s, "example.com", 1);
    assert_eq!(&again[again.len() - 4..], &[198, 18, 0, 1]);
    // AAAA は答えなし
    let v6 = dns_query(&mut s, "example.com", 28);
    assert_eq!(be16(&v6, 6), 0);
    assert_eq!(v6[3] & 0x0F, 0);
    // 点のない名前は NXDOMAIN
    let single = dns_query(&mut s, "isatap", 1);
    assert_eq!((be16(&single, 6), single[3] & 0x0F), (0, 3));
}

/// 仮のアドレスへの接続: Connect（名前）→ SYN-ACK → データの往復 → 両方の FIN。
#[test]
fn tcp_connect_exchange_and_close() {
    let mut s = Stack::new();
    dns_query(&mut s, "example.com", 1);
    let dst = [198, 18, 0, 1];
    s.input(0, &guest_tcp(dst, 3000, 80, 1000, 0, SYN, &[]));
    assert!(
        s.take_frames().is_empty(),
        "no SYN-ACK before the connect result"
    );
    let reqs = s.take_requests();
    let [Request::Connect { id, target, port }] = &reqs[..] else {
        panic!("{reqs:?}");
    };
    assert_eq!((target, *port), (&Target::Host("example.com".into()), 80));
    let id = *id;
    s.connected(id, true);
    let (iss, ack, fl, _) = as_tcp(&s.take_frames()[0]);
    assert_eq!((ack, fl), (1001, SYN | ACK));
    s.input(1, &guest_tcp(dst, 3000, 80, 1001, iss + 1, ACK, &[]));
    s.input(
        2,
        &guest_tcp(
            dst,
            3000,
            80,
            1001,
            iss + 1,
            ACK | PSH,
            b"GET / HTTP/1.0\r\n\r\n",
        ),
    );
    assert_eq!(
        s.take_requests(),
        [Request::Send {
            id,
            data: b"GET / HTTP/1.0\r\n\r\n".to_vec()
        }]
    );
    let (_, ack, fl, _) = as_tcp(&s.take_frames()[0]);
    assert_eq!((ack, fl), (1019, ACK));

    // 外からの応答（MSS を超える長さは分けて送る）と EOF
    let body = vec![b'x'; 2000];
    s.recv(id, &body);
    s.remote_closed(id);
    let out = s.take_frames();
    let segs: Vec<_> = out.iter().map(|f| as_tcp(f)).collect();
    assert_eq!(segs[0].3.len(), 1460);
    assert_eq!(segs[1].3.len(), 540);
    assert_eq!(segs[2].2, FIN | ACK);
    let fin_seq = segs[2].0;
    assert_eq!(fin_seq, iss + 1 + 2000);
    // ゲストが全部と FIN を確認し、自分も FIN を送る
    s.input(3, &guest_tcp(dst, 3000, 80, 1019, fin_seq + 1, ACK, &[]));
    assert_eq!(s.connections(), 1);
    s.input(
        4,
        &guest_tcp(dst, 3000, 80, 1019, fin_seq + 1, FIN | ACK, &[]),
    );
    assert_eq!(
        s.take_requests(),
        [Request::Shutdown { id }, Request::Close { id }]
    );
    let (_, ack, _, _) = as_tcp(&s.take_frames()[0]);
    assert_eq!(ack, 1020);
    assert_eq!(s.connections(), 0);
}

/// 失われたセグメントは期限で送り直す。接続の失敗は RST。
#[test]
fn tcp_retransmit_and_refused() {
    let mut s = Stack::new();
    let dst = [93, 184, 216, 34];
    s.input(0, &guest_tcp(dst, 3001, 80, 5000, 0, SYN, &[]));
    let Request::Connect { id, target, .. } = s.take_requests().remove(0) else {
        panic!()
    };
    assert_eq!(target, Target::Ip(dst));
    s.connected(id, true);
    let (iss, _, _, _) = as_tcp(&s.take_frames()[0]);
    s.input(10, &guest_tcp(dst, 3001, 80, 5001, iss + 1, ACK, &[]));
    s.recv(id, b"hello");
    assert_eq!(as_tcp(&s.take_frames()[0]).3, b"hello");
    s.poll(10 + RTO_MIN - 1);
    assert!(s.take_frames().is_empty());
    s.poll(10 + RTO_MIN);
    let (seq, _, _, d) = as_tcp(&s.take_frames()[0]);
    assert_eq!((seq, d.as_slice()), (iss + 1, &b"hello"[..]));

    s.input(20, &guest_tcp(dst, 3002, 81, 7000, 0, SYN, &[]));
    let Request::Connect { id: id2, .. } = s.take_requests().remove(0) else {
        panic!()
    };
    s.connected(id2, false);
    let (_, ack, fl, _) = as_tcp(&s.take_frames()[0]);
    assert_eq!((ack, fl), (7001, RST | ACK));
}
