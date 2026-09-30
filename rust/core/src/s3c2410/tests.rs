//! Go の device/s3c2410 のテスト（intc_timer・adc・lcd・rtc・stub・uart）を
//! 移したもの。入力と期待値は Go と同じ。

#![allow(clippy::type_complexity)]

use super::adc::*;
use super::intc::*;
use super::lcd::rgb565;
use super::rtc::*;
use super::*;

// ---- INTC ----

#[test]
fn intc_mask_and_deliver() {
    let mut ic = Intc::new();
    // リセット時は全マスク: raise しても IRQ 線は立たない
    ic.raise(INT_TIMER4);
    assert!(!ic.irq());
    assert_eq!(ic.read(REG_SRCPND, 4), 1 << INT_TIMER4);
    // マスク解除 → IRQ 線が立ち、INTPND/INTOFFSET が確定
    ic.write(REG_INTMSK, 4, !(1 << INT_TIMER4));
    assert!(ic.irq() && !ic.fiq());
    assert_eq!(ic.read(REG_INTOFFSET, 4), INT_TIMER4);
    assert_eq!(ic.read(REG_INTPND, 4), 1 << INT_TIMER4);
    // ハンドラのクリア手順: SRCPND → INTPND に 1 を書く
    ic.write(REG_SRCPND, 4, 1 << INT_TIMER4);
    ic.write(REG_INTPND, 4, 1 << INT_TIMER4);
    assert!(!ic.irq());
    assert_eq!(ic.read(REG_SRCPND, 4), 0);
}

#[test]
fn intc_fixed_priority() {
    let mut ic = Intc::new();
    ic.write(REG_INTMSK, 4, 0); // 全部マスク解除
    ic.raise(INT_UART0); // 28
    ic.raise(INT_TIMER0); // 10 ← 小さい方が勝つ
    assert_eq!(ic.read(REG_INTOFFSET, 4), INT_TIMER0);
    // Timer0 をクリアすると UART0 が選ばれる
    ic.write(REG_SRCPND, 4, 1 << INT_TIMER0);
    assert_eq!(ic.read(REG_INTOFFSET, 4), INT_UART0);
    assert!(ic.irq());
}

#[test]
fn intc_fiq() {
    let mut ic = Intc::new();
    ic.write(REG_INTMOD, 4, 1 << INT_TIMER1); // Timer1 を FIQ に
    ic.raise(INT_TIMER1);
    // FIQ は INTMSK に関係なく立ち、IRQ 側（INTPND）には現れない
    assert!(ic.fiq() && !ic.irq());
    assert_eq!(ic.read(REG_INTPND, 4), 0);
}

#[test]
fn intc_sub_source() {
    let mut ic = Intc::new();
    ic.write(REG_INTMSK, 4, !(1 << INT_UART1));
    ic.write(REG_INTSUBMSK, 4, !(1 << SUB_RXD1));
    ic.raise_sub_mask(1 << SUB_RXD1);
    assert!(ic.irq());
    assert_eq!(ic.read(REG_INTOFFSET, 4), INT_UART1);
    // SRCPND だけクリアしてもサブが残っていれば立て直される
    ic.write(REG_SRCPND, 4, 1 << INT_UART1);
    assert_eq!(ic.read(REG_SRCPND, 4), 1 << INT_UART1);
    // サブをクリアすれば落ちる
    ic.write(REG_SUBSRCPND, 4, 1 << SUB_RXD1);
    ic.write(REG_SRCPND, 4, 1 << INT_UART1);
    assert!(!ic.irq());
}

// ---- PWM タイマー ----

/// advance の戻り値（満了したタイマーのビット）を満了の記録として数える。
fn fired(mask: u32, log: &mut Vec<u32>) {
    for n in 0..5 {
        if mask & (1 << n) != 0 {
            log.push(n);
        }
    }
}

#[test]
fn timer4_periodic_interrupt() {
    let mut tm = PwmTimer::new();
    let mut log = vec![];
    // PCLK/2、プリスケーラ 0 → 1 カウント = 2 PCLK。TCNTB4=100 → 最初の満了は
    // 100 カウント（200 PCLK）後、以降の周期は TCNTB+1 = 101 カウント（202 PCLK）。
    tm.write(0x00, 4, 0);
    tm.write(0x04, 4, 0);
    tm.write(0x3C, 4, 100); // TCNTB4
    tm.write(0x08, 4, 1 << 21); // Timer4 マニュアルアップデート
    tm.write(0x08, 4, 1 << 20 | 1 << 22); // スタート + 自動リロード
    fired(tm.advance(199), &mut log);
    assert!(log.is_empty(), "fired too early");
    fired(tm.advance(1), &mut log);
    assert_eq!(log, [4]);
    // 自動リロードで周期的に発火する（周期 202 PCLK）
    fired(tm.advance(201), &mut log);
    assert_eq!(log.len(), 1, "period is TCNTB+1 counts");
    fired(tm.advance(1), &mut log);
    assert_eq!(log.len(), 2);
    fired(tm.advance(202), &mut log);
    assert_eq!(log.len(), 3);
}

