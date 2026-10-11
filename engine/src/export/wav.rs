//! Streaming WAV/BWF writer.
//!
//! Writes into `<name>.part` and renames on [`WavWriter::finish`], so an
//! interrupted export never leaves a half-written file under the final name.

use std::fs::{self, File};
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

use crate::audio::bwf::{Bext, SampleFormat};

const BEXT_LEN: usize = 602;

pub struct WavWriter {
    out: BufWriter<File>,
    part: PathBuf,
    path: PathBuf,
    format: SampleFormat,
    channels: u16,
    frames: u64,
    /// Offsets of the size fields to patch on finish.
    riff_size_at: u64,
    fact_at: Option<u64>,
    data_size_at: u64,
    data_start: u64,
}

impl WavWriter {
    pub fn create(
        path: &Path,
        format: SampleFormat,
        channels: u16,
        sample_rate: u32,
        bext: Option<&Bext>,
    ) -> Result<Self> {
        let (tag, bits) = match format {
            SampleFormat::Int(bits) => (1u16, bits),
            SampleFormat::Float(bits) => (3u16, bits),
        };
        let block_align = channels * bits / 8;

        let part = path.with_extension("wav.part");
        let mut out = BufWriter::with_capacity(1 << 20, File::create(&part)?);
        out.write_all(b"RIFF")?;
        out.write_all(&0u32.to_le_bytes())?;
        out.write_all(b"WAVE")?;

        if let Some(bext) = bext {
            out.write_all(b"bext")?;
            out.write_all(&(BEXT_LEN as u32).to_le_bytes())?;
            out.write_all(&bext_body(bext))?;
        }

        out.write_all(b"fmt ")?;
        out.write_all(&16u32.to_le_bytes())?;
        out.write_all(&tag.to_le_bytes())?;
        out.write_all(&channels.to_le_bytes())?;
        out.write_all(&sample_rate.to_le_bytes())?;
        out.write_all(&(sample_rate * u32::from(block_align)).to_le_bytes())?;
        out.write_all(&block_align.to_le_bytes())?;
        out.write_all(&bits.to_le_bytes())?;

        // Non-PCM formats should carry a fact chunk with the frame count.
        let mut fact_at = None;
        if tag == 3 {
            out.write_all(b"fact")?;
            out.write_all(&4u32.to_le_bytes())?;
            fact_at = Some(out.stream_position()?);
            out.write_all(&0u32.to_le_bytes())?;
        }

        out.write_all(b"data")?;
        let data_size_at = out.stream_position()?;
        out.write_all(&0u32.to_le_bytes())?;
        let data_start = out.stream_position()?;

        Ok(WavWriter {
            out,
            part,
            path: path.to_path_buf(),
            format,
            channels,
            frames: 0,
            riff_size_at: 4,
            fact_at,
            data_size_at,
            data_start,
        })
    }

    /// Appends interleaved frames with `channels` samples each.
    pub fn write(&mut self, samples: &[f32]) -> Result<()> {
        let n = usize::from(self.channels);
        debug_assert_eq!(samples.len() % n, 0);
        let mut buf = Vec::with_capacity(samples.len() * 4);
        encode(self.format, samples, &mut buf);
        self.out.write_all(&buf)?;
        self.frames += (samples.len() / n) as u64;
        Ok(())
    }

    pub fn finish(mut self) -> Result<PathBuf> {
        let data_len = self.out.stream_position()? - self.data_start;
        ensure!(
            data_len < u64::from(u32::MAX) - 4096,
            "регион больше 4 ГБ: такой WAV не записать"
        );
        if data_len % 2 == 1 {
            self.out.write_all(&[0])?;
        }
        let end = self.out.stream_position()?;

        self.out.seek(SeekFrom::Start(self.data_size_at))?;
        self.out.write_all(&(data_len as u32).to_le_bytes())?;
        if let Some(at) = self.fact_at {
            self.out.seek(SeekFrom::Start(at))?;
            self.out.write_all(&(self.frames as u32).to_le_bytes())?;
        }
        self.out.seek(SeekFrom::Start(self.riff_size_at))?;
        self.out.write_all(&((end - 8) as u32).to_le_bytes())?;
        self.out.flush()?;
        self.out.get_ref().sync_all()?;
        drop(self.out);

        fs::rename(&self.part, &self.path)?;
        Ok(self.path)
    }

    /// Drops the partial file.
    pub fn abort(self) {
        let part = self.part.clone();
        drop(self);
        fs::remove_file(part).ok();
    }
}

fn encode(format: SampleFormat, samples: &[f32], out: &mut Vec<u8>) {
    let clamp = |x: f32| x.clamp(-1.0, 1.0);
    match format {
        SampleFormat::Int(8) => out.extend(
            samples
                .iter()
                .map(|&x| ((clamp(x) * 127.0).round() as i16 + 128) as u8),
        ),
        SampleFormat::Int(16) => {
            for &x in samples {
                out.extend(((clamp(x) * 32767.0).round() as i16).to_le_bytes());
            }
        }
        SampleFormat::Int(24) => {
            for &x in samples {
                let v = (clamp(x) * 8_388_607.0).round() as i32;
                out.extend(&v.to_le_bytes()[..3]);
            }
        }
        SampleFormat::Int(32) => {
            for &x in samples {
                let v = (f64::from(clamp(x)) * 2_147_483_647.0).round() as i32;
                out.extend(v.to_le_bytes());
            }
        }
        // Float keeps overs above 0 dBFS: 32-bit float recordings rely on that.
        SampleFormat::Float(32) => {
            for &x in samples {
                out.extend(x.to_le_bytes());
            }
        }
        SampleFormat::Float(64) => {
            for &x in samples {
                out.extend(f64::from(x).to_le_bytes());
            }
        }
        other => unreachable!("no encoder for {other:?}"),
    }
}

fn put(field: &mut [u8], text: &str) {
    let bytes = text.as_bytes();
    let n = bytes.len().min(field.len());
    field[..n].copy_from_slice(&bytes[..n]);
}

fn bext_body(b: &Bext) -> [u8; BEXT_LEN] {
    let mut body = [0u8; BEXT_LEN];
    put(&mut body[0..256], &b.description);
    put(&mut body[256..288], &b.originator);
    put(&mut body[320..330], &b.origination_date);
    put(&mut body[330..338], &b.origination_time);
    body[338..342].copy_from_slice(&(b.time_reference as u32).to_le_bytes());
    body[342..346].copy_from_slice(&((b.time_reference >> 32) as u32).to_le_bytes());
    body[346..348].copy_from_slice(&1u16.to_le_bytes());
    body
}
