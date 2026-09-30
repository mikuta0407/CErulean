//! 音の書き出し（--audio-out）。16 ビット・ステレオの WAV（RIFF）。

use std::io::{Seek, SeekFrom, Write};

use cerulean_core::smdk2410::{AudioChunk, INSTRUCTIONS_PER_SECOND, PCLK_HZ};

/// 続きの音とみなす遅れ（ms）。CLI が取り出す間隔（仮想時間 0.1 秒）と DMA の区切りより長く。
const SLACK_MS: u64 = 200;

/// コアが送り出したサンプルを WAV に書く。サンプリング周波数は最初の音のもの
/// （途中で変わったら知らせて、そのまま書く）。音の間の無音は仮想時間に合わせて
/// 埋める: 音が届いたら、その終わりが取り出した時点の仮想時間に来るように前に 0 を
/// 足す。ただし足す量が SLACK より小さければ続きの音とみなして足さない（コアは DMA の
/// 区切り（約 12ms）ごとにまとめて出すので、再生中でも届く量は仮想時間の経過と少し
/// ずれる。埋めると音の途中に無音を挟んでしまう）。
pub struct WavOut {
    path: String,
    out: std::io::BufWriter<std::fs::File>,
    rate: Option<u32>,
    /// 書いたフレーム数（左右 1 組が 1 フレーム）
    frames: u64,
    /// 書き始めた命令数
    start_steps: u64,
    warned: bool,
}

impl WavOut {
    pub fn create(path: &str, start_steps: u64) -> Result<WavOut, String> {
        let f = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
        let mut w = WavOut {
            path: path.to_string(),
            out: std::io::BufWriter::new(f),
            rate: None,
            frames: 0,
            start_steps,
            warned: false,
        };
        w.header(0, 0).map_err(|e| format!("{path}: {e}"))?;
        Ok(w)
    }

    fn header(&mut self, rate: u32, frames: u64) -> std::io::Result<()> {
        let data = (frames * 4).min(u32::MAX as u64 - 36) as u32;
        let w = &mut self.out;
        w.write_all(b"RIFF")?;
        w.write_all(&(36 + data).to_le_bytes())?;
        w.write_all(b"WAVEfmt ")?;
        w.write_all(&16u32.to_le_bytes())?;
        w.write_all(&1u16.to_le_bytes())?; // PCM
        w.write_all(&2u16.to_le_bytes())?; // ステレオ
        w.write_all(&rate.to_le_bytes())?;
        w.write_all(&(rate * 4).to_le_bytes())?;
        w.write_all(&4u16.to_le_bytes())?;
        w.write_all(&16u16.to_le_bytes())?;
        w.write_all(b"data")?;
        w.write_all(&data.to_le_bytes())
    }

    /// 取り出した音を書く（steps は取り出した時点の命令数）。
    pub fn feed(&mut self, chunks: Vec<AudioChunk>, steps: u64) -> Result<(), String> {
        let path = self.path.clone();
        let io = |e: std::io::Error| format!("{path}: {e}");
        let n: u64 = chunks.iter().map(|c| c.samples.len() as u64 / 2).sum();
        if let Some(c) = chunks.first()
            && self.rate.is_none()
        {
            let r = (PCLK_HZ as u64 / c.frame_ticks.max(1) as u64) as u32;
            eprintln!("cerulean: audio at {r} Hz");
            self.rate = Some(r);
        }
        let Some(rate) = self.rate else {
            return Ok(());
        };
        if n == 0 {
            return Ok(());
        }
        // 今回の分の終わりが、仮想時間で今までに流れたはずのフレーム数に来るように。
        let due =
            (steps - self.start_steps) as u128 * rate as u128 / INSTRUCTIONS_PER_SECOND as u128;
        let slack = rate as u64 * SLACK_MS / 1000;
        let gap = match (due as u64).saturating_sub(self.frames + n) {
            g if g < slack => 0,
            g => g,
        };
        let zeros = [0u8; 4 * 256];
        let mut left = gap;
        while left > 0 {
            let k = left.min(256);
            self.out.write_all(&zeros[..k as usize * 4]).map_err(io)?;
            left -= k;
        }
        self.frames += gap;
        for c in chunks {
            let r = (PCLK_HZ as u64 / c.frame_ticks.max(1) as u64) as u32;
            if r != rate && !self.warned {
                eprintln!("cerulean: audio rate changed to {r} Hz (written as {rate} Hz)");
                self.warned = true;
            }
            let mut buf = Vec::with_capacity(c.samples.len() / 2 * 4);
            for [l, r] in c.samples.as_chunks::<2>().0 {
                buf.extend_from_slice(&l.to_le_bytes());
                buf.extend_from_slice(&r.to_le_bytes());
            }
            self.out.write_all(&buf).map_err(io)?;
            self.frames += c.samples.len() as u64 / 2;
        }
        Ok(())
    }

    /// ヘッダの長さを書き直して閉じる。
    pub fn finish(mut self) -> Result<(), String> {
        let path = self.path.clone();
        let io = |e: std::io::Error| format!("{path}: {e}");
        let rate = self.rate.unwrap_or(44_100);
        self.out.seek(SeekFrom::Start(0)).map_err(io)?;
        let frames = self.frames;
        self.header(rate, frames).map_err(io)?;
        self.out.flush().map_err(io)?;
        eprintln!(
            "cerulean: wrote {} ({:.2}s of audio)",
            self.path,
            frames as f64 / rate as f64
        );
        Ok(())
    }
}
