//! Go の machine/smdk2410 のテスト（smdk2410・input・kbd・run）を移したものと、
//! 合成プログラムでの Go との一致確認。

use super::*;
use crate::arm::UndefinedError;
use crate::loader::{Format, Segment, load_words};

fn words(ws: &[u32]) -> Vec<u8> {
    ws.iter().flat_map(|w| w.to_le_bytes()).collect()
}

fn image(addr: u32, entry: u32, data: Vec<u8>) -> Image {
    Image {
        format: Format::Bin,
        start: addr,
        length: data.len() as u32,
        entry,
        segs: vec![Segment { addr, data }],
        records: vec![],
    }
}

/// デバッグシリアル（UART1）に "OK" を出力してから停止する小さなプログラムを、
/// CE 仮想アドレス（0x80070000）に置いたイメージとして実行する統合テスト。
#[test]
fn boot_to_uart() {
    let prog = words(&[
        0xE3A00205, // MOV r0, #0x50000000   (UART ベース)
        0xE3800901, // ORR r0, r0, #0x4000   (UART1)
        0xE3800020, // ORR r0, r0, #0x20     (UTXH)
        0xE3A0104F, // MOV r1, #'O'
        0xE5C01000, // STRB r1, [r0]
        0xE3A0104B, // MOV r1, #'K'
        0xE5C01000, // STRB r1, [r0]
        0xE8B10000, // 空リスト LDM（UNPREDICTABLE → エミュレーション停止するはず）
    ]);
    let mut m = Machine::new();
    m.load_image(&image(0x80070000, 0x80070000, prog)).unwrap();
    m.reset();
    // エントリの VA→PA 変換: 0x80070000 → 0x30070000
    assert_eq!(m.cpu.pc(), 0x30070000);
    let mut err = None;
    for _ in 0..100 {
        if let Err(e) = m.step() {
            err = Some(e);
            break;
        }
    }
    match err {
        Some(StopError::Undefined(UndefinedError { pc, .. })) => assert_eq!(pc, 0x3007001C),
        other => panic!("expected UndefinedError, got {other:?}"),
    }
    assert_eq!(m.take_uart1(), b"OK");
    assert_eq!(m.steps(), 8, "エラーを起こした命令も数える");
}

#[test]
fn va_to_pa_table() {
    for (va, want) in [
        (0x80000000, Some(0x30000000)), // カーネルキャッシュ空間
        (0x80070000, Some(0x30070000)),
        (0xA0070000, Some(0x30070000)), // 非キャッシュ空間も同じ物理へ
        (0x30001000, Some(0x30001000)), // 物理アドレス直指定はそのまま
        (0x00001000, None),             // マップなし
        (0xC0000000, None),
    ] {
        assert_eq!(va_to_pa(va).ok(), want, "{va:08X}");
    }
}

#[test]
fn load_image_out_of_range() {
    let mut m = Machine::new();
    // RAM 末尾（128MB）を越える
    assert!(
        m.load_image(&image(0x87FFFFFC, 0x80000000, vec![0; 16]))
            .is_err()
    );
}

// ---- タッチ（Go の input_test）----

/// touch.dll の座標変換（0x0153170C）をトレースした命令列どおりに再現したもの
/// （定数の逆数掛け・算術シフト・負の補正・クリップ）。
fn driver_x4(d1: u32) -> i32 {
    let r3 = (d1 & 0x3FF) as i32 - 0x55;
    let r4 = r3.wrapping_mul(960);
    let hi = ((r4 as i64 * 0x094F2095i64) >> 32) as i32;
    let mut v = hi >> 5;
    v += ((v as u32) >> 31) as i32;
    clamp(v, 960)
}

fn driver_y4(d0: u32) -> i32 {
    let inv = (0x3FF - (d0 & 0x3FF)) as i32;
    let lr = (inv - 0x69).wrapping_mul(1280);
    let hi = ((lr as i64 * 0x2572FB07i64) >> 32) as i32;
    let mut v = hi >> 7;
    v += ((v as u32) >> 31) as i32;
    clamp(v, 1280)
}

fn clamp(v: i32, n: i32) -> i32 {
    v.clamp(0, n - 1)
}

