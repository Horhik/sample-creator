//! Streaming sample readers. Nothing here ever holds a whole file in memory.
//!
//! WAV/BWF goes through our own reader (exact frame seeks are a single file seek);
//! compressed formats go through symphonia.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use anyhow::{Context, Result, anyhow};
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::Timestamp;

use super::bwf::{self, Bext, Cue, SampleFormat};

#[derive(Debug, Clone)]
pub struct AudioInfo {
    pub channels: u16,
    pub sample_rate: u32,
    pub total_frames: u64,
    /// Human readable encoding, e.g. "32-bit float" or "MP3".
    pub encoding: String,
    pub bext: Option<Bext>,
    pub cues: Vec<Cue>,
}

impl AudioInfo {
    pub fn duration_secs(&self) -> f64 {
        self.total_frames as f64 / f64::from(self.sample_rate)
    }
}

/// Sequential reader of interleaved `f32` frames with random access by frame.
pub trait FrameSource: Send {
    fn seek(&mut self, frame: u64) -> Result<()>;

    /// Appends up to `max_frames` interleaved frames to `out`. Returns frames read; 0 at the end.
    fn read(&mut self, out: &mut Vec<f32>, max_frames: usize) -> Result<usize>;
}

pub fn probe(path: &Path) -> Result<AudioInfo> {
    Ok(open(path)?.0)
}

pub fn open(path: &Path) -> Result<(AudioInfo, Box<dyn FrameSource>)> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;

    match bwf::read_info(&mut file) {
        Ok(wav) => {
            let info = AudioInfo {
                channels: wav.channels,
                sample_rate: wav.sample_rate,
                total_frames: wav.total_frames(),
                encoding: wav.format.label(),
                bext: wav.bext.clone(),
                cues: wav.cues.clone(),
            };
            Ok((info, Box::new(WavSource::new(file, wav)?)))
        }
        Err(wav_err) => {
            let is_wav = path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("wav") || e.eq_ignore_ascii_case("bwf"));
            if is_wav {
                return Err(wav_err.context(format!("cannot read {}", path.display())));
            }
            let source = SymphoniaSource::new(path)?;
            Ok((source.info.clone(), Box::new(source)))
        }
    }
}

struct WavSource {
    reader: BufReader<File>,
    format: SampleFormat,
    channels: usize,
    block_align: usize,
    data_offset: u64,
    total_frames: u64,
    pos: u64,
    bytes: Vec<u8>,
}

impl WavSource {
    fn new(file: File, info: bwf::WavInfo) -> Result<Self> {
        let mut source = WavSource {
            reader: BufReader::with_capacity(1 << 20, file),
            format: info.format,
            channels: usize::from(info.channels),
            block_align: usize::from(info.block_align),
            data_offset: info.data_offset,
            total_frames: info.total_frames(),
            pos: 0,
            bytes: Vec::new(),
        };
        source.seek(0)?;
        Ok(source)
    }
}

impl FrameSource for WavSource {
    fn seek(&mut self, frame: u64) -> Result<()> {
        self.pos = frame.min(self.total_frames);
        self.reader.seek(SeekFrom::Start(
            self.data_offset + self.pos * self.block_align as u64,
        ))?;
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<f32>, max_frames: usize) -> Result<usize> {
        let frames = (max_frames as u64).min(self.total_frames - self.pos) as usize;
        if frames == 0 {
            return Ok(0);
        }
        self.bytes.resize(frames * self.block_align, 0);
        self.reader.read_exact(&mut self.bytes)?;
        self.pos += frames as u64;

        out.reserve(frames * self.channels);
        decode_pcm(self.format, &self.bytes, out);
        Ok(frames)
    }
}

fn decode_pcm(format: SampleFormat, bytes: &[u8], out: &mut Vec<f32>) {
    match format {
        SampleFormat::Int(8) => out.extend(bytes.iter().map(|&b| (f32::from(b) - 128.0) / 128.0)),
        SampleFormat::Int(16) => out.extend(
            bytes
                .chunks_exact(2)
                .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0),
        ),
        SampleFormat::Int(24) => out.extend(
            bytes
                .chunks_exact(3)
                .map(|s| (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0),
        ),
        SampleFormat::Int(32) => out.extend(
            bytes
                .chunks_exact(4)
                .map(|s| i32::from_le_bytes(s.try_into().unwrap()) as f32 / 2_147_483_648.0),
        ),
        SampleFormat::Float(32) => out.extend(
            bytes
                .chunks_exact(4)
                .map(|s| f32::from_le_bytes(s.try_into().unwrap())),
        ),
        SampleFormat::Float(64) => out.extend(
            bytes
                .chunks_exact(8)
                .map(|s| f64::from_le_bytes(s.try_into().unwrap()) as f32),
        ),
        other => unreachable!("bwf::parse_fmt rejects {other:?}"),
    }
}

struct SymphoniaSource {
    info: AudioInfo,
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track_id: u32,
    /// Decoded frames not handed out yet.
    pending: Vec<f32>,
    /// Frames to drop after an inexact seek.
    skip: u64,
}