#[test]
fn timer_one_shot_stops() {
    let mut tm = PwmTimer::new();
    tm.write(0x0C, 4, 10); // TCNTB0
    tm.write(0x08, 4, 1 << 1); // Timer0 マニュアルアップデート
    tm.write(0x08, 4, 1 << 0); // スタート（自動リロードなし）
    assert_eq!(tm.advance(1000), 1 << 0, "one-shot");
    assert_eq!(tm.next_event(), NO_EVENT);
}

#[test]
fn timer_tcnto_readback() {
    let mut tm = PwmTimer::new();
    tm.write(0x3C, 4, 100);
    tm.write(0x08, 4, 1 << 21);
    tm.write(0x08, 4, 1 << 20 | 1 << 22);
    tm.advance(60); // 1 カウント = 2 PCLK → 30 カウント経過
    assert_eq!(tm.read(0x40, 4), 70);
}

#[test]
fn timer_prescaler_scale() {
    let mut tm = PwmTimer::new();
    // Timer2: プリスケーラ1 = 9 (÷10)、mux 1/4 → 1 カウント = 40 PCLK
    tm.write(0x00, 4, 9 << 8);
    tm.write(0x04, 4, 1 << 8);
    tm.write(0x24, 4, 5); // TCNTB2 → 周期 200 PCLK
    tm.write(0x08, 4, 1 << 13);
    tm.write(0x08, 4, 1 << 12 | 1 << 15);
    assert_eq!(tm.advance(199), 0, "fired too early");
    assert_eq!(tm.advance(1), 1 << 2);
}

// ---- ADC ----

/// 戻り値のサブソースの束から INT_TC の回数を数える。
fn tc(mask: u32) -> u32 {
    (mask >> SUB_TC) & 1
}

#[test]
fn adc_pen_down_interrupt() {
    // (名前, ADCTSC, ペン操作（INT_TC の回数を返す）, INT_TC の回数)
    let cases: &[(&str, u32, fn(&mut Adc) -> u32, u32)] = &[
        (
            "wait down, pen down",
            0xD3,
            |a| tc(a.set_pen(true, 1, 2)),
            1,
        ),
        (
            "wait down, down then up",
            0xD3,
            |a| tc(a.set_pen(true, 1, 2)) + tc(a.set_pen(false, 0, 0)),
            1,
        ),
        (
            "wait down, move while down",
            0xD3,
            |a| tc(a.set_pen(true, 1, 2)) + tc(a.set_pen(true, 3, 4)),
            1,
        ),
        // ペンアップ状態でアップ検出待ちに入った時点の 1 回だけ（ダウンでは出ない）
        (
            "wait up (UD_SEN), pen down",
            0x1D3,
            |a| a.set_pen(true, 1, 2),
            1,
        ),
        (
            "no-op mode, pen down",
            0xD0,
            |a| tc(a.set_pen(true, 1, 2)),
            0,
        ),
        ("X mode, pen down", 0xD1, |a| tc(a.set_pen(true, 1, 2)), 0),
    ];
    for &(name, adctsc, setup, want) in cases {
        let mut a = Adc::new();
        let n = tc(a.write(REG_ADCTSC, 4, adctsc)) + setup(&mut a);
        assert_eq!(n, want, "{name}");
    }
}

/// 既にペンダウンの状態で割り込み待ちモードに入ると INT_TC が出る。
#[test]
fn adc_enter_wait_while_pen_down() {
    let mut a = Adc::new();
    assert_eq!(a.set_pen(true, 1, 2), 0);
    assert_eq!(a.write(REG_ADCTSC, 4, 0xD3), 1 << SUB_TC);
}

/// touch.dll の実際の手順: ダウン検出 → サンプリング（0xDC）→ bit8=1 で
/// 割り込み待ち（0x1D3）→ ペンアップで INT_TC。
#[test]
fn adc_pen_up_interrupt() {
    let mut a = Adc::new();
    let log = [
        a.write(REG_ADCTSC, 4, 0xD3),
        a.set_pen(true, 1, 2),
        a.write(REG_ADCTSC, 4, 0xDC),
        a.write(REG_ADCTSC, 4, 0x1D3),
    ];
    assert_eq!(log.iter().map(|&m| tc(m)).sum::<u32>(), 1, "after down");
    assert_eq!(
        a.set_pen(false, 0, 0),
        1 << SUB_TC,
        "pen up in UD_SEN wait mode"
    );
}

/// サンプリング中（割り込み待ちでない間）にペンが上がっても、bit8=1 の
/// 割り込み待ちに入った時点で INT_TC が出る（取りこぼさない）。
#[test]
fn adc_pen_up_during_sampling() {
    let mut a = Adc::new();
    a.set_pen(true, 1, 2);
    assert_eq!(a.write(REG_ADCTSC, 4, 0xDC), 0);
    assert_eq!(a.set_pen(false, 0, 0), 0, "INT_TC raised outside wait mode");
    assert_eq!(a.write(REG_ADCTSC, 4, 0x1D3), 1 << SUB_TC);
}

