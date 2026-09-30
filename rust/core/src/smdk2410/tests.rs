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
/// w4・h4 は画面の幅・高さの 4 倍（ドライバの変数。QVGA で 960・1280）。
fn driver_x4(d1: u32, w4: i32) -> i32 {
    let r3 = (d1 & 0x3FF) as i32 - 0x55;
    let r4 = r3.wrapping_mul(w4);
    let hi = ((r4 as i64 * 0x094F2095i64) >> 32) as i32;
    let mut v = hi >> 5;
    v += ((v as u32) >> 31) as i32;
    clamp(v, w4)
}

fn driver_y4(d0: u32, h4: i32) -> i32 {
    let inv = (0x3FF - (d0 & 0x3FF)) as i32;
    let lr = (inv - 0x69).wrapping_mul(h4);
    let hi = ((lr as i64 * 0x2572FB07i64) >> 32) as i32;
    let mut v = hi >> 7;
    v += ((v as u32) >> 31) as i32;
    clamp(v, h4)
}

fn clamp(v: i32, n: i32) -> i32 {
    v.clamp(0, n - 1)
}

/// 全ピクセルについて、touch_to_raw の生値をドライバの式に通すと同じ
/// ピクセル（1/4 単位の座標 ÷4）に戻ること（QVGA・VGA・正方形）。
#[test]
fn touch_to_raw_round_trip() {
    for (w, h) in [
        (TOUCH_SCREEN_W, TOUCH_SCREEN_H),
        (480, 640),
        (240, 240),
        (480, 480),
    ] {
        for x in 0..w {
            for y in 0..h {
                let (xp, yp) = touch_to_raw(x, y, w, h);
                assert!(xp <= 1023 && yp <= 1023);
                assert_eq!(
                    (
                        driver_x4(yp, 4 * w as i32) / 4,
                        driver_y4(xp, 4 * h as i32) / 4
                    ),
                    (x as i32, y as i32),
                    "{w}x{h} ({x},{y})"
                );
            }
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
        assert_eq!(touch_to_raw(x, y, 240, 320), (xp, yp), "({x},{y})");
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

/// 画面の大きさ: set_display は Device Emulator と同じ形で BSP の引数の領域に置き、
/// タッチの範囲はゲストが LCD に設定した大きさに従う。
#[test]
fn display_args_and_touch_size() {
    let mut m = Machine::new();
    assert!(m.set_display(1024, 1024).is_err(), "frame buffer over 1MB");
    assert!(m.set_display(8, 640).is_err());
    m.set_display(480, 640).unwrap();
    assert_eq!(
        m.peek_ram(0x3002_0044, 10).unwrap(),
        [0x34, 0xDE, 0x12, 0xDE, 0xE0, 0x01, 0x80, 0x02, 0x10, 0x00]
    );
    // LCD の設定の前は 240×320
    assert_eq!(m.touch_screen_size(), (240, 320));
    assert!(m.touch_down(479, 639).is_err());
    // OAL と同じ設定（TFT・16bpp・ENVID、LINEVAL=639、HOZVAL=479）
    bus_w(&mut m, 0x4D00_0004, 4, 639 << 14);
    bus_w(&mut m, 0x4D00_0008, 4, 479 << 8);
    bus_w(&mut m, 0x4D00_0000, 4, 0x6F9);
    assert_eq!(m.touch_screen_size(), (480, 640));
    m.touch_down(479, 639).unwrap();
    assert!(m.touch_down(480, 0).is_err());
}

// ---- キーボード用マイコン（Go の kbd_test）----

/// 1 バイトにつき EINT1 を 1 回: 積んだ時点で 1 回、ドライバが 1 バイト
/// 読むごとに残りがあればもう 1 回。
#[test]
fn kbd_one_interrupt_per_byte() {
    let mut k = KbdMcu::default();
    assert!(k.push(0x5A));
    assert!(!k.push(0xDA), "second waits for the first to be read");
    assert_eq!(k.transfer(0xFF), 0x5A);
    assert_eq!(k.transfer(0xFF), 0xDA);
    assert_eq!(k.transfer(0xFF), 0, "empty queue returns 0");
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
            (k.transfer(0xFF), k.transfer(0xFF)),
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

/// 押して直ぐ離したとき（2 バイトが同時に積まれる）: OAL の割り込みの流れ（ISR が
/// マスクして SRCPND/INTPND をクリア → IST が SPI で 1 バイト読む → InterruptDone が
/// SRCPND をクリアしてからマスクを外す。2026-09-30 に --watch で観察）で、離した
/// バイトの EINT1 が消されずに届くこと。
#[test]
fn kbd_second_byte_after_interrupt_done() {
    use crate::bus::Devices;
    use crate::s3c2410::{REG_INTMSK, REG_INTPND, REG_SRCPND};
    let mut m = Machine::new();
    let b = &mut m.sys.board;
    let eint1 = 1u32 << 1;
    b.write(Dev::Intc, REG_INTMSK, 4, !eint1);
    m.key_down("Right").unwrap();
    m.key_up("Right").unwrap();
    let b = &mut m.sys.board;
    for want in [0x6F, 0xEF] {
        assert_ne!(b.intc.read(REG_SRCPND, 4) & eint1, 0, "EINT1 for {want:#x}");
        // ISR
        b.write(Dev::Intc, REG_INTMSK, 4, 0xFFFF_FFFF);
        b.write(Dev::Intc, REG_SRCPND, 4, eint1);
        b.write(Dev::Intc, REG_INTPND, 4, eint1);
        // IST（SPI の転送はキーボード用マイコンを直接読む）
        assert_eq!(b.kbd.transfer(0xFF), want);
        // InterruptDone
        b.write(Dev::Intc, REG_SRCPND, 4, eint1);
        b.write(Dev::Intc, REG_INTMSK, 4, !eint1);
    }
    assert_eq!(
        b.intc.read(REG_SRCPND, 4) & eint1,
        0,
        "no EINT1 after the queue drains"
    );
}

/// キー名の一覧は Go の KeyNames と同じくソート済み（バイト順）。
#[test]
fn key_names_sorted() {
    let names: Vec<&str> = KEY_SCAN_CODES.iter().map(|(n, _)| *n).collect();
    let mut sorted = names.clone();
    sorted.sort();
    assert_eq!(names, sorted);
    assert_eq!(names.len(), 68); // 57 + 記号 11（2026-09-30）
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

/// 実イメージでのスナップショット往復（CERULEAN_IMAGE があるときだけ。Go の
/// TestRealImageSnapshotResume）。「N 命令で保存 → 復元して M 命令」と「通しで
/// N+M 命令」で、UART 出力と最終状態（スナップショットのバイト列）が一致する。
#[test]
fn real_image_snapshot_resume() {
    let Some(path) = std::env::var_os("CERULEAN_IMAGE") else {
        return;
    };
    const N: u64 = 300_000_000;
    const M: u64 = 100_000_000;
    let data = std::fs::read(path).unwrap();
    let img = crate::loader::load(&data, "image.bin", 0x30000000).unwrap();
    let boot = || {
        let mut m = Machine::new();
        m.load_image(&img).unwrap();
        m.set_rtc(2006, 1, 2, 15, 4, 5);
        m.reset();
        m
    };
    let mut a = boot();
    a.run_until(N + M).unwrap();
    let out_a = a.take_uart1();

    let mut b = boot();
    b.run_until(N).unwrap();
    let mut out_b = b.take_uart1();
    let mut buf = vec![];
    b.save_snapshot(&mut buf, "id").unwrap();
    let mut c = Machine::new();
    c.load_snapshot(&buf[..]).unwrap();
    c.run_until(N + M).unwrap();
    out_b.extend(c.take_uart1());
    assert!(out_a == out_b, "UART output differs");

    let (mut sa, mut sc) = (vec![], vec![]);
    a.save_snapshot(&mut sa, "id").unwrap();
    c.save_snapshot(&mut sc, "id").unwrap();
    assert!(sa == sc, "final state differs");
}

// ---- PC カード（バンク2）と外部割り込み ----

fn bus_w(m: &mut Machine, pa: u32, size: u32, v: u32) {
    let Sys { bus, board, .. } = &mut m.sys;
    bus.write(pa, size, v, board).unwrap();
}

fn bus_r(m: &mut Machine, pa: u32, size: u32) -> u32 {
    let Sys { bus, board, .. } = &mut m.sys;
    bus.read(pa, size, board).unwrap()
}

/// PD6710 のレジスタ（Index/Data のポート 0x3E0/0x3E1。I/O 空間は PA 0x11000000〜）。
fn pcic_w(m: &mut Machine, reg: u8, v: u8) {
    bus_w(m, 0x1100_03E0, 1, reg as u32);
    bus_w(m, 0x1100_03E1, 1, v as u32);
}

fn pcic_r(m: &mut Machine, reg: u8) -> u8 {
    bus_w(m, 0x1100_03E0, 1, reg as u32);
    bus_r(m, 0x1100_03E1, 1) as u8
}

fn srcpnd(m: &Machine) -> u32 {
    m.sys.board.intc.srcpnd
}

/// pcc_smdk2410.dll・atadisk.dll と同じ手順（2026-09-29 に観察した値）で、挿入の
/// 管理割り込み（-INTR → EINT3）・CIS・構成・ATA のコマンドと割り込み（IRQ3 → EINT8）
/// がバス越しに通ること。
#[test]
fn pc_card_through_the_bus() {
    let mut m = Machine::new();
    assert_eq!(pcic_r(&mut m, 0x00), 0x82, "Chip Revision");
    // GPF3 = EINT3（立ち下がり）、GPG0 = EINT8（High レベル）、EINT8 のマスクを外す。
    // 方式を先に決める（リセット値の 000 は Low レベルで、ピンが Low なので EINTPEND が
    // 立つ。実機と同じ）
    bus_w(&mut m, 0x5600_0088, 4, 0x2000);
    bus_w(&mut m, 0x5600_0050, 4, 0x80);
    bus_w(&mut m, 0x5600_008C, 4, 0x1);
    bus_w(&mut m, 0x5600_0060, 4, 0x2);
    bus_w(&mut m, 0x5600_00A4, 4, 0x00FF_FEF0);
    pcic_w(&mut m, 0x05, 0x0C); // カード検出・Ready の管理割り込み
    pcic_w(&mut m, 0x03, 0x10); // 管理割り込みは -INTR
    assert_eq!(srcpnd(&m), 0);

    let mut disk = vec![0u8; 64 * 512];
    disk[5 * 512] = 0x5A;
    m.insert_card(disk).unwrap();
    assert_eq!(srcpnd(&m) & 1 << 3, 1 << 3, "insertion raises EINT3");
    assert_eq!(pcic_r(&mut m, 0x01) & 0x0C, 0x0C, "card detected");
    assert_eq!(pcic_r(&mut m, 0x04), 0x08, "Card Detect Change");
    assert_eq!(pcic_r(&mut m, 0x04), 0x00, "cleared by reading");
    m.sys.board.intc.write(0x00, 4, 1 << 3);

    // 電源・リセット解除・属性メモリの窓（System 0〜7FFFFF、REG）
    pcic_w(&mut m, 0x02, 0x93);
    pcic_w(&mut m, 0x03, 0x50);
    for (r, v) in [
        (0x10, 0x00),
        (0x11, 0x00),
        (0x12, 0xFF),
        (0x13, 0xC7),
        (0x14, 0x00),
        (0x15, 0x40),
    ] {
        pcic_w(&mut m, r, v);
    }
    pcic_w(&mut m, 0x06, 0x21);
    assert_eq!(pcic_r(&mut m, 0x01) & 0x60, 0x60, "powered and ready");
    assert_eq!(bus_r(&mut m, 0x1000_0000, 1), 0x01, "CISTPL_DEVICE");
    bus_w(&mut m, 0x1000_0200, 1, 0x42); // 構成 2（プライマリ）・レベル
    // I/O 窓 0: 1F0〜1F7、窓 1: 3F6〜3F7（自動の幅）、カードの IRQ は IRQ3
    for (r, v) in [(0x08, 0xF0), (0x09, 0x01), (0x0A, 0xF7), (0x0B, 0x01)] {
        pcic_w(&mut m, r, v);
    }
    for (r, v) in [(0x0C, 0xF6), (0x0D, 0x03), (0x0E, 0xF7), (0x0F, 0x03)] {
        pcic_w(&mut m, r, v);
    }
    pcic_w(&mut m, 0x07, 0x22);
    pcic_w(&mut m, 0x06, 0xE1);
    pcic_w(&mut m, 0x03, 0x73);
    assert_eq!(bus_r(&mut m, 0x1100_03F6, 1), 0x50, "Alt Status");

    // LBA 5 を読む: 割り込みは EINT8（レベル）。Status を読むまで立て直される
    for (port, v) in [
        (0x1F2, 1),
        (0x1F3, 5),
        (0x1F4, 0),
        (0x1F5, 0),
        (0x1F6, 0xE0),
        (0x1F7, 0x20),
    ] {
        bus_w(&mut m, 0x1100_0000 + port, 1, v);
    }
    let int_eint8_23 = 1 << 5;
    assert_ne!(srcpnd(&m) & int_eint8_23, 0);
    bus_w(&mut m, 0x5600_00A8, 4, 1 << 8); // EINTPEND をクリア
    m.sys.board.intc.write(0x00, 4, int_eint8_23);
    assert_ne!(srcpnd(&m) & int_eint8_23, 0, "level: still requested");
    assert_eq!(bus_r(&mut m, 0x1100_01F7, 1), 0x58, "Status (DRQ)");
    bus_w(&mut m, 0x5600_00A8, 4, 1 << 8);
    m.sys.board.intc.write(0x00, 4, int_eint8_23);
    assert_eq!(srcpnd(&m) & int_eint8_23, 0, "released by reading Status");
    assert_eq!(bus_r(&mut m, 0x1100_01F0, 2), 0x5A, "first data word");
    // 32 ビットの読み出しは続く番地への 16 ビットの 2 回: 下位はデータ、上位は
    // 1F2h/1F3h（セクタ数 1・セクタ番号 5）
    assert_eq!(bus_r(&mut m, 0x1100_01F0, 4), 0x0501_0000);

    // スナップショット: 同じバイト列に戻り、カードの中身も戻る
    let mut buf = vec![];
    m.save_snapshot(&mut buf, "id").unwrap();
    let mut r = Machine::new();
    r.load_snapshot(&buf[..]).unwrap();
    let mut again = vec![];
    r.save_snapshot(&mut again, "id").unwrap();
    assert!(buf == again, "save after load must give the same bytes");
    assert_eq!(r.card_disk(), m.card_disk());

    // 電源投入で RDY が変わった（Ready Change）。読んで -INTR を戻してから抜く:
    // カード検出の変化で再び EINT3（立ち下がり）
    assert_eq!(pcic_r(&mut m, 0x04), 0x04, "Ready Change");
    m.sys.board.intc.write(0x00, 4, u32::MAX);
    let disk = m.eject_card().unwrap();
    assert_eq!(disk[5 * 512], 0x5A);
    assert_eq!(srcpnd(&m) & 1 << 3, 1 << 3);
    assert_eq!(pcic_r(&mut m, 0x01) & 0x4C, 0, "no card, no power");
    assert!(m.eject_card().is_none());
}

/// PC カードの前の版（machine の版数 1）のスナップショットを読める
/// （コントローラは初期状態・カードなし）。
#[test]
fn snapshot_version_1_loads_without_pc_card() {
    let mut m = synthetic("idle.words");
    m.run_until(1000).unwrap();
    let mut buf = vec![];
    m.save_snapshot(&mut buf, "id").unwrap();
    // 版数 2 の machine チャンクを版数 1 にし、末尾の pcic チャンクを外して作り直す
    let v1 = rewrite_as_v1(&buf);
    let mut r = Machine::new();
    r.load_snapshot(&v1[..]).unwrap();
    assert_eq!(r.steps(), 1000);
    assert!(r.card_disk().is_none());
    assert_eq!(r.sys.board.pcic, crate::pccard::Pd6710::new());
}

/// スナップショットを読み直し、machine を版数 1 に、pcic 以降を除いて書き直す。
fn rewrite_as_v1(buf: &[u8]) -> Vec<u8> {
    rewrite_as(buf, 1)
}

/// スナップショット（フラッシュなし）を machine の版数 v（4 以下）の形に書き直す。DMA と IIS は値保持
/// スタブの形（書かれたレジスタ）にし、版数 1 なら pcic 以降を除く。
fn rewrite_as(buf: &[u8], v: u16) -> Vec<u8> {
    use crate::s3c2410::{Dma, Iis, Stub};
    use crate::snapshot::{Reader, Writer};
    let mut rd = Reader::new(buf).unwrap();
    let mut w = Writer::new(
        vec![],
        &rd.header.machine.clone(),
        &rd.header.image_id.clone(),
    )
    .unwrap();
    let mut iis_stub = Stub::new(&[]);
    while let Some(c) = rd.next_chunk().unwrap() {
        match c.name.as_str() {
            "pcic" if v == 1 => break,
            // 版数 5 までの machine は命令数・端数・エントリの 16 バイト（6 でフラッシュの
            // 大きさを足した）
            "machine" => w.raw_chunk(&c.name, v, &c.body[..16]).unwrap(),
            "dma" => {
                let mut dma = Dma::new();
                let mut d = c.decoder(Dma::STATE_VERSION).unwrap();
                dma.load_state(&mut d).unwrap();
                let mut st = Stub::new(&[]);
                for (n, ch) in dma.ch.iter().enumerate() {
                    let b = n as u32 * 0x40;
                    for (o, x) in [
                        (0x00, ch.disrc),
                        (0x04, ch.disrcc),
                        (0x08, ch.didst),
                        (0x0C, ch.didstc),
                        (0x10, ch.dcon),
                        (0x20, (ch.on as u32) << 1),
                    ] {
                        if x != 0 {
                            st.write(b + o, 4, x);
                        }
                    }
                }
                w.chunk("dma", 1, |e| st.save_state(e)).unwrap();
            }
            "iis" => {
                let mut iis = Iis::new();
                let mut d = c.decoder(Iis::STATE_VERSION).unwrap();
                iis.load_state(&mut d).unwrap();
                for (o, x) in [
                    (0x00, iis.con),
                    (0x04, iis.mode),
                    (0x08, iis.psr),
                    (0x0C, iis.fcon),
                ] {
                    if x != 0 {
                        iis_stub.write(o, 4, x);
                    }
                }
            }
            "stub:de-paravirt" => {
                w.chunk("stub:iis", 1, |e| iis_stub.save_state(e)).unwrap();
                w.raw_chunk(&c.name, c.version, &c.body).unwrap();
            }
            _ => w.raw_chunk(&c.name, c.version, &c.body).unwrap(),
        }
    }
    w.finish().unwrap()
}

/// DMA と IIS が値保持スタブだった版（machine の版数 4）のスナップショットを読める
/// （書かれたレジスタを引き継ぎ、転送は始まっていない・FIFO は空とする）。
#[test]
fn snapshot_version_4_converts_dma_and_iis() {
    let mut m = synthetic("idle.words");
    m.run_until(1000).unwrap();
    // 音声ドライバと同じ設定で DMA を ON にし、IIS はまだ始めない
    for (a, v) in [
        (0x5500_0000, 0xA2),
        (0x5500_0004, 0xAD),
        (0x5500_0008, 0x42),
        (0x5500_000C, 0xA000),
        (0x4B00_0080, 0x3000_0000),
        (0x4B00_0088, 0x5500_0010),
        (0x4B00_008C, 3),
        (0x4B00_0090, 0xA090_0400),
        (0x4B00_00A0, 2),
    ] {
        bus_w(&mut m, a, 4, v);
    }
    assert_eq!(bus_r(&mut m, 0x4B00_0094, 4), 1 << 20 | (0x400 - 32));
    let mut buf = vec![];
    m.save_snapshot(&mut buf, "id").unwrap();
    let mut r = Machine::new();
    r.load_snapshot(&rewrite_as(&buf, 4)[..]).unwrap();
    assert_eq!(r.sys.board.iis.read(0x04, 4), 0xAD);
    let c = &r.sys.board.dma.ch[2];
    assert_eq!(
        (c.disrc, c.dcon, c.on, c.curr_tc),
        (0x3000_0000, 0xA090_0400, true, 0)
    );
    // 新しい版で保存し直すと、スタブの版にない FIFO・カウンタの分だけが違う
    // （次の DMA の要求で埋まる）。
    let b = &mut r.sys.board;
    let ints = b.dma.service_iis_tx(&mut b.iis);
    assert_eq!(ints, 0);
    assert_eq!(b.dma.read(0x94, 4), 1 << 20 | (0x400 - 32));
}

/// フラッシュのイメージ（WM6）の構成: バンク0 に NOR フラッシュが載り、MMU 越しの
/// 読みは中身を直接、書き込みは AMD 方式のコマンドになる。ID 読み出しの間も先頭の
/// 数語以外は中身が読める（XIP のドライバがフラッシュ上で動き続ける）。
#[test]
fn flash_through_the_mmu() {
    let mut data = vec![0u8; 0x30000];
    data[0..4].copy_from_slice(&0xEA0003FEu32.to_le_bytes());
    data[0x40..0x44].copy_from_slice(b"ECEC");
    data[0x1000..0x1004].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    data[0x2000..0x2002].copy_from_slice(&0xFF0Fu16.to_le_bytes());
    data[0x10000..0x10004].copy_from_slice(&0xE1A0_0000u32.to_le_bytes());
    let img = crate::loader::load(&data, "PPC_JPN.bin", 0).unwrap();
    let mut m = Machine::new();
    m.load_image(&img).unwrap();
    m.reset();
    assert_eq!((m.flash_size(), m.cpu.pc()), (0x30000, 0));
    assert_eq!(bus_r(&mut m, 0x30000, 4), 0, "open bus after the flash");

    // 変換表（TTB = PA 0x30004000）: VA 0〜 → フラッシュ、VA 0x30000000〜 → SDRAM
    let ttb = 0x3000_4000;
    for (i, pa) in [(0u32, 0u32), (0x300, 0x3000_0000)] {
        bus_w(&mut m, ttb + i * 4, 4, pa | 0xC02); // セクション・AP=11・ドメイン 0
    }
    let s = &mut m.sys;
    s.cp15_write(0, 2, 0, 0, ttb);
    s.cp15_write(0, 3, 0, 0, 3); // ドメイン 0 はマネージャ
    s.cp15_write(0, 1, 0, 0, 1); // M
    assert_eq!(s.read(0x1000, 4).unwrap(), 0x1234_5678);
    assert!(s.ram_run(0x1000, 4, false).is_some(), "read directly");
    assert!(
        s.ram_run(0x1000, 4, true).is_none(),
        "never written directly"
    );

    let unlock = |s: &mut Sys, cmd: u32| {
        s.write(0xAAAA, 2, 0xAAAA).unwrap();
        s.write(0x5554, 2, 0x5555).unwrap();
        s.write(0xAAAA, 2, cmd).unwrap();
    };
    // 自動選択: 先頭の語だけ ID、他は中身
    unlock(s, 0x9090);
    assert_eq!(s.read(0, 2).unwrap(), 0x0001);
    assert_eq!(s.read(2, 2).unwrap(), 0x225B);
    assert_eq!(s.read(0x40, 4).unwrap(), u32::from_le_bytes(*b"ECEC"));
    assert_eq!(s.read(0x1000, 4).unwrap(), 0x1234_5678);
    s.write(0, 2, 0xF0F0).unwrap();
    assert_eq!(s.read(0, 4).unwrap(), 0xEA0003FE);

    // 書き込みは 1→0 だけ
    unlock(s, 0xA0A0);
    s.write(0x2000, 2, 0x0FFF).unwrap();
    assert_eq!(s.read(0x2000, 2).unwrap(), 0x0F0F);

    // セクタの消去はデコード済みのページを捨てさせる
    assert_eq!(s.read(0x10000, 4).unwrap(), 0xE1A0_0000);
    s.mmu.mark_code(0x10000);
    unlock(s, 0x8080);
    s.write(0xAAAA, 2, 0xAAAA).unwrap();
    s.write(0x5554, 2, 0x5555).unwrap();
    s.write(0x10004, 2, 0x3030).unwrap();
    assert_eq!(s.read(0x10000, 4).unwrap(), 0xFFFF_FFFF);
    assert_eq!(s.read(0x1FFFC, 4).unwrap(), 0xFFFF_FFFF);
    assert_eq!(s.read(0x2000, 2).unwrap(), 0x0F0F, "other sector kept");
    let mut inv = vec![];
    while let Some(p) = s.mmu.take_code_invalidated() {
        inv.push(p);
    }
    assert!(inv.contains(&0x10000), "{inv:X?}");

    // スナップショット（ID 読み出しの途中）: フラッシュごと戻る
    unlock(s, 0x9090);
    let mut buf = vec![];
    m.save_snapshot(&mut buf, "id").unwrap();
    let mut r = Machine::new();
    r.load_snapshot(&buf[..]).unwrap();
    assert_eq!(r.flash_size(), 0x30000);
    assert_eq!(r.sys.read(2, 2).unwrap(), 0x225B, "still in autoselect");
    assert_eq!(r.sys.read(0x2000, 2).unwrap(), 0x0F0F);
    let mut again = vec![];
    r.save_snapshot(&mut again, "id").unwrap();
    assert!(buf == again, "save after load must give the same bytes");
    // フラッシュのないスナップショットを読めば、フラッシュのない構成に戻る
    let mut plain = vec![];
    Machine::new().save_snapshot(&mut plain, "id").unwrap();
    r.load_snapshot(&plain[..]).unwrap();
    assert_eq!(r.flash_size(), 0);
    assert_eq!(bus_r(&mut r, 0, 4), 0, "open bus");
}