impl SymphoniaSource {
    fn new(path: &Path) -> Result<Self> {
        let open_reader = || -> Result<Box<dyn FormatReader>> {
            let file = File::open(path)?;
            let mss = MediaSourceStream::new(Box::new(file), Default::default());
            let mut hint = Hint::new();
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                hint.with_extension(ext);
            }
            Ok(symphonia::default::get_probe().probe(
                &hint,
                mss,
                FormatOptions::default(),
                MetadataOptions::default(),
            )?)
        };

        let mut format = open_reader()?;
        let track = format
            .default_track(TrackType::Audio)
            .context("no audio track")?;
        let track_id = track.id;
        let params = track
            .codec_params
            .as_ref()
            .and_then(|p| p.audio())
            .context("track has no audio parameters")?
            .clone();
        let known_frames = track.num_frames;

        let sample_rate = params.sample_rate.context("unknown sample rate")?;
        let channels = params
            .channels
            .as_ref()
            .map(|c| c.count() as u16)
            .context("unknown channel layout")?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(&params, &AudioDecoderOptions::default())?;

        // MP3 without a Xing/LAME header has no frame count: count packets once.
        let total_frames = match known_frames {
            Some(n) => n,
            None => {
                let mut counter = open_reader()?;
                let mut n = 0u64;
                while let Some(packet) = counter.next_packet()? {
                    if packet.track_id == track_id {
                        n += packet.dur.get();
                    }
                }
                n
            }
        };

        let encoding = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_uppercase)
            .unwrap_or_else(|| "?".into());
        // Dropping the probe-time reader position: start from the beginning explicitly.
        format.seek(
            SeekMode::Accurate,
            SeekTo::Timestamp {
                ts: Timestamp::new(0),
                track_id,
            },
        )?;

        Ok(SymphoniaSource {
            info: AudioInfo {
                channels,
                sample_rate,
                total_frames,
                encoding,
                bext: None,
                cues: Vec::new(),
            },
            format,
            decoder,
            track_id,
            pending: Vec::new(),
            skip: 0,
        })
    }
}

impl FrameSource for SymphoniaSource {
    fn seek(&mut self, frame: u64) -> Result<()> {
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Timestamp {
                    ts: Timestamp::new(frame as i64),
                    track_id: self.track_id,
                },
            )
            .map_err(|e| anyhow!("seek failed: {e}"))?;
        self.decoder.reset();
        self.pending.clear();
        self.skip = (seeked.required_ts.get() - seeked.actual_ts.get()).max(0) as u64;
        Ok(())
    }

    fn read(&mut self, out: &mut Vec<f32>, max_frames: usize) -> Result<usize> {
        let channels = usize::from(self.info.channels);
        while self.pending.len() < max_frames * channels {
            let packet = match self.format.next_packet() {
                Ok(Some(p)) => p,
                Ok(None) => break,
                Err(SymError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            if packet.track_id != self.track_id {
                continue;
            }
            let buf = match self.decoder.decode(&packet) {
                Ok(buf) => buf,
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            };
            let start = self.pending.len();
            self.pending.resize(start + buf.samples_interleaved(), 0.0);
            buf.copy_to_slice_interleaved(&mut self.pending[start..]);

            if self.skip > 0 {
                let drop = (self.skip as usize * channels).min(self.pending.len());
                self.pending.drain(..drop);
                self.skip -= (drop / channels) as u64;
            }
        }

        let take = (max_frames * channels).min(self.pending.len());
        out.extend(self.pending.drain(..take));
        Ok(take / channels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::bwf::tests::tascam_like;

    #[test]
    fn wav_reads_and_seeks_exact_frames() {
        let frames: Vec<[f32; 2]> = (0..10).map(|i| [i as f32, -(i as f32)]).collect();
        let dir = std::env::temp_dir().join(format!("sc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("take.wav");
        std::fs::write(&path, tascam_like(&frames)).unwrap();

        let (info, mut src) = open(&path).unwrap();
        assert_eq!((info.channels, info.total_frames), (2, 10));

        let mut out = Vec::new();
        assert_eq!(src.read(&mut out, 3).unwrap(), 3);
        assert_eq!(out, [0.0, -0.0, 1.0, -1.0, 2.0, -2.0]);

        src.seek(8).unwrap();
        out.clear();
        assert_eq!(src.read(&mut out, 100).unwrap(), 2);
        assert_eq!(out, [8.0, -8.0, 9.0, -9.0]);
        assert_eq!(src.read(&mut out, 100).unwrap(), 0);

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn decodes_24_bit_sign() {
        let mut out = Vec::new();
        decode_pcm(
            SampleFormat::Int(24),
            &[0xFF, 0xFF, 0x7F, 0x00, 0x00, 0x80],
            &mut out,
        );
        assert!((out[0] - 1.0).abs() < 1e-6);
        assert_eq!(out[1], -1.0);
    }
}
