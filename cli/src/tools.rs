//! 調査・計測用のサブコマンド（goldencmp・segspeed・ihist・genrate）。

use std::process::ExitCode;
use std::time::Instant;

use cerulean_core::emu::{self, Session};
use cerulean_core::script::{self, Event, Kind};
use cerulean_core::smdk2410::{INSTRUCTIONS_PER_SECOND, Machine};

use crate::{parse_rtc, parse_u64, read_image};

/// スナップショットを読み込んだマシン。
fn load_snap(path: &str) -> Result<Machine, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("{path}: {e}"))?;
    let mut m = Machine::new();
    m.load_snapshot(std::io::BufReader::new(f))
        .map_err(|e| format!("{path}: {e}"))?;
    Ok(m)
}

// ---- goldencmp ----

/// JSON Lines の 1 行から、キーの値（文字列ならクォートの中、数・オブジェクトは
/// そのまま）を取り出す。一致確認の結果（tmp/internal-docs/testdata/golden/README.md）の書式だけを
/// 読む最小の読み取り（serde を使わない）。
fn json_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let k = format!("\"{key}\":");
    let rest = &line[line.find(&k)? + k.len()..];
    if let Some(s) = rest.strip_prefix('"') {
        return s.split('"').next();
    }
    if rest.starts_with('{') {
        return rest.find('}').map(|i| &rest[..=i]);
    }
    rest.split([',', '}']).next()
}