#[test]
fn adc_conversion() {
    // (名前, ADCTSC, DAT0 期待, DAT1 期待, 変換時間: ADCDLY=100、PRSCVL=49 → 1 回 100+250)
    let cases: &[(&str, u32, u32, u32, i64)] = &[
        ("auto sequential X/Y", 0x0C, 0x123, 0x2AB, 700),
        ("X only", 0x69, 0x123, 0, 350),
        ("Y only", 0x9A, 0, 0x2AB, 350),
    ];
    for &(name, adctsc, want0, want1, ticks) in cases {
        let mut a = Adc::new();
        a.set_pen(true, 0x123, 0x2AB);
        a.write(REG_ADCTSC, 4, adctsc);
        a.write(REG_ADCDLY, 4, 100);
        a.write(REG_ADCCON, 4, ADCCON_PRSCEN | 49 << 6 | ADCCON_ENABLESTART);
        let v = a.read(REG_ADCCON, 4);
        assert!(
            v & ADCCON_ECFLG == 0 && v & ADCCON_ENABLESTART == 0,
            "{name}: ADCCON right after start = {v:08X}"
        );
        assert_eq!(a.advance(ticks - 1), 0, "{name}: completed too early");
        assert_eq!(a.read(REG_ADCCON, 4) & ADCCON_ECFLG, 0);
        assert_eq!(a.advance(1), 1 << SUB_ADC, "{name}");
        assert_ne!(a.read(REG_ADCCON, 4) & ADCCON_ECFLG, 0);
        let (d0, d1) = (a.read(REG_ADCDAT0, 4), a.read(REG_ADCDAT1, 4));
        assert_eq!((d0 & 0x3FF, d1 & 0x3FF), (want0, want1), "{name}");
        assert_eq!(d0 & DAT_UPDOWN, 0, "{name}: UPDOWN says pen up while down");
        // 状態ビット: AUTO_PST[14]・XY_PST[13:12] は ADCTSC の写し。
        assert_eq!((d0 >> 12) & 7, adctsc & 7, "{name}");
    }
}

#[test]
fn adc_pen_up_flag() {
    let mut a = Adc::new();
    assert!(a.read(REG_ADCDAT0, 4) & DAT_UPDOWN != 0 && a.read(REG_ADCDAT1, 4) & DAT_UPDOWN != 0);
    a.set_pen(true, 0, 0);
    assert_eq!(a.read(REG_ADCDAT0, 4) & DAT_UPDOWN, 0);
}

#[test]
fn adc_read_start() {
    let mut a = Adc::new();
    a.write(REG_ADCDLY, 4, 10);
    a.write(REG_ADCCON, 4, ADCCON_READSTART); // プリスケーラ無効: 10+5 ティック
    a.read(REG_ADCDAT0, 4);
    a.advance(15);
    assert_ne!(
        a.read(REG_ADCCON, 4) & ADCCON_ECFLG,
        0,
        "READ_START did not start a conversion"
    );
}

/// stable_read は read と同じ値を返し、読み出しで変換が始まる場合
/// （READ_START 有効時の ADCDAT0）だけ断る。
#[test]
fn adc_stable_read() {
    let mut a = Adc::new();
    a.write(REG_ADCCON, 4, 1); // ENABLE_START
    for off in [REG_ADCCON, REG_ADCTSC, REG_ADCDLY, REG_ADCDAT0, REG_ADCDAT1] {
        assert_eq!(a.stable_read(off, 4), Some(a.read(off, 4)), "{off:X}");
    }
    // 変換中は ECFLG=0、完了後は 1（値は変換完了のイベントでだけ変わる）。
    assert_eq!(a.stable_read(REG_ADCCON, 4).unwrap() & ADCCON_ECFLG, 0);
    a.advance(a.next_event());
    assert_ne!(a.stable_read(REG_ADCCON, 4).unwrap() & ADCCON_ECFLG, 0);
    a.write(REG_ADCCON, 4, ADCCON_READSTART);
    assert_eq!(a.stable_read(REG_ADCDAT0, 4), None);
    assert_eq!(a.converting, 0, "stable_read started a conversion");
}

// ---- LCD ----

/// TFT・bpp・解像度・フレームバッファを指定してレジスタを書く。
fn lcd_setup(l: &mut Lcd, bppmode: u32, w: u32, h: u32, base: u32, con5: u32, offsize: u32) {
    let pagewidth = match bppmode {
        0xD => w * 2,
        0xB => w / 2,
        _ => w, // 16bpp: 1 ピクセル = 1 ハーフワード
    };
    l.write(0x00, 4, 3 << 5 | bppmode << 1 | 1); // TFT・ENVID=1
    l.write(0x04, 4, (h - 1) << 14);
    l.write(0x08, 4, (w - 1) << 8);
    l.write(0x10, 4, con5);
    l.write(0x14, 4, (base >> 22) << 21 | (base >> 1) & 0x1FFFFF);
    l.write(0x1C, 4, offsize << 11 | pagewidth);
}

