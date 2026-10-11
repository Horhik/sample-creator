//! Minimal RIFF/WAVE (and RF64) header reader.
//!
//! Reads only chunk headers and the small metadata chunks we care about (`fmt `,
//! `bext`, `cue `, `LIST/adtl`); the `data` chunk is skipped with a seek, so this
//! is cheap even for multi-gigabyte files on a slow card reader. Chunk order is not
//! assumed: some recorders write `bext`, `cue `, `LIST`, `JUNK` before `fmt `.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};

use anyhow::{Context, Result, bail, ensure};

/// Metadata chunks bigger than this are ignored instead of being read into memory.
const MAX_META_CHUNK: u32 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// Unsigned 8-bit or signed little-endian integer with this many bits per sample.
    Int(u16),
    /// IEEE float, 32 or 64 bits.
    Float(u16),
}

impl SampleFormat {
    pub fn label(self) -> String {
        match self {
            SampleFormat::Int(bits) => format!("{bits}-bit"),
            SampleFormat::Float(bits) => format!("{bits}-bit float"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Bext {
    pub description: String,
    pub originator: String,
    /// `yyyy-mm-dd` as written by the recorder.
    pub origination_date: String,
    /// `hh:mm:ss` as written by the recorder.
    pub origination_time: String,
    /// Samples since midnight at the first sample of the file.
    pub time_reference: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cue {
    pub id: u32,
    pub frame: u64,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavInfo {
    pub format: SampleFormat,
    pub channels: u16,
    pub sample_rate: u32,
    /// Bytes per frame (all channels).
    pub block_align: u16,
    pub data_offset: u64,
    pub data_len: u64,
    pub bext: Option<Bext>,
    /// Sorted by position.
    pub cues: Vec<Cue>,
}

impl WavInfo {
    pub fn total_frames(&self) -> u64 {
        self.data_len / u64::from(self.block_align)
    }
}

struct Fmt {
    format: SampleFormat,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
}

pub fn read_info<R: Read + Seek>(r: &mut R) -> Result<WavInfo> {
    let file_len = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;

    let mut header = [0u8; 12];
    r.read_exact(&mut header)
        .context("file too short for a RIFF header")?;
    let rf64 = match &header[0..4] {
        b"RIFF" => false,
        b"RF64" | b"BW64" => true,
        _ => bail!("not a RIFF file"),
    };
    ensure!(&header[8..12] == b"WAVE", "RIFF file is not WAVE");

    let mut fmt = None;
    let mut data = None;
    let mut ds64_data_len = None;
    let mut bext = None;
    let mut cue_points: Vec<(u32, u64)> = Vec::new();
    let mut labels: HashMap<u32, String> = HashMap::new();

    let mut pos = 12u64;
    while pos + 8 <= file_len {
        r.seek(SeekFrom::Start(pos))?;
        let mut chunk_header = [0u8; 8];
        r.read_exact(&mut chunk_header)?;
        let id: [u8; 4] = chunk_header[0..4].try_into().unwrap();
        let size = u32::from_le_bytes(chunk_header[4..8].try_into().unwrap());
        let body_start = pos + 8;

        if &id == b"data" {
            let declared = match ds64_data_len {
                Some(len) if rf64 && size == u32::MAX => len,
                _ => u64::from(size),
            };
            // A recorder that lost power leaves a too-small or too-big size; trust the file.
            let available = file_len - body_start;
            let len = if declared == 0 || declared > available {
                available
            } else {
                declared
            };
            data = Some((body_start, len));
            pos = body_start + len + (len & 1);
            continue;
        }

        let read_body = |r: &mut R| -> Result<Option<Vec<u8>>> {
            if size > MAX_META_CHUNK || body_start + u64::from(size) > file_len {
                return Ok(None);
            }
            let mut body = vec![0u8; size as usize];
            r.read_exact(&mut body)?;
            Ok(Some(body))
        };

        match &id {
            b"fmt " => {
                let body = read_body(r)?.context("unreadable fmt chunk")?;
                fmt = Some(parse_fmt(&body)?);
            }
            b"ds64" => {
                if let Some(body) = read_body(r)?
                    && body.len() >= 16
                {
                    ds64_data_len = Some(u64::from_le_bytes(body[8..16].try_into().unwrap()));
                }
            }
            b"bext" => bext = read_body(r)?.and_then(|b| parse_bext(&b)),
            b"cue " => {
                if let Some(body) = read_body(r)? {
                    cue_points = parse_cue(&body);
                }
            }
            b"LIST" => {
                if let Some(body) = read_body(r)?
                    && body.starts_with(b"adtl")
                {
                    labels.extend(parse_adtl(&body[4..]));
                }
            }
            _ => {}
        }

        pos = body_start + u64::from(size) + u64::from(size & 1);
    }

    let fmt = fmt.context("no fmt chunk")?;
    let (data_offset, data_len) = data.context("no data chunk")?;

    let mut cues: Vec<Cue> = cue_points
        .into_iter()
        .map(|(id, frame)| Cue {
            id,
            frame,
            label: labels.remove(&id),
        })
        .collect();
    cues.sort_by_key(|c| (c.frame, c.id));

    Ok(WavInfo {
        format: fmt.format,
        channels: fmt.channels,
        sample_rate: fmt.sample_rate,
        block_align: fmt.block_align,
        data_offset,
        data_len,
        bext,
        cues,
    })
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(b[at..at + 2].try_into().unwrap())
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn parse_fmt(b: &[u8]) -> Result<Fmt> {
    ensure!(b.len() >= 16, "fmt chunk too short");
    let mut tag = u16_at(b, 0);
    let channels = u16_at(b, 2);
    let sample_rate = u32_at(b, 4);
    let block_align = u16_at(b, 12);
    let bits = u16_at(b, 14);

    // WAVE_FORMAT_EXTENSIBLE: the real format is the first two bytes of the sub-format GUID.
    if tag == 0xFFFE {
        ensure!(b.len() >= 26, "extensible fmt chunk too short");
        tag = u16_at(b, 24);
    }
    ensure!(
        channels > 0 && sample_rate > 0,
        "fmt chunk has zero channels or rate"
    );
    ensure!(
        block_align > 0 && block_align.is_multiple_of(channels),
        "fmt chunk has bad block align"
    );

    // Container width decides how samples are laid out; `bits` may be smaller (e.g. 20-in-24).
    let container = block_align / channels * 8;
    let format = match (tag, container) {
        (1, 8 | 16 | 24 | 32) => SampleFormat::Int(container),
        (3, 32 | 64) => SampleFormat::Float(container),
        _ => bail!("unsupported WAV encoding (tag {tag:#x}, {bits} bits)"),
    };
    Ok(Fmt {
        format,
        channels,
        sample_rate,
        block_align,
    })
}

fn ascii_field(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim_end().to_string()
}

fn parse_bext(b: &[u8]) -> Option<Bext> {
    if b.len() < 346 {
        return None;
    }
    let lo = u64::from(u32_at(b, 338));
    let hi = u64::from(u32_at(b, 342));
    Some(Bext {
        description: ascii_field(&b[0..256]),
        originator: ascii_field(&b[256..288]),
        origination_date: ascii_field(&b[320..330]),
        origination_time: ascii_field(&b[330..338]),
        time_reference: lo | (hi << 32),
    })
}

fn parse_cue(b: &[u8]) -> Vec<(u32, u64)> {
    if b.len() < 4 {
        return Vec::new();
    }
    let count = u32_at(b, 0) as usize;
    b[4..]
        .chunks_exact(24)
        .take(count)
        .map(|p| (u32_at(p, 0), u64::from(u32_at(p, 20))))
        .collect()
}

fn parse_adtl(mut b: &[u8]) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    while b.len() >= 8 {
        let size = u32_at(b, 4) as usize;
        let Some(body) = b.get(8..8 + size) else {
            break;
        };
        if &b[0..4] == b"labl" && body.len() >= 4 {
            out.push((u32_at(body, 0), ascii_field(&body[4..])));
        }
        b = b.get(8 + size + (size & 1)..).unwrap_or_default();
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend((body.len() as u32).to_le_bytes());
        out.extend(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn fmt_body(tag: u16, channels: u16, rate: u32, bits: u16) -> Vec<u8> {
        let align = channels * bits / 8;
        let mut b = Vec::new();
        b.extend(tag.to_le_bytes());
        b.extend(channels.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend((rate * u32::from(align)).to_le_bytes());
        b.extend(align.to_le_bytes());
        b.extend(bits.to_le_bytes());
        b
    }

    fn riff(chunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = chunks.concat();
        let mut out = b"RIFF".to_vec();
        out.extend((body.len() as u32 + 4).to_le_bytes());
        out.extend(b"WAVE");
        out.extend(body);
        out
    }

    /// A small stereo 32-bit float WAV laid out like a hand-held recorder writes it.
    pub(crate) fn recorder_like(frames: &[[f32; 2]]) -> Vec<u8> {
        let mut bext = vec![0u8; 602];
        bext[256..256 + 12].copy_from_slice(b"PCM Recorder");
        bext[320..330].copy_from_slice(b"2026-10-08");
        bext[330..338].copy_from_slice(b"22:47:55");
        bext[338..342].copy_from_slice(&3938686848u32.to_le_bytes());

        let mut cue = 2u32.to_le_bytes().to_vec();
        for (id, at) in [(1u32, 3u32), (2, 1)] {
            cue.extend(id.to_le_bytes());
            cue.extend(at.to_le_bytes());
            cue.extend(b"data");
            cue.extend([0u8; 8]);
            cue.extend(at.to_le_bytes());
        }

        let mut adtl = b"adtl".to_vec();
        adtl.extend(chunk(
            b"labl",
            &[&1u32.to_le_bytes()[..], b"MARK_02\0"].concat(),
        ));
        adtl.extend(chunk(
            b"labl",
            &[&2u32.to_le_bytes()[..], b"MARK_01\0"].concat(),
        ));

        let data: Vec<u8> = frames
            .iter()
            .flatten()
            .flat_map(|s| s.to_le_bytes())
            .collect();

        riff(&[
            chunk(b"bext", &bext),
            chunk(b"cue ", &cue),
            chunk(b"LIST", &adtl),
            chunk(b"JUNK", &[0u8; 30]),
            chunk(b"fmt ", &fmt_body(3, 2, 48000, 32)),
            chunk(b"data", &data),
        ])
    }

    #[test]
    fn reads_recorder_layout() {
        let bytes = recorder_like(&[[0.0, 0.0]; 5]);
        let info = read_info(&mut Cursor::new(bytes)).unwrap();

        assert_eq!(info.format, SampleFormat::Float(32));
        assert_eq!(
            (info.channels, info.sample_rate, info.block_align),
            (2, 48000, 8)
        );
        assert_eq!(info.total_frames(), 5);

        let bext = info.bext.unwrap();
        assert_eq!(bext.originator, "PCM Recorder");
        assert_eq!(bext.origination_date, "2026-10-08");
        assert_eq!(bext.origination_time, "22:47:55");
        assert_eq!(bext.time_reference, 3938686848);

        let cues: Vec<_> = info
            .cues
            .iter()
            .map(|c| (c.frame, c.label.as_deref().unwrap()))
            .collect();
        assert_eq!(cues, [(1, "MARK_01"), (3, "MARK_02")]);
    }

    #[test]
    fn truncated_data_chunk_uses_what_is_on_disk() {
        let mut bytes = riff(&[
            chunk(b"fmt ", &fmt_body(1, 1, 44100, 16)),
            chunk(b"data", &[0u8; 20]),
        ]);
        bytes.truncate(bytes.len() - 6);
        let info = read_info(&mut Cursor::new(bytes)).unwrap();
        assert_eq!(info.total_frames(), 7);
        assert_eq!(info.format, SampleFormat::Int(16));
    }

    #[test]
    fn rejects_non_wave() {
        assert!(read_info(&mut Cursor::new(b"OggS0000000000000".to_vec())).is_err());
    }
}
