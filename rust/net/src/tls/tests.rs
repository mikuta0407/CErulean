//! 試験用の小さな TLS 1.0 のクライアント（WM5 と同じ SSL 2.0 互換の ClientHello）で、
//! ハンドシェイクと双方向のデータを確かめる。
use super::*;

struct Client {
    transcript: Vec<u8>,
    client_random: [u8; 32],
    master: Vec<u8>,
    read: Option<Cipher>,
    write: Option<Cipher>,
}

fn records(buf: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut v = Vec::new();
    let mut i = 0;
    while i + 5 <= buf.len() {
        let n = u16::from_be_bytes([buf[i + 3], buf[i + 4]]) as usize;
        v.push((buf[i], buf[i + 5..i + 5 + n].to_vec()));
        i += 5 + n;
    }
    v
}

impl Client {
    fn seal(&mut self, ct: u8, data: &[u8]) -> Vec<u8> {
        let mut body = data.to_vec();
        if let Some(c) = &mut self.write {
            body.extend_from_slice(&c.mac(ct, data));
            c.rc4.apply(&mut body);
            c.seq += 1;
        }
        let mut r = vec![ct, 3, 1];
        r.extend_from_slice(&(body.len() as u16).to_be_bytes());
        r.extend_from_slice(&body);
        r
    }

    fn open(&mut self, body: &[u8]) -> Vec<u8> {
        let mut b = body.to_vec();
        let c = self.read.as_mut().unwrap();
        c.rc4.apply(&mut b);
        b.truncate(b.len() - c.mac_len());
        c.seq += 1;
        b
    }
}

#[test]
fn handshake_and_data() {
    let mut rng = Drbg::new(b"tls test");
    let key = RsaKey::generate(&mut rng, 512);
    let mut s = TlsServer::new(vec![b"fake-cert".to_vec()], key.clone());
    let mut c = Client {
        transcript: Vec::new(),
        client_random: [0; 32],
        master: Vec::new(),
        read: None,
        write: None,
    };
    // SSL 2.0 互換の ClientHello（WM5 が送った形: RC4-MD5・RC4-SHA・3DES と SSL 2.0 の暗号）
    let challenge = [0x11u8; 16];
    let mut msg = vec![1, 3, 1, 0, 9, 0, 0, 0, 16];
    msg.extend_from_slice(&[0, 0, 4, 0, 0, 5, 1, 0, 0x80]);
    msg.extend_from_slice(&challenge);
    let mut hello = vec![0x80 | (msg.len() >> 8) as u8, msg.len() as u8];
    hello.extend_from_slice(&msg);
    c.transcript.extend_from_slice(&msg);
    c.client_random[16..].copy_from_slice(&challenge);
    assert!(s.input(&hello, &mut rng).unwrap().is_empty());
    let flight = s.take_output();
    let recs = records(&flight);
    assert_eq!(recs.len(), 1);
    let hs = &recs[0].1;
    c.transcript.extend_from_slice(hs);
    assert_eq!(hs[0], HS_SERVER_HELLO);
    let server_random = &hs[4 + 2..4 + 34];
    assert_eq!(&hs[4 + 35..4 + 37], &RC4_128_SHA.to_be_bytes());
    // ClientKeyExchange
    let pms: Vec<u8> = [3u8, 1]
        .into_iter()
        .chain(std::iter::repeat_n(0x42, 46))
        .collect();
    let k = key.size();
    let mut em = vec![0u8, 2];
    em.extend(std::iter::repeat_n(0x33u8, k - 3 - 48));
    em.push(0);
    em.extend_from_slice(&pms);
    let enc = key.public(&em);
    let mut kx = (enc.len() as u16).to_be_bytes().to_vec();
    kx.extend_from_slice(&enc);
    let kxm = hs_msg(HS_CLIENT_KX, &kx);
    c.transcript.extend_from_slice(&kxm);
    let mut seed = c.client_random.to_vec();
    seed.extend_from_slice(server_random);
    c.master = prf(&pms, b"master secret", &seed, 48);
    let mut seed2 = server_random.to_vec();
    seed2.extend_from_slice(&c.client_random);
    let kb = prf(&c.master, b"key expansion", &seed2, 72);
    let mk = |mac: &[u8], key: &[u8]| Cipher {
        rc4: Rc4::new(key),
        mac_key: mac.to_vec(),
        sha: true,
        seq: 0,
    };
    let mut out = c.seal(CT_HANDSHAKE, &kxm);
    out.extend_from_slice(&c.seal(CT_CCS, &[1]));
    c.write = Some(mk(&kb[..20], &kb[40..56]));
    c.read = Some(mk(&kb[20..40], &kb[56..72]));
    let mut fseed = md5(&c.transcript).to_vec();
    fseed.extend_from_slice(&sha1(&c.transcript));
    let fin = hs_msg(HS_FINISHED, &prf(&c.master, b"client finished", &fseed, 12));
    c.transcript.extend_from_slice(&fin);
    out.extend_from_slice(&c.seal(CT_HANDSHAKE, &fin));
    out.extend_from_slice(&c.seal(CT_APP, b"GET / HTTP/1.0\r\n\r\n"));
    let plain = s.input(&out, &mut rng).unwrap();
    assert!(s.is_open());
    assert_eq!(plain, b"GET / HTTP/1.0\r\n\r\n");
    // サーバーの CCS・Finished と応答
    s.send(b"HTTP/1.0 200 OK\r\n\r\nhi");
    let recs = records(&s.take_output());
    assert_eq!(recs[0], (CT_CCS, vec![1]));
    let sfin = c.open(&recs[1].1);
    let mut fseed = md5(&c.transcript).to_vec();
    fseed.extend_from_slice(&sha1(&c.transcript));
    assert_eq!(
        sfin,
        hs_msg(HS_FINISHED, &prf(&c.master, b"server finished", &fseed, 12))
    );
    assert_eq!(c.open(&recs[2].1), b"HTTP/1.0 200 OK\r\n\r\nhi");
    // MAC を壊したレコードは拒否する
    let mut bad = c.seal(CT_APP, b"x");
    let last = bad.len() - 1;
    bad[last] ^= 1;
    assert!(s.input(&bad, &mut rng).is_err());
}