#[test]
fn lcd_config() {
    let base = LcdConfig {
        enabled: true,
        tft: true,
        ..Default::default()
    };
    let cases: &[(&str, u32, u32, u32, u32, u32, u32, LcdConfig)] = &[
        (
            "WM5 想定 240x320 16bpp 565 HWSWP",
            0xC,
            240,
            320,
            0x30100000,
            1 << 11 | 1,
            0,
            LcdConfig {
                bpp: 16,
                width: 240,
                height: 320,
                base: 0x30100000,
                stride: 480,
                hw_swap: true,
                frm565: true,
                page_width: 240,
                ..base
            },
        ),
        (
            "仮想画面（OFFSIZE あり）",
            0xC,
            240,
            320,
            0x33F00000,
            0,
            16,
            LcdConfig {
                bpp: 16,
                width: 240,
                height: 320,
                base: 0x33F00000,
                stride: 512,
                page_width: 240,
                off_size: 16,
                ..base
            },
        ),
        (
            "24bpp BPP24BL",
            0xD,
            640,
            480,
            0x31000000,
            1 << 12,
            0,
            LcdConfig {
                bpp: 24,
                width: 640,
                height: 480,
                base: 0x31000000,
                stride: 2560,
                bpp24_low: true,
                page_width: 1280,
                ..base
            },
        ),
        (
            "8bpp パレット",
            0xB,
            320,
            240,
            0x30000000,
            0,
            0,
            LcdConfig {
                bpp: 8,
                width: 320,
                height: 240,
                base: 0x30000000,
                stride: 320,
                page_width: 160,
                ..base
            },
        ),
    ];
    for &(name, bppmode, w, h, fb, con5, offsize, want) in cases {
        let mut l = Lcd::new();
        lcd_setup(&mut l, bppmode, w, h, fb, con5, offsize);
        assert_eq!(l.config(), want, "{name}");
    }
}

#[test]
fn lcd_config_disabled_and_stn() {
    let mut l = Lcd::new();
    l.write(0x00, 4, 2 << 5 | 0xC << 1); // PNRMODE=10（STN 8bit）・ENVID=0
    let c = l.config();
    assert!(!c.enabled && !c.tft && c.bpp == 0);
    assert!(l.frame(|_| Ok(0)).is_err());
}

#[test]
fn lcd_read_only_fields() {
    let mut l = Lcd::new();
    l.write(0x00, 4, 0xFFFFFFFF);
    assert_eq!(
        l.read(0x00, 4) & (0x3FF << 18),
        0,
        "LINECNT must be read-only"
    );
    l.write(0x10, 4, 0xFFFFFFFF);
    assert_eq!(
        l.read(0x10, 4) & (0xF << 13),
        0,
        "VSTATUS/HSTATUS must be read-only"
    );
    l.write(0x400 + 4 * 5, 4, 0xF800);
    assert_eq!(l.read(0x400 + 4 * 5, 4), 0xF800);
}

/// 物理アドレス→ワードの疎なメモリ。
fn fake_mem(words: &[(u32, u32)]) -> impl FnMut(u32) -> Result<u32, crate::bus::BusError> + '_ {
    move |pa| Ok(words.iter().find(|w| w.0 == pa).map_or(0, |w| w.1))
}

fn px(f: &Frame, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * f.width + x) * 4) as usize;
    f.rgba[i..i + 4].try_into().unwrap()
}

#[test]
fn lcd_frame16() {
    const RED: [u8; 4] = [0xFF, 0, 0, 0xFF];
    const GREEN: [u8; 4] = [0, 0xFF, 0, 0xFF];
    const BLUE: [u8; 4] = [0, 0, 0xFF, 0xFF];
    const WHITE: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];
    // 4x2 画像、ワード = [上位ハーフ | 下位ハーフ]。
    let mem = [
        (0x30000000, 0xF800 << 16 | 0x07E0),
        (0x30000004, 0x001F << 16 | 0xFFFF),
        // 2 行目（OFFSIZE=2 ハーフワードで stride=12 バイト）
        (0x3000000C, 0xFFFF << 16 | 0x001F),
    ];
    for (name, con5, row0) in [
        // スワップなし: 上位ハーフワードが左ピクセル。
        ("no swap", 1 << 11, [RED, GREEN, BLUE, WHITE]),
        // HWSWP: 下位ハーフワードが左ピクセル（LE の自然な並び）。
        ("HWSWP", 1 << 11 | 1, [GREEN, RED, WHITE, BLUE]),
    ] {
        let mut l = Lcd::new();
        lcd_setup(&mut l, 0xC, 4, 2, 0x30000000, con5, 2);
        let (f, _) = l.frame(fake_mem(&mem)).unwrap();
        for (x, want) in row0.into_iter().enumerate() {
            assert_eq!(px(&f, x as u32, 0), want, "{name} ({x},0)");
        }
        // 2 行目の先頭は stride 分ずれた位置から読まれる。
        assert_eq!(
            px(&f, 0, 1),
            if con5 & 1 != 0 { BLUE } else { WHITE },
            "{name}"
        );
    }
}