/// dump（版数 1）の各語の名前（README の表の順。steps は u64）。
fn dump_fields() -> Vec<String> {
    let mut f = vec!["version".to_string(), "steps".to_string()];
    f.extend((0..16).map(|i| format!("r{i}")));
    for m in ["usr", "fiq"] {
        f.extend((8..=14).map(|i| format!("{m}_r{i}")));
    }
    for m in ["irq", "svc", "abt", "und"] {
        f.push(format!("{m}_r13"));
        f.push(format!("{m}_r14"));
    }
    f.push("cpsr".into());
    f.extend(
        ["fiq", "irq", "svc", "abt", "und"]
            .iter()
            .map(|m| format!("spsr_{m}")),
    );
    f.extend(
        ["c1", "c2", "c3", "c5", "c6", "c13"]
            .iter()
            .map(|c| format!("cp15_{c}")),
    );
    f
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// CPU 状態のダンプの食い違いをレジスタ名つきで並べる。
fn cpu_diff(key: &str, want: &str, got: &str) -> Vec<String> {
    let (Some(e), Some(a)) = (unhex(want), unhex(got)) else {
        return vec![format!("{key}:   (dump is not hex)")];
    };
    if e.len() != 212 || a.len() != 212 {
        return vec![format!(
            "{key}:   (dump not comparable: len {}/{})",
            e.len(),
            a.len()
        )];
    }
    let mut d = vec![];
    let mut off = 0;
    for name in dump_fields() {
        let size = if name == "steps" { 8 } else { 4 };
        let rd = |b: &[u8]| {
            b[off..off + size]
                .iter()
                .rev()
                .fold(0u64, |v, &x| v << 8 | x as u64)
        };
        let (ev, av) = (rd(&e), rd(&a));
        if ev != av {
            d.push(format!("{key}:   {name:<10} {av:08X}, want {ev:08X}"));
        }
        off += size;
    }
    d
}

/// goldencmp <expected.jsonl> <actual.jsonl>: 一致確認の結果を比べる。同じ
/// (event, steps) の行どうしを比べ、食い違った項目を表示する（CPU の差は
/// レジスタ名つき）。すべて一致すれば 0、食い違いがあれば 1。
pub fn cmd_goldencmp(args: &[String]) -> Result<ExitCode, String> {
    let [exp, act] = args else {
        return Err("usage: cerulean goldencmp <expected.jsonl> <actual.jsonl>".into());
    };
    let read = |p: &str| -> Result<Vec<String>, String> {
        let t = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
        let lines: Vec<String> = t
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(String::from)
            .collect();
        for (i, l) in lines.iter().enumerate() {
            if json_field(l, "format") != Some("1") {
                return Err(format!("{p}:{}: unsupported format", i + 1));
            }
        }
        Ok(lines)
    };
    let (exp, act) = (read(exp)?, read(act)?);
    let key = |l: &str| {
        format!(
            "{}@{}",
            json_field(l, "event").unwrap_or("?"),
            json_field(l, "steps").unwrap_or("?")
        )
    };
    let mut diffs = vec![];
    for e in &exp {
        let k = key(e);
        let Some(a) = act.iter().find(|a| key(a) == k) else {
            diffs.push(format!("{k}: missing in actual"));
            continue;
        };
        if json_field(e, "stop") != json_field(a, "stop") {
            diffs.push(format!(
                "{k}: stop {:?}, want {:?}",
                json_field(a, "stop"),
                json_field(e, "stop")
            ));
        }
        let (ec, ac) = (
            json_field(e, "cpu").unwrap_or(""),
            json_field(a, "cpu").unwrap_or(""),
        );
        if ec != ac {
            diffs.push(format!("{k}: cpu differs"));
            diffs.extend(cpu_diff(&k, ec, ac));
        }
        for f in [
            "ram_sha256",
            "uart1_sha256",
            "uart1_bytes",
            "screen_sha256",
            "screen_w",
            "screen_h",
        ] {
            if json_field(e, f) != json_field(a, f) {
                diffs.push(format!(
                    "{k}: {f} = {:?}, want {:?}",
                    json_field(a, f),
                    json_field(e, f)
                ));
            }
        }
    }
    for a in &act {
        if !exp.iter().any(|e| key(e) == key(a)) {
            diffs.push(format!("{}: not in expected", key(a)));
        }
    }
    if diffs.is_empty() {
        println!("match ({} records)", exp.len());
        return Ok(ExitCode::SUCCESS);
    }
    for d in diffs {
        println!("{d}");
    }
    Ok(ExitCode::from(1))
}

// ---- segspeed ----

/// segspeed [--seg 秒] <snapshot> <script>: スクリプトを再生し、仮想時間の一定区間
/// （既定 0.25 秒）ごとの実時間比・アイドル割合・実処理の命令/秒を表示する
/// （操作のどの区間が遅いかを見る）。shot/snap は無視し、quit で止まる（quit が
/// 無ければスクリプトの最後の後 1 秒で止まる）。
pub fn cmd_segspeed(args: &[String]) -> Result<ExitCode, String> {
    let (seg, rest) = match args {
        [f, v, rest @ ..] if f == "--seg" => (v.parse::<f64>().map_err(|_| "bad --seg")?, rest),
        _ => (0.25, args),
    };
    let [snap, script_path] = rest else {
        return Err("usage: cerulean segspeed [--seg sec] <snapshot> <script>".into());
    };
    let mut m = load_snap(snap)?;
    let src = std::fs::read_to_string(script_path).map_err(|e| format!("{script_path}: {e}"))?;
    let mut evs = script::parse(&src, INSTRUCTIONS_PER_SECOND).map_err(|e| e.to_string())?;
    let start = m.steps();
    evs.retain(|e| e.step >= start); // 再開点より前のイベントは適用済みとみなす
    let end = evs.iter().find(|e| e.kind == Kind::Quit).map_or_else(
        || evs.last().map_or(start, |e| e.step) + INSTRUCTIONS_PER_SECOND,
        |e| e.step,
    );
    let mut s = Session::new();
    s.schedule(evs);
    let ips = INSTRUCTIONS_PER_SECOND as f64;
    let seg_steps = (seg * ips) as u64;
    if seg_steps == 0 {
        return Err("--seg too small".into());
    }
    let mut apply = |m: &mut Machine, ev: &Event| -> Result<bool, String> {
        match ev.kind {
            Kind::Shot | Kind::Snap => Ok(false),
            Kind::Quit => Ok(true),
            _ => {
                println!("   >>> {} {} {} {}", ev.kind, ev.x, ev.y, ev.key);
                emu::apply_input(m, ev)
            }
        }
    };
    let total = Instant::now();
    loop {
        let (st, sk, w) = (m.steps(), m.idle_skipped(), Instant::now());
        let quit = s
            .run(&mut m, (st + seg_steps).min(end), &mut apply)
            .map_err(|e| e.to_string())?;
        let _ = m.take_uart1();
        let n = m.steps() - st;
        let el = w.elapsed().as_secs_f64();
        let busy = n - (m.idle_skipped() - sk);
        println!(
            "v={:6.2}s  ratio={:5.2}x  busy={:5.1}%  busyMIPS={:5.1}",
            (m.steps() - start) as f64 / ips,
            n as f64 / ips / el,
            100.0 * busy as f64 / n.max(1) as f64,
            busy as f64 / el / 1e6
        );
        if quit || m.steps() >= end {
            break;
        }
    }
    println!(
        "total {:.1}s wall for {:.1}s virtual",
        total.elapsed().as_secs_f64(),
        (m.steps() - start) as f64 / ips
    );
    Ok(ExitCode::SUCCESS)
}

// ---- ihist ----

/// ARM 命令の大まかな種類（特化・高速化の対象を選ぶための分類）。
fn class(w: u32) -> String {
    const OPN: [&str; 16] = [
        "and", "eor", "sub", "rsb", "add", "adc", "sbc", "rsc", "tst", "teq", "cmp", "cmn", "orr",
        "mov", "bic", "mvn",
    ];
    let cond = if w >> 28 != 0xE { "(cond)" } else { "" };
    let op = OPN[((w >> 21) & 0xF) as usize];
    let s = if w & (1 << 20) != 0 { "s" } else { "" };
    match (w >> 25) & 7 {
        0 if w & 0x0FFFFFF0 == 0x012FFF10 => "bx".into(),
        0 if w & 0x90 == 0x90 => if (w >> 5) & 3 == 0 {
            "mul/swp"
        } else {
            "ldrh/sb/sh"
        }
        .into(),
        0 if (8..=11).contains(&((w >> 21) & 0xF)) && w & (1 << 20) == 0 => "mrs/msr".into(),
        0 => {
            let kind = if w & 0x10 != 0 {
                "reg-regshift"
            } else if w & 0xFF0 != 0 {
                "reg-immshift"
            } else {
                "reg-lsl0"
            };
            format!("dp {op}{s} {kind}{cond}")
        }
        1 => format!("dp {op}{s} imm{cond}"),
        2 => format!(
            "ldst imm L={} B={} P={} W={}{cond}",
            (w >> 20) & 1,
            (w >> 22) & 1,
            (w >> 24) & 1,
            (w >> 21) & 1
        ),
        3 => format!("ldst reg L={}{cond}", (w >> 20) & 1),
        4 => format!("ldm/stm L={}{cond}", (w >> 20) & 1),
        5 => format!("{}{cond}", if w & (1 << 24) != 0 { "bl" } else { "b" }),
        _ => "cop/swi".into(),
    }
}

/// ihist [--count N] [--max-steps N] [--skip-page P] [--pairs] <snapshot>: スナップショットから
/// 1 命令ずつ実行し、実行した ARM 命令の種類の分布を数える（特化・高速化の対象を
/// 選ぶための調査用）。指定の 4KB ページ（既定は OAL のアイドルループの 0x800AF）と
/// Thumb 命令は数えない。命令語は表示用の読み出し（TLB を埋めない）で読む。
/// --pairs は、直前の命令の次のアドレスで続けて実行した 2 命令の組を数える
/// （スーパー命令の候補を選ぶ用）。--unjit は JIT-to-wasm の対象外の命令（と Thumb）だけを
/// 数え、全命令に対する割合を出す（段階5 で対象を広げる候補を選ぶ用）。
pub fn cmd_ihist(args: &[String]) -> Result<ExitCode, String> {
    let (mut count, mut max_steps, mut skip_page, mut snap) =
        (20_000_000u64, 300_000_000u64, 0x800AFu64, None);
    let (mut pairs, mut unjit) = (false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || {
            it.next()
                .ok_or(format!("{a} needs a value"))
                .and_then(|v| parse_u64(v))
        };
        match a.as_str() {
            "--count" => count = val()?,
            "--max-steps" => max_steps = val()?,
            "--skip-page" => skip_page = val()?,
            "--pairs" => pairs = true,
            "--unjit" => unjit = true,
            s => snap = Some(s.to_string()),
        }
    }
    let snap =
        snap.ok_or("usage: cerulean ihist [--count n] [--max-steps n] [--skip-page p] <snapshot>")?;
    let mut m = load_snap(&snap)?;
    let mut cnt = std::collections::BTreeMap::<String, u64>::new();
    let mut total = 0;
    let end = m.steps() + max_steps;
    // 直前に数えた命令（次のアドレス, 種類）
    let mut prev: Option<(u32, String)> = None;
    while m.steps() < end && total < count {
        let pc = m.cpu.pc();
        if unjit && (skip_page == 0 || (pc >> 12) as u64 != skip_page) {
            total += 1;
            let c = if m.cpu.thumb() {
                Some("thumb".to_string())
            } else {
                m.peek32(pc)
                    .filter(|&w| !cerulean_core::jit::supported_word(w))
                    .map(class)
            };
            if let Some(c) = c {
                *cnt.entry(c).or_default() += 1;
            }
        } else if !unjit
            && !m.cpu.thumb()
            && (skip_page == 0 || (pc >> 12) as u64 != skip_page)
            && let Some(w) = m.peek32(pc)
        {
            let c = class(w);
            if !pairs {
                *cnt.entry(c).or_default() += 1;
                total += 1;
            } else {
                if let Some((next, p)) = &prev
                    && *next == pc
                {
                    *cnt.entry(format!("{p} ; {c}")).or_default() += 1;
                }
                total += 1;
                prev = Some((pc.wrapping_add(4), c));
            }
        } else {
            prev = None;
        }
        m.step().map_err(|e| e.to_string())?;
        let _ = m.take_uart1();
    }
    let mut l: Vec<_> = cnt.into_iter().collect();
    l.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    for (k, v) in l.iter().take(40) {
        println!("{:6.2}% {k}", 100.0 * *v as f64 / total.max(1) as f64);
    }
    Ok(ExitCode::SUCCESS)
}

// ---- genrate ----

/// genrate [--rtc T] [--steps N] <image>: リセットから N 命令（既定 6 億）実行し、MMU の
/// 変換世代の増加頻度と、コードページの印付け・書き込み検出の回数、デコード済み
/// ページ数を表示する（デコードキャッシュの性能・メモリの調査用。世代が上がるたびに
/// CPU は実行中ページの記憶を捨てる）。
pub fn cmd_genrate(args: &[String]) -> Result<ExitCode, String> {
    let (mut rtc, mut steps, mut image) = ([2006, 1, 2, 15, 4, 5], 600_000_000u64, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--rtc" => rtc = parse_rtc(it.next().ok_or("--rtc needs a value")?)?,
            "--steps" => steps = parse_u64(it.next().ok_or("--steps needs a value")?)?,
            s => image = Some(s.to_string()),
        }
    }
    let image = image.ok_or("usage: cerulean genrate [--rtc time] [--steps n] <image>")?;
    let img = read_image(&image)?;
    let mut m = Machine::new();
    m.load_image(&img).map_err(|e| e.to_string())?;
    m.set_rtc(rtc[0], rtc[1], rtc[2], rtc[3], rtc[4], rtc[5]);
    m.reset();
    while m.steps() < steps {
        m.run_until((m.steps() + 100_000_000).min(steps))
            .map_err(|e| e.to_string())?;
        let _ = m.take_uart1();
    }
    let generation = m.sys.mmu.code_gen();
    let (marks, writes) = m.sys.mmu.code_stats();
    println!(
        "gen={generation} ({:.1} per 1000 instr) codeMarks={marks} codeWrites={writes} codePages={}",
        generation as f64 / steps as f64 * 1000.0,
        m.sys.code.page_count()
    );
    Ok(ExitCode::SUCCESS)
}

