//! 一致確認用の出力（testdata/golden/README.md の結果の JSON と trace-hash）。
//! Go 版の cmd/cerulean/result.go と同じ値を出す（書式は README が正）。

use std::io::Write;

use cerulean_core::arm::StopError;
use cerulean_core::smdk2410::{Machine, SDRAM_BASE};
use sha2::{Digest, Sha256};

/// 結果の JSON の版数（README の定義を変えたら上げる）。
const RESULT_FORMAT: u32 = 1;

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

/// SDRAM 全体（PA 0x30000000 から 128MB）の SHA-256。
pub fn ram_sha256(m: &Machine) -> String {
    let (ram, _) = m.sys.bus.ram(SDRAM_BASE).expect("SDRAM is mapped");
    sha256_hex(ram)
}

/// UART1 の送信バイト列を数えながら SHA-256 を取る。
pub struct UartTap {
    sum: Sha256,
    pub n: u64,
}

impl UartTap {
    pub fn new() -> UartTap {
        UartTap {
            sum: Sha256::new(),
            n: 0,
        }
    }

    pub fn feed(&mut self, b: &[u8]) {
        self.sum.update(b);
        self.n += b.len() as u64;
    }

    /// 今までの送信バイト列の SHA-256（状態は変えない）。
    pub fn digest(&self) -> String {
        hex(&self.sum.clone().finalize())
    }
}

/// 停止の種類（README の stop.kind）。
pub enum Stop {
    MaxSteps,
    Quit,
    EventError,
    Emu(StopError),
}

impl Stop {
    fn json(&self) -> String {
        match self {
            Stop::MaxSteps => r#"{"kind":"max-steps"}"#.into(),
            Stop::Quit => r#"{"kind":"quit"}"#.into(),
            Stop::EventError => r#"{"kind":"event-error"}"#.into(),
            Stop::Emu(StopError::Undefined(u)) => {
                format!(r#"{{"kind":"undefined","pc":{},"word":{}}}"#, u.pc, u.word)
            }
            Stop::Emu(StopError::Bus(b)) => format!(
                r#"{{"kind":"bus-error","addr":{},"write":{}}}"#,
                b.addr, b.write
            ),
        }
    }
}

/// --result の出力先（JSON Lines）。
pub struct ResultWriter {
    pub out: std::fs::File,
}

impl ResultWriter {
    /// 1 行書く。event は "checkpoint" か "stop"。
    pub fn write(
        &mut self,
        m: &mut Machine,
        uart: &UartTap,
        event: &str,
        stop: Option<&Stop>,
    ) -> std::io::Result<()> {
        let dump = m.cpu_dump();
        let (screen, w, h) = match m.frame() {
            Ok((f, _)) => (sha256_hex(&f.rgba), f.width, f.height),
            Err(_) => (String::new(), 0, 0),
        };
        let stop = stop
            .map(|s| format!(r#","stop":{}"#, s.json()))
            .unwrap_or_default();
        writeln!(
            self.out,
            r#"{{"format":{RESULT_FORMAT},"event":"{event}","steps":{}{stop},"cpu":"{}","cpu_sha256":"{}","ram_sha256":"{}","uart1_sha256":"{}","uart1_bytes":{},"screen_sha256":"{screen}","screen_w":{w},"screen_h":{h}}}"#,
            m.steps(),
            hex(&dump),
            sha256_hex(&dump),
            ram_sha256(m),
            uart.digest(),
            uart.n,
        )
    }
}

/// --trace-hash の出力。every 命令ごとに「命令数 CPU ダンプの SHA-256」、
/// ram_every 命令ごとに RAM の SHA-256 も加えた 1 行を書く。
pub struct TraceHasher {
    pub out: Box<dyn Write>,
    pub every: u64,
    pub ram_every: u64,
}

impl TraceHasher {
    /// steps が出力する命令数か（cpu, ram）。
    fn due(&self, steps: u64) -> (bool, bool) {
        if steps == 0 {
            return (false, false);
        }
        let ram = self.ram_every != 0 && steps.is_multiple_of(self.ram_every);
        (
            ram || (self.every != 0 && steps.is_multiple_of(self.every)),
            ram,
        )
    }

    /// steps より後で次に出力する命令数（実行ループの止まる点）。
    pub fn next(&self, steps: u64) -> u64 {
        [self.every, self.ram_every]
            .into_iter()
            .filter(|&k| k != 0)
            .map(|k| next_multiple(steps, k))
            .min()
            .unwrap_or(u64::MAX)
    }

    pub fn emit(&mut self, m: &Machine) -> std::io::Result<()> {
        let (cpu, ram) = self.due(m.steps());
        if !cpu {
            return Ok(());
        }
        let mut line = format!("{} {}", m.steps(), sha256_hex(&m.cpu_dump()));
        if ram {
            line += &format!(" ram={}", ram_sha256(m));
        }
        writeln!(self.out, "{line}")
    }
}

/// n より大きい最小の k の倍数。
pub fn next_multiple(n: u64, k: u64) -> u64 {
    (n / k + 1) * k
}