#[test]
fn lcd_frame24_and_palette() {
    let mut l = Lcd::new();
    lcd_setup(&mut l, 0xD, 2, 1, 0x30000000, 0, 0);
    let (f, _) = l
        .frame(fake_mem(&[
            (0x30000000, 0x123456),
            (0x30000004, 0xFF000000),
        ]))
        .unwrap();
    assert_eq!(px(&f, 0, 0), [0x12, 0x34, 0x56, 0xFF]);
    assert_eq!(
        px(&f, 1, 0),
        [0, 0, 0, 0xFF],
        "上位 8 ビットは無視されるはず"
    );
    // 8bpp: 1 ワードに 4 ピクセル、最上位バイトが左。
    let mut l = Lcd::new();
    lcd_setup(&mut l, 0xB, 4, 1, 0x30000000, 1 << 11, 0);
    l.write(0x400 + 4, 4, 0xF800); // 1 = 赤
    l.write(0x400 + 8, 4, 0x001F); // 2 = 青
    let (f, _) = l.frame(fake_mem(&[(0x30000000, 0x01020001)])).unwrap();
    let want = [
        [0xFF, 0, 0, 0xFF],
        [0, 0, 0xFF, 0xFF],
        [0, 0, 0, 0xFF],
        [0xFF, 0, 0, 0xFF],
    ];
    for (x, w) in want.into_iter().enumerate() {
        assert_eq!(px(&f, x as u32, 0), w, "8bpp ({x},0)");
    }
}

#[test]
fn rgb565_expand() {
    for (p, is565, want) in [
        (0xFFFF, true, [0xFF, 0xFF, 0xFF, 0xFF]),
        (0x0000, true, [0, 0, 0, 0xFF]),
        (0x8410, true, [0x84, 0x82, 0x84, 0xFF]), // 中間灰（ビット複製の確認）
        (0xFFFF, false, [0xFF, 0xFF, 0xFF, 0xFF]),
        (0xF800, false, [0xF8 | 0x3, 0, 0, 0xFF]), // 5:5:5:I、I=0 → R=111110 → 0xFB
        (0x0001, false, [0x04, 0x04, 0x04, 0xFF]),
    ] {
        assert_eq!(rgb565(p, is565), want, "{p:04X} {is565}");
    }
}

// ---- RTC ----

#[test]
fn rtc_read() {
    let mut r = Rtc::new(1000); // 1 秒 = 1000 ティック
    r.set_time(2006, 12, 31, 23, 59, 58);
    let check = |r: &Rtc, label: &str, want: &[(u32, u32)]| {
        for &(off, w) in want {
            assert_eq!(r.read(off, 4), w, "{label}: reg {off:02X}");
        }
    };
    // 2006-12-31 は日曜（BCDDAY=1）
    check(
        &r,
        "start",
        &[
            (0x70, 0x58),
            (0x74, 0x59),
            (0x78, 0x23),
            (0x7C, 0x31),
            (0x80, 0x01),
            (0x84, 0x12),
            (0x88, 0x06),
        ],
    );
    r.advance(999); // 1 秒未満: 変わらない
    check(&r, "+0.999s", &[(0x70, 0x58)]);
    r.advance(1001); // 計 2 秒: 年をまたぐ
    check(
        &r,
        "+2s",
        &[
            (0x70, 0x00),
            (0x74, 0x00),
            (0x78, 0x00),
            (0x7C, 0x01),
            (0x80, 0x02),
            (0x84, 0x01),
            (0x88, 0x07),
        ],
    );
}

#[test]
fn rtc_write() {
    let mut r = Rtc::new(1000);
    r.set_time(2006, 1, 2, 3, 4, 5);
    r.write(0x78, 4, 0x15); // RTCEN=0: 無視される
    assert_eq!(r.read(0x78, 4), 0x03);
    r.write(0x40, 4, 1);
    r.write(0x88, 4, 0x08);
    r.write(0x84, 4, 0x02);
    r.write(0x7C, 4, 0x29); // 2008 はうるう年
    r.write(0x78, 4, 0x15);
    assert_eq!(r.now(), unix_to_date(date_to_unix(2008, 2, 29, 15, 4, 5)));
    r.advance(1000);
    assert_eq!(r.read(0x70, 4), 0x06);
}

#[test]
fn rtc_write_keeps_subsecond_ticks() {
    let mut r = Rtc::new(1000);
    r.set_time(2006, 1, 2, 3, 4, 5);
    r.advance(2500);
    r.write(0x40, 4, 1);
    r.write(0x74, 4, 0x30); // 分を 30 に
    assert_eq!(r.elapsed, 500, "秒未満の端数を保つ");
    assert_eq!((r.now().minute, r.now().second), (30, 7));
}