// ---- blockstat ----

/// blockstat [--steps N] <snapshot> [script]: 実行した命令列を動的な基本ブロック
/// （PC が連続する命令の列）に区切り、長さの分布と、ブロック内の命令の性質
/// （ロード/ストア・条件付き・PSR 操作）の割合を数える（段階4 の IR の設計の材料）。
/// アイドルループ（0x800AF000 台）と Thumb は数えない。
pub fn cmd_blockstat(args: &[String]) -> Result<ExitCode, String> {
    let (mut steps, mut rest) = (100_000_000u64, vec![]);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--steps" => steps = parse_u64(it.next().ok_or("--steps needs a value")?)?,
            s => rest.push(s.to_string()),
        }
    }
    let (snap, script_path) = match rest.as_slice() {
        [s] => (s.clone(), None),
        [s, p] => (s.clone(), Some(p.clone())),
        _ => return Err("usage: cerulean blockstat [--steps n] <snapshot> [script]".into()),
    };
    let mut m = load_snap(&snap)?;
    let mut s = Session::new();
    if let Some(p) = &script_path {
        let src = std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?;
        let start = m.steps();
        let evs = script::parse(&src, INSTRUCTIONS_PER_SECOND).map_err(|e| e.to_string())?;
        s.schedule(evs.into_iter().filter(|e| e.step >= start));
    }
    let mut apply = |m: &mut Machine, ev: &Event| -> Result<bool, String> {
        match ev.kind {
            Kind::Shot | Kind::Snap | Kind::Quit => Ok(false),
            _ => emu::apply_input(m, ev),
        }
    };
    let end = m.steps() + steps;
    let mut hist = [0u64; 65]; // ブロック長（64 以上はまとめる）ごとの命令数
    let (mut total, mut ldst, mut ldm, mut cond, mut psr, mut blocks) =
        (0u64, 0u64, 0u64, 0u64, 0u64, 0u64);
    let (mut cur_len, mut next_pc) = (0usize, u32::MAX);
    let flush = |len: &mut usize, hist: &mut [u64; 65], blocks: &mut u64| {
        if *len > 0 {
            hist[(*len).min(64)] += *len as u64;
            *blocks += 1;
            *len = 0;
        }
    };
    while m.steps() < end {
        let pc = m.cpu.pc();
        let arm_code = !m.cpu.thumb() && pc >> 12 != 0x800AF;
        if arm_code && let Some(w) = m.peek32(pc) {
            if pc != next_pc {
                flush(&mut cur_len, &mut hist, &mut blocks);
            }
            cur_len += 1;
            next_pc = pc.wrapping_add(4);
            total += 1;
            match (w >> 25) & 7 {
                2 | 3 => ldst += 1,
                0 if w & 0x90 == 0x90 && (w >> 5) & 3 != 0 => ldst += 1,
                4 => ldm += 1,
                0 | 1 if (8..=11).contains(&((w >> 21) & 0xF)) && w & (1 << 20) == 0 => psr += 1,
                _ => {}
            }
            if w >> 28 != 0xE {
                cond += 1;
            }
        } else {
            flush(&mut cur_len, &mut hist, &mut blocks);
            next_pc = u32::MAX;
        }
        let next = m.steps() + 1;
        s.run(&mut m, next, &mut apply).map_err(|e| e.to_string())?;
        let _ = m.take_uart1();
    }
    flush(&mut cur_len, &mut hist, &mut blocks);
    let pct = |n: u64| 100.0 * n as f64 / total.max(1) as f64;
    println!(
        "ARM instructions {total} in {blocks} dynamic blocks (avg {:.1})",
        total as f64 / blocks.max(1) as f64
    );
    println!(
        "load/store {:.1}%  ldm/stm {:.1}%  conditional {:.1}%  mrs/msr {:.1}%",
        pct(ldst),
        pct(ldm),
        pct(cond),
        pct(psr)
    );
    let mut acc = 0;
    for (len, n) in hist.iter().enumerate().skip(1) {
        acc += n;
        if [1, 2, 3, 4, 5, 6, 8, 10, 12, 16, 24, 32, 48, 64].contains(&len) {
            println!("  blocks <= {len:2}: {:5.1}% of instructions", pct(acc));
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// disasm: スナップショットの時点の仮想アドレスから命令を逆アセンブルする（調査用）。
/// 変換は保存時点の MMU（FCSE の PID を含む）で行う。
pub fn cmd_disasm(args: &[String]) -> Result<ExitCode, String> {
    let [snap, va, n] = args else {
        return Err("usage: cerulean disasm <snapshot> <va> <count>".into());
    };
    let mut m = load_snap(snap)?;
    let (va, n) = (parse_u64(va)? as u32, parse_u64(n)? as u32);
    for i in 0..n {
        let a = va.wrapping_add(i * 4);
        match m.peek32(a) {
            Some(w) => println!("{a:08X}  {w:08X}  {}", cerulean_core::arm::disasm(w, a)),
            None => println!("{a:08X}  ????????"),
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_fields() {
        let l = r#"{"format":1,"event":"stop","steps":12,"stop":{"kind":"quit"},"cpu":"ab","screen_w":0}"#;
        assert_eq!(json_field(l, "format"), Some("1"));
        assert_eq!(json_field(l, "event"), Some("stop"));
        assert_eq!(json_field(l, "steps"), Some("12"));
        assert_eq!(json_field(l, "stop"), Some(r#"{"kind":"quit"}"#));
        assert_eq!(json_field(l, "screen_w"), Some("0"));
        assert_eq!(json_field(l, "nope"), None);
    }

    #[test]
    fn cpu_diff_names_registers() {
        let mut a = vec![0u8; 212];
        let e = a.clone();
        a[12 + 4] = 7; // r1
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        let d = cpu_diff("k", &hex(&e), &hex(&a));
        assert_eq!(d, ["k:   r1         00000007, want 00000000"]);
    }
}