/// 全ピクセルについて、touch_to_raw の生値をドライバの式に通すと同じ
/// ピクセル（1/4 単位の座標 ÷4）に戻ること。
#[test]
fn touch_to_raw_round_trip() {
    for x in 0..TOUCH_SCREEN_W {
        for y in 0..TOUCH_SCREEN_H {
            let (xp, yp) = touch_to_raw(x, y);
            assert!(xp <= 1023 && yp <= 1023);
            assert_eq!(
                (driver_x4(yp) / 4, driver_y4(xp) / 4),
                (x as i32, y as i32),
                "({x},{y})"
            );
        }
    }
}

/// 代表点（画面の四隅と中央）の生値。軸の入れ替えと Y の反転を明示する。
#[test]
fn touch_to_raw_corners() {
    for (x, y, xp, yp) in [
        (0, 0, 1023 - 105 - 1, 85 + 2),     // 左上: XP 大・YP 小
        (239, 0, 1023 - 105 - 1, 85 + 878), // 右上: YP 大
        (0, 319, 1023 - 105 - 874, 85 + 2), // 左下: XP 小
        (120, 160, 1023 - 105 - 439, 85 + 442),
    ] {
        assert_eq!(touch_to_raw(x, y), (xp, yp), "({x},{y})");
    }
}

#[test]
fn touch_range() {
    let mut m = Machine::new();
    for (x, y) in [(-1, 0), (0, -1), (240, 0), (0, 320)] {
        assert!(m.touch_down(x, y).is_err(), "({x},{y})");
    }
    m.touch_down(239, 319).unwrap();
}

// ---- キーボード用マイコン（Go の kbd_test）----

/// 1 バイトにつき EINT1 を 1 回: 積んだ時点で 1 回、ドライバが 1 バイト
/// 読むごとに残りがあればもう 1 回。
#[test]
fn kbd_one_interrupt_per_byte() {
    let mut k = KbdMcu::default();
    assert!(k.push(0x5A));
    assert!(!k.push(0xDA), "second waits for the first to be read");
    assert_eq!(k.transfer(0xFF), (0x5A, true));
    assert_eq!(k.transfer(0xFF), (0xDA, false));
    assert_eq!(k.transfer(0xFF), (0, false), "empty queue returns 0");
    assert_eq!(k.log, [0xFF; 3]);
}

#[test]
fn key_down_up_bytes() {
    let mut m = Machine::new();
    for (key, code) in [
        ("Enter", 0x5A),
        ("Up", 0x6C),
        ("Down", 0x6A),
        ("Left", 0x6D),
        ("Right", 0x6F),
    ] {
        m.key_down(key).unwrap();
        m.key_up(key).unwrap();
        let k = &mut m.sys.board.kbd;
        assert_eq!(
            (k.transfer(0xFF).0, k.transfer(0xFF).0),
            (code, code | 0x80),
            "{key}"
        );
    }
    assert!(m.key_down("SoftL").is_err(), "unknown key accepted");
    // EINT1 が INTC に届いていること
    m.key_down("Enter").unwrap();
    assert_ne!(
        m.sys.board.intc.read(0, 4) & (1 << 1),
        0,
        "EINT1 not raised"
    );
}

/// キー名の一覧は Go の KeyNames と同じくソート済み（バイト順）。
#[test]
fn key_names_sorted() {
    let names: Vec<&str> = KEY_SCAN_CODES.iter().map(|(n, _)| *n).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(names.len(), 57);
}

// ---- 実行ループ（Go の run_test）と Go との一致 ----

/// testdata の合成プログラム（コンパイル時に埋め込む。wasm32-wasip1 のテストでは
/// ファイルを読めないため）。
fn synthetic(name: &str) -> Machine {
    let data: &[u8] = match name {
        "idle.words" => include_bytes!("../../../../testdata/golden/synthetic/idle.words"),
        "adc-poll.words" => include_bytes!("../../../../testdata/golden/synthetic/adc-poll.words"),
        _ => panic!("unknown synthetic program {name}"),
    };
    let img = load_words(data).unwrap();
    let mut m = Machine::new();
    m.load_image(&img).unwrap();
    m.set_rtc(2006, 1, 2, 15, 4, 5);
    m.reset();
    m
}

/// 1 命令ずつ（step）とまとめて（run_until）で結果が同じこと。
#[test]
fn step_matches_run_until() {
    let mut a = synthetic("idle.words");
    let mut b = synthetic("idle.words");
    const N: u64 = 40000;
    for _ in 0..N {
        a.step().unwrap();
    }
    b.run_until(N).unwrap();
    assert_eq!(a.cpu_dump(), b.cpu_dump());
    assert!(a.sys.bus.ram(SDRAM_BASE).unwrap().0 == b.sys.bus.ram(SDRAM_BASE).unwrap().0);
}