#[test]
fn bcd() {
    for v in [0, 9, 10, 45, 99] {
        assert_eq!(from_bcd(to_bcd(v)), v);
    }
    assert_eq!(to_bcd(59), 0x59);
    assert_eq!(
        from_bcd(0xFF),
        165,
        "不正な BCD もそのまま計算する（Go と同じ）"
    );
}

/// Go の time.Date(...).Unix() と、その日時の年月日時分秒・曜日（Go で計算した値）。
/// 範囲外の値の正規化（2 月 31 日、13 月、0 日、24 時など）を含む。
#[test]
fn calendar_matches_go() {
    let cases: &[(
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
        (i64, u32, u32, u32, u32, u32, u32),
    )] = &[
        (2006, 1, 2, 15, 4, 5, 1136214245, (2006, 1, 2, 15, 4, 5, 1)),
        (
            2008,
            2,
            29,
            15,
            4,
            5,
            1204297445,
            (2008, 2, 29, 15, 4, 5, 5),
        ),
        (2006, 2, 31, 0, 0, 0, 1141344000, (2006, 3, 3, 0, 0, 0, 5)),
        (2006, 13, 1, 0, 0, 0, 1167609600, (2007, 1, 1, 0, 0, 0, 1)),
        (2006, 0, 0, 0, 0, 0, 1133308800, (2005, 11, 30, 0, 0, 0, 3)),
        (
            2099,
            12,
            31,
            23,
            59,
            59,
            4102444799,
            (2099, 12, 31, 23, 59, 59, 4),
        ),
        (2000, 1, 1, 0, 0, 0, 946684800, (2000, 1, 1, 0, 0, 0, 6)),
        (
            2165,
            16,
            45,
            99,
            165,
            165,
            6197147265,
            (2166, 5, 19, 5, 47, 45, 1),
        ),
        (
            1999,
            12,
            31,
            24,
            60,
            60,
            946688460,
            (2000, 1, 1, 1, 1, 0, 6),
        ),
        (2100, 2, 29, 0, 0, 0, 4107542400, (2100, 3, 1, 0, 0, 0, 1)),
    ];
    for &(y, mo, d, h, mi, s, unix, (wy, wmo, wd, wh, wmi, ws, wwd)) in cases {
        assert_eq!(
            date_to_unix(y, mo, d, h, mi, s),
            unix,
            "{y}-{mo}-{d} {h}:{mi}:{s}"
        );
        let t = unix_to_date(unix);
        assert_eq!(
            (
                t.year, t.month, t.day, t.hour, t.minute, t.second, t.weekday
            ),
            (wy, wmo, wd, wh, wmi, ws, wwd),
            "{unix}"
        );
    }
}

/// 暦の変換の往復（1900〜2200 年の毎日）。
#[test]
fn calendar_round_trip() {
    let start = date_to_unix(1900, 1, 1, 0, 0, 0) / 86400;
    let end = date_to_unix(2200, 1, 1, 0, 0, 0) / 86400;
    let mut prev = unix_to_date((start - 1) * 86400);
    for day in start..end {
        let t = unix_to_date(day * 86400);
        assert_eq!(
            date_to_unix(t.year, t.month as i64, t.day as i64, 0, 0, 0),
            day * 86400
        );
        assert_eq!(t.weekday, (prev.weekday + 1) % 7);
        prev = t;
    }
}

// ---- 値保持スタブ ----

#[test]
fn stub_read_write() {
    let mut s = Stub::new(&[(0x10, 0xDEADBEEF)]);
    for (name, off, size, want) in [
        ("初期値ワード", 0x10, 4, 0xDEADBEEF),
        ("初期値バイト0", 0x10, 1, 0xEF),
        ("初期値バイト3", 0x13, 1, 0xDE),
        ("初期値ハーフ下位", 0x10, 2, 0xBEEF),
        ("初期値ハーフ上位", 0x12, 2, 0xDEAD),
        ("未書き込みは0", 0x20, 4, 0),
    ] {
        assert_eq!(s.read(off, size), want, "{name}");
    }
    s.write(0x00, 4, 0x11223344);
    assert_eq!(s.read(0x00, 4), 0x11223344);
    s.write(0x01, 1, 0xAA); // バイト書き込みは該当バイトのみ
    assert_eq!(s.read(0x00, 4), 0x1122AA44);
    s.write(0x02, 2, 0x5566); // ハーフ書き込みは該当ハーフのみ
    assert_eq!(s.read(0x00, 4), 0x5566AA44);
    // forced は読み出しにだけ OR される
    let s = Stub::new(&[]).force_read_bits(0x00, 1 << 7);
    assert_eq!(s.read(0x00, 4), 0x80);
}

// ---- UART ----

#[test]
fn uart_transmit() {
    let mut u = Uart::new(true);
    for ch in b"Hi\n" {
        u.write(0x20, 1, *ch as u32);
    }
    assert_eq!(u.take_tx(), b"Hi\n");
    assert!(u.take_tx().is_empty());
    let mut quiet = Uart::new(false);
    quiet.write(0x20, 1, b'x' as u32);
    assert!(quiet.take_tx().is_empty());
}

#[test]
fn uart_status_and_config() {
    let mut u = Uart::new(false);
    // bit1 (TX buffer empty) と bit2 (transmitter empty) が立ち、RX ready (bit0) は立たない
    assert_eq!(u.read(0x10, 4) & 0x7, 0x6);
    u.write(0x00, 4, 0x3);
    u.write(0x28, 4, 0x1A);
    assert_eq!((u.read(0x00, 4), u.read(0x28, 4)), (0x3, 0x1A));
}

// ---- SPI・DMA ----

struct Echo;

impl SpiSlaves for Echo {
    fn transfer(&mut self, ch: usize, tx: u8) -> Option<u8> {
        (ch == 1).then_some(tx ^ 0xFF)
    }
}

#[test]
fn spi_transfer_and_interrupt() {
    let mut s = Spi::new();
    assert_eq!(s.read(0x04, 4), 1, "REDY は常に 1");
    assert_eq!(s.read(0x08, 4), 0x02, "SPPIN のリセット値");
    // チャネル 1: ポーリングモードでは割り込みなし
    assert_eq!(s.write(0x30, 4, 0x5A, &mut Echo), None);
    assert_eq!(s.read(0x34, 4), 0xA5);
    // 割り込みモード（SMOD=01）
    s.write(0x20, 4, 1 << 5, &mut Echo);
    assert_eq!(s.write(0x30, 4, 0x00, &mut Echo), Some(1));
    // チャネル 0 には何もつながっていない: 0 を受信
    assert_eq!(s.write(0x10, 4, 0x77, &mut Echo), None);
    assert_eq!(s.read(0x14, 4), 0);
}

/// 音声ドライバ（s3c2410x_wavedev.dll）と同じ設定（2026-09-30 の --watch）で
/// DMA チャネル 2 と IIS を動かす。
fn wavedev_setup() -> (Dma, Iis) {
    let (mut d, mut i) = (Dma::new(), Iis::new());
    let ft = i.frame_ticks();
    i.write(0x00, 4, 0x02); // IISCON: PSEN
    i.write(0x04, 4, 0xAD); // IISMOD: 送信・16 ビット・384fs・32fs
    i.write(0x08, 4, 0x42); // IISPSR: A=B=2
    i.write(0x0C, 4, 0xA000); // IISFCON: 送信 FIFO・DMA
    i.write(0x00, 4, 0xA2); // IISCON: 送信の DMA 要求
    d.write(0x80, 4, 0x339D_0000, ft); // DISRC2
    d.write(0x80 + 0x04, 4, 0, ft); // DISRCC2: AHB・増加
    d.write(0x80 + 0x08, 4, 0x5500_0010, ft); // DIDST2: IISFIFO
    d.write(0x80 + 0x0C, 4, 3, ft); // DIDSTC2: APB・固定
    d.write(0x80 + 0x10, 4, 0xA090_0400, ft); // DCON2
    (d, i)
}

#[test]
fn iis_dma_playback() {
    let (mut d, mut i) = wavedev_setup();
    assert_eq!(i.half_ticks(), 576, "(A+1)*384/2");
    assert_eq!(d.read(0x94, 4), 0, "CURR_TC before ON");
    // ON: IIS が止まっていても FIFO の空きで要求が出て 32 項目埋まる
    d.write(0xA0, 4, 2, i.frame_ticks());
    assert_eq!(d.service_iis_tx(&mut i), 0);
    assert_eq!(i.read(0x0C, 4) >> 6 & 0x3F, 32, "TX FIFO count");
    assert_ne!(i.read(0x00, 4) & 0x80, 0, "TXFR (not empty)");
    assert_eq!(
        d.read(0x94, 4),
        1 << 20 | (0x400 - 32),
        "DSTAT busy + CURR_TC"
    );
    assert_eq!(d.read(0x98, 4), 0x339D_0000 + 64, "DCSRC");
    assert_eq!(d.next_event(&i), NO_EVENT, "IIS not started");
    // 次のバッファを設定して IIS を始める
    d.write(0x80, 4, 0x339D_0800, i.frame_ticks());
    i.write(0x00, 4, 0xA3);
    let due = d.next_event(&i);
    assert_eq!(due, 576 * (0x400 - 32));
    assert_eq!(d.advance(&mut i, due - 1), 0);
    assert_eq!(d.read(0x94, 4) & 0xFFFFF, 1);
    assert_eq!(d.advance(&mut i, 1), 1 << 2, "INT_DMA2 at CURR_TC=0");
    assert_eq!(d.read(0x94, 4), 0, "CURR_TC=0 until the next request");
    assert_eq!(
        d.ready,
        vec![Segment {
            src: 0x339D_0000,
            units: 0x400,
            size: 2,
            inc: true,
            frame_ticks: 1152,
        }]
    );
    assert!(d.read(0xA0, 4) & 2 != 0, "auto reload keeps ON");
    // 次の送り出しで自動リロード（新しい DISRC から）
    assert_eq!(d.next_event(&i), 576 * 0x400);
    assert_eq!(d.advance(&mut i, 576), 0);
    assert_eq!(d.read(0x94, 4) & 0xFFFFF, 0x3FF);
    assert_eq!(d.read(0x98, 4), 0x339D_0802);
    // STOP: 直ちに止まり、そこまでの転送を区切る
    d.ready.clear();
    d.write(0xA0, 4, 4, i.frame_ticks());
    assert_eq!(d.read(0xA0, 4), 0);
    assert_eq!(d.read(0x94, 4), 0);
    assert_eq!(d.ready.len(), 1);
    assert_eq!((d.ready[0].src, d.ready[0].units), (0x339D_0800, 1));
    assert_eq!(d.next_event(&i), NO_EVENT);
    // FIFO は送り出しで空になり、以後は何も起きない
    d.advance(&mut i, 576 * 1000);
    assert_eq!(i.read(0x00, 4) & 0x80, 0, "TXFR (empty)");
}