/// 期待値の JSON Lines（testdata/golden/expected）から、命令数ごとの CPU 状態の
/// ダンプ（16 進）を取り出す（serde を使わない最小の読み取り）。
fn expected_cpu_dumps(name: &str) -> Vec<(u64, String)> {
    let text = match name {
        "synthetic-idle" => {
            include_str!("../../../../testdata/golden/expected/synthetic-idle.jsonl")
        }
        "synthetic-adc-poll" => {
            include_str!("../../../../testdata/golden/expected/synthetic-adc-poll.jsonl")
        }
        _ => panic!("unknown scenario {name}"),
    };
    let field = |line: &str, key: &str| -> String {
        let k = format!("\"{key}\":");
        let rest = &line[line.find(&k).unwrap() + k.len()..];
        rest.trim_start_matches('"')
            .split(['"', ',', '}'])
            .next()
            .unwrap()
            .to_string()
    };
    text.lines()
        .map(|l| (field(l, "steps").parse().unwrap(), field(l, "cpu")))
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 合成プログラムの基準シナリオで、CPU 状態のダンプが Go の期待値と一致する
/// （RAM・UART・画面のハッシュは CLI の一致確認で見る）。止まる点を不揃いに
/// 刻んでも同じになることも確かめる。
#[test]
fn synthetic_scenarios_match_go() {
    for name in ["synthetic-idle", "synthetic-adc-poll"] {
        let words = &name["synthetic-".len()..];
        let want = expected_cpu_dumps(name);
        let mut m = synthetic(&format!("{words}.words"));
        for (steps, dump) in &want {
            m.run_until(*steps).unwrap();
            assert_eq!(&hex(&m.cpu_dump()), dump, "{name} at {steps}");
        }
        let mut chunked = synthetic(&format!("{words}.words"));
        let mut s = 0;
        for (i, step) in [1u64, 150, 5000, 7, 12345, 3, 60000, 99991, 2]
            .iter()
            .cycle()
            .enumerate()
        {
            s = (s + step).min(want.last().unwrap().0);
            chunked.run_until(s).unwrap();
            if s == want.last().unwrap().0 || i > 10000 {
                break;
            }
        }
        assert_eq!(
            chunked.cpu_dump(),
            m.cpu_dump(),
            "{name}: chunked run differs"
        );
    }
}

#[allow(clippy::type_complexity)]
/// 比べられる全状態（CPU のダンプと退避領域・RAM・TLB・時間を持つデバイス・
/// 仮想時間）。アイドルスキップの有無で一致することの確認用。
fn full_state(
    m: &Machine,
) -> (
    Vec<u8>,
    crate::arm::Cpu,
    Vec<u8>,
    Vec<(u32, u32, u8)>,
    String,
    (u32, u64, i64),
) {
    let (ram, _) = m.sys.bus.ram(SDRAM_BASE).unwrap();
    let tlb = m
        .sys
        .mmu
        .tlb
        .iter()
        .map(|e| (e.tag, e.pa, e.perm))
        .collect();
    let b = &m.sys.board;
    let devs = format!("{:?} {:?} {:?} {:?}", b.intc, b.timer, b.adc, b.rtc);
    // 仮想時間は溜めたティックを含めて比べる（pending はデバイスに渡す前の分）
    let mut cpu = m.cpu.clone();
    cpu.set_history(0);
    (
        m.cpu_dump(),
        cpu,
        ram.to_vec(),
        tlb,
        devs,
        (b.tick_acc, b.steps, b.pending),
    )
}

/// アイドルスキップの有無で、同じ命令数まで進めたときの全状態が一致すること。
/// run_until の上限を不揃いに刻み、上限・タイマー期限・割り込みの境界をまたぐ
/// 場合を含める（Go の TestIdleSkipMatchesStepping）。
#[test]
fn idle_skip_matches_stepping() {
    let mut a = synthetic("idle.words");
    let mut b = synthetic("idle.words");
    b.set_idle_skip(false);
    let mut limit = 0;
    for (i, chunk) in [1u64, 150, 5000, 7, 12345, 3, 60000, 99991, 2, 150000]
        .into_iter()
        .enumerate()
    {
        limit += chunk;
        a.run_until(limit).unwrap();
        b.run_until(limit).unwrap();
        assert_eq!((a.steps(), b.steps()), (limit, limit));
        // pending（溜めたティック）はスキップの有無で溜まり方が違ってよいので、
        // 同期してから比べる（同期は状態の見え方を変えない）。
        a.sys.board.sync_time();
        b.sys.board.sync_time();
        assert!(
            full_state(&a) == full_state(&b),
            "chunk {i} (step {limit}): state differs"
        );
    }
    // 割り込みで実際にループを抜けていること（カウンタが進む）と、大半を飛ばしていること。
    let (ram, off) = a.sys.bus.ram(0x30001004).unwrap();
    let count = u32::from_le_bytes(ram[off as usize..off as usize + 4].try_into().unwrap());
    assert!(
        count >= 50,
        "counter = {count} (timer interrupts did not break the loop)"
    );
    assert!(
        a.idle_skipped() >= limit / 2,
        "skipped {} of {limit}",
        a.idle_skipped()
    );
    assert_eq!(b.idle_skipped(), 0);
}

/// MMIO のポーリング（ADC の stable_read）でも、スキップの有無で全状態が一致する。
#[test]
fn idle_skip_mmio_poll_matches_stepping() {
    let mut a = synthetic("adc-poll.words");
    let mut b = synthetic("adc-poll.words");
    b.set_idle_skip(false);
    let mut limit = 0;
    for (i, chunk) in [7u64, 100000, 31, 250000, 4, 400001]
        .into_iter()
        .enumerate()
    {
        limit += chunk;
        a.run_until(limit).unwrap();
        b.run_until(limit).unwrap();
        a.sys.board.sync_time();
        b.sys.board.sync_time();
        assert!(
            full_state(&a) == full_state(&b),
            "chunk {i} (step {limit}): state differs"
        );
    }
    assert!(
        a.cpu.reg(7) >= 4,
        "conversions completed = {}",
        a.cpu.reg(7)
    );
    assert!(
        a.idle_skipped() >= limit / 2,
        "skipped {} of {limit}",
        a.idle_skipped()
    );
}

// ---- スナップショット ----

/// 保存 → 読み込み → 続きの実行が、通し実行と一致する（計画書 §5.4）。
/// 同じ状態からは同じバイト列になることも確かめる。
#[test]
fn snapshot_resume_matches_uninterrupted() {
    for name in ["idle.words", "adc-poll.words"] {
        let mut through = synthetic(name);
        through.run_until(700_001).unwrap();

        let mut first = synthetic(name);
        first.run_until(333_333).unwrap();
        let mut buf = vec![];
        first.save_snapshot(&mut buf, "image-id").unwrap();
        let mut again = vec![];
        first.save_snapshot(&mut again, "image-id").unwrap();
        assert!(buf == again, "{name}: same state must give the same bytes");

        let mut resumed = Machine::new();
        assert_eq!(resumed.load_snapshot(&buf[..]).unwrap(), "image-id");
        assert_eq!(resumed.steps(), 333_333);
        let mut round = vec![];
        resumed.save_snapshot(&mut round, "image-id").unwrap();
        assert!(
            round == buf,
            "{name}: save after load must give the same bytes"
        );

        resumed.run_until(700_001).unwrap();
        through.sys.board.sync_time();
        resumed.sys.board.sync_time();
        assert!(
            full_state(&resumed) == full_state(&through),
            "{name}: resumed run differs"
        );
    }
}

/// 壊れたスナップショットは panic せずエラーになり、別のマシン名も拒む。
#[test]
fn snapshot_rejects_corruption() {
    let mut m = synthetic("idle.words");
    m.run_until(1000).unwrap();
    let mut buf = vec![];
    m.save_snapshot(&mut buf, "x").unwrap();
    // RAM の中の 1 バイトを壊す → CRC で検出
    let mut bad = buf.clone();
    let mid = bad.len() / 2;
    bad[mid] ^= 0x40;
    assert!(Machine::new().load_snapshot(&bad[..]).is_err());
    // 切り詰め
    assert!(Machine::new().load_snapshot(&buf[..buf.len() - 1]).is_err());
    assert!(Machine::new().load_snapshot(&buf[..100]).is_err());
}