/// ドライバが再生を止める手順（2026-09-30 の --watch）の後、次の再生で DMA を ON に
/// すると CURR_TC が読み込まれる（送信 FIFO を無効にしたときに中身を捨てるので、
/// S/W Work-Around の待ちが終わる）。
#[test]
fn iis_dma_stop_then_replay() {
    let (mut d, mut i) = wavedev_setup();
    let ft = i.frame_ticks();
    d.write(0xA0, 4, 2, ft);
    d.service_iis_tx(&mut i);
    i.write(0x00, 4, 0xA3);
    d.advance(&mut i, 576 * 100);
    // 止める: STOP → TXIDLE → TXEN=0・TXDMA=0 → 送信なし
    d.write(0xA0, 4, 6, ft);
    d.write(0xA0, 4, 0, ft);
    i.write(0x00, 4, 0x18B);
    i.write(0x0C, 4, 0x0800);
    i.write(0x04, 4, 0x2D);
    assert_eq!(i.read(0x0C, 4) >> 6 & 0x3F, 0, "TX FIFO flushed");
    // 次の再生（ブートの初回と同じ手順）
    i.write(0x04, 4, 0xAD);
    i.write(0x0C, 4, 0xA000);
    i.write(0x00, 4, 0xA2);
    d.write(0xA0, 4, 2, ft);
    assert_eq!(d.service_iis_tx(&mut i), 0);
    assert_eq!(d.read(0x94, 4) & 0xFFFFF, 0x400 - 32, "CURR_TC loaded");
}

/// RELOAD=1 は CURR_TC=0 で ON_OFF を落とす。INT=0 なら割り込みはないが、区切りの
/// 期限はある（RAM を読む時機を固定するため）。
#[test]
fn dma_no_reload_no_int() {
    let (mut d, mut i) = wavedev_setup();
    let ft = i.frame_ticks();
    d.write(0x90, 4, 0x00D0_0040, ft); // INT=0・H/W 要求・RELOAD=1・ハーフワード・TC=64
    d.write(0xA0, 4, 2, ft);
    d.service_iis_tx(&mut i);
    i.write(0x00, 4, 0xA3);
    assert_eq!(d.next_event(&i), 576 * 32);
    assert_eq!(d.advance(&mut i, 576 * 32), 0);
    assert_eq!(d.next_event(&i), NO_EVENT);
    assert_eq!(d.read(0xA0, 4), 0, "ON_OFF cleared");
    assert_eq!(d.ready.len(), 1);
}

/// advance は加算的（期限の手前でどう区切っても、まとめて進めた場合と同じ状態）。
#[test]
fn dma_advance_additive() {
    let (mut d, mut i) = wavedev_setup();
    let ft = i.frame_ticks();
    d.write(0xA0, 4, 2, ft);
    d.service_iis_tx(&mut i);
    i.write(0x00, 4, 0xA3);
    let mut seed = 12345u32;
    for _ in 0..20 {
        let due = d.next_event(&i);
        let (mut a, mut ai) = (d.clone(), i.clone());
        let whole = a.advance(&mut ai, due);
        let (mut b, mut bi) = (d.clone(), i.clone());
        let mut left = due;
        let mut got = 0;
        while left > 0 {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let n = ((seed >> 8) as i64 % 5000 + 1).min(left);
            got |= b.advance(&mut bi, n);
            left -= n;
            if left > 0 {
                assert_eq!(got, 0, "interrupt before the deadline");
                assert_eq!(b.next_event(&bi), left, "deadline is exact");
            }
        }
        assert_eq!((got, &b, &bi), (whole, &a, &ai));
        assert_eq!(whole, 1 << 2);
        (d, i) = (a, ai);
        d.ready.clear();
    }
}
