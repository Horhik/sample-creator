//! Waveform summaries (min/max per channel per bin), computed in the background.
//!
//! Two levels: `FINE` frames per bin for zoomed-in views and `COARSE` for the
//! overview. The UI can draw while the worker is still filling them in. A finished
//! summary is cached under `$XDG_CACHE_HOME/sample-creator/peaks/`, never next to
//! the recording.

use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::thread;

use anyhow::{Context, Result, ensure};

use super::decode;

pub const FINE: u64 = 256;
pub const COARSE: u64 = FINE * 64;

/// Closest zoom, in frames per pixel: one fine bin spans at most 4 px, so the
/// waveform stays smooth and fills the view. Deeper would only stretch bins.
pub const MIN_FRAMES_PER_PX: f64 = FINE as f64 / 4.0;

/// Bottom of the waveform's decibel scale: quieter is drawn as silence.
pub const DB_FLOOR: f32 = -60.0;

/// A sample value placed on the decibel scale, sign kept: 0 dB is the full
/// height, [`DB_FLOOR`] the middle line. Quiet takes stay readable, and a
/// sound that is loud only next to an even quieter one still stands out.
pub fn db_scale(x: f32) -> f32 {
    let a = x.abs();
    if a <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * a.log10();
    (1.0 - db / DB_FLOOR).clamp(0.0, 1.0).copysign(x)
}

const CACHE_MAGIC: &[u8; 8] = b"SCPEAK01";

/// One bin of one channel, quantised to i16.
#[derive(Clone, Copy, Default)]
struct MinMax {
    min: i16,
    max: i16,
}

impl MinMax {
    const EMPTY: MinMax = MinMax {
        min: i16::MAX,
        max: i16::MIN,
    };

    fn merge(self, other: MinMax) -> MinMax {
        MinMax {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
        }
    }
}

fn quantise(x: f32) -> i16 {
    (x.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

struct Levels {
    /// `bin * channels + channel`
    fine: Vec<MinMax>,
    coarse: Vec<MinMax>,
}

pub struct Peaks {
    pub channels: usize,
    pub total_frames: u64,
    levels: RwLock<Levels>,
    done_frames: AtomicU64,
}

impl Peaks {
    fn new(channels: usize, total_frames: u64) -> Self {
        let bins = |per: u64| total_frames.div_ceil(per) as usize * channels;
        Peaks {
            channels,
            total_frames,
            levels: RwLock::new(Levels {
                fine: vec![MinMax::EMPTY; bins(FINE)],
                coarse: vec![MinMax::EMPTY; bins(COARSE)],
            }),
            done_frames: AtomicU64::new(0),
        }
    }

    /// Frames summarised so far, from the start of the file.
    pub fn done_frames(&self) -> u64 {
        self.done_frames.load(Ordering::Acquire)
    }

    pub fn is_complete(&self) -> bool {
        self.done_frames() >= self.total_frames
    }

    pub fn read(&self) -> PeaksReader<'_> {
        PeaksReader {
            peaks: self,
            levels: self.levels.read().unwrap(),
            done: self.done_frames(),
        }
    }
}

pub struct PeaksReader<'a> {
    peaks: &'a Peaks,
    levels: RwLockReadGuard<'a, Levels>,
    done: u64,
}

impl PeaksReader<'_> {
    /// Min and max of `channel` over frames `[start, end)`, or `None` if not computed yet.
    pub fn min_max(&self, channel: usize, start: f64, end: f64) -> Option<(f32, f32)> {
        let start = start.max(0.0) as u64;
        let end = (end.max(0.0).ceil() as u64).min(self.done).max(start + 1);
        if start >= self.done {
            return None;
        }
        let (bins, per) = if end - start >= COARSE * 2 {
            (&self.levels.coarse, COARSE)
        } else {
            (&self.levels.fine, FINE)
        };
        let channels = self.peaks.channels;
        let first = (start / per) as usize;
        let last = (end.div_ceil(per) as usize).max(first + 1);
        let mut acc = MinMax::EMPTY;
        for bin in first..last {
            if let Some(m) = bins.get(bin * channels + channel) {
                acc = acc.merge(*m);
            }
        }
        (acc.min <= acc.max).then(|| (f32::from(acc.min) / 32767.0, f32::from(acc.max) / 32767.0))
    }
}

/// Handle to a running (or finished) summary. Dropping it stops the worker.
pub struct PeakJob {
    pub peaks: Arc<Peaks>,
    cancel: Arc<AtomicBool>,
}

impl Drop for PeakJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn start(path: &Path, channels: usize, total_frames: u64) -> PeakJob {
    let peaks = Arc::new(Peaks::new(channels, total_frames));
    let cancel = Arc::new(AtomicBool::new(false));

    let (worker_peaks, worker_cancel, path) = (peaks.clone(), cancel.clone(), path.to_path_buf());
    thread::Builder::new()
        .name("peaks".into())
        .spawn(move || {
            let cache = cache_path(&path);
            if let Err(err) = run(&path, cache, &worker_peaks, &worker_cancel) {
                eprintln!("peaks for {}: {err:#}", path.display());
            }
        })
        .expect("spawn peaks thread");

    PeakJob { peaks, cancel }
}

fn run(path: &Path, cache: Option<PathBuf>, peaks: &Peaks, cancel: &AtomicBool) -> Result<()> {
    if let Some(cache) = &cache
        && load_cache(cache, peaks).is_ok()
    {
        return Ok(());
    }
    // A file on a computer that is not downloaded: its waveform waits until
    // it is (reading it all over the network just for this is too much).
    if crate::vfs::is_remote(path) && !crate::remote::has_copy(path) {
        anyhow::bail!("not downloaded yet");
    }

    let (_, mut source) = decode::open(path)?;
    let channels = peaks.channels;
    // A whole number of coarse bins per block, so each block finishes its coarse bins.
    let block = COARSE as usize * 4;
    let mut buf = Vec::with_capacity(block * channels);
    let mut fine = Vec::with_capacity(block / FINE as usize * channels);
    let mut frame = 0u64;

    while !cancel.load(Ordering::Relaxed) {
        buf.clear();
        let read = source.read(&mut buf, block)?;
        if read == 0 {
            break;
        }

        fine.clear();
        for chunk in buf.chunks(FINE as usize * channels) {
            let start = fine.len();
            fine.resize(start + channels, MinMax::EMPTY);
            for frame in chunk.chunks_exact(channels) {
                for (acc, &s) in fine[start..].iter_mut().zip(frame) {
                    let q = quantise(s);
                    *acc = acc.merge(MinMax { min: q, max: q });
                }
            }
        }

        let mut levels = peaks.levels.write().unwrap();
        let first_fine = (frame / FINE) as usize * channels;
        levels.fine[first_fine..first_fine + fine.len()].copy_from_slice(&fine);
        let first_coarse = (frame / COARSE) as usize;
        for (i, group) in fine.chunks(channels * (COARSE / FINE) as usize).enumerate() {
            for ch in 0..channels {
                let merged = group
                    .iter()
                    .skip(ch)
                    .step_by(channels)
                    .fold(MinMax::EMPTY, |a, &b| a.merge(b));
                levels.coarse[(first_coarse + i) * channels + ch] = merged;
            }
        }
        drop(levels);

        frame += read as u64;
        peaks.done_frames.store(frame, Ordering::Release);
    }

    if frame >= peaks.total_frames
        && let Some(cache) = cache
        && let Err(err) = save_cache(&cache, peaks)
    {
        eprintln!("cannot write peak cache {}: {err:#}", cache.display());
    }
    Ok(())
}

fn cache_path(path: &Path) -> Option<PathBuf> {
    let fingerprint = super::fingerprint(path).ok()?;
    Some(
        super::cache_dir()?
            .join("peaks")
            .join(format!("{fingerprint:016x}.peaks")),
    )
}

fn load_cache(cache: &Path, peaks: &Peaks) -> Result<()> {
    let mut r = BufReader::new(fs::File::open(cache)?);
    let mut header = [0u8; 8 + 2 + 8];
    r.read_exact(&mut header)?;
    ensure!(&header[..8] == CACHE_MAGIC, "bad magic");
    let channels = u16::from_le_bytes(header[8..10].try_into().unwrap()) as usize;
    let frames = u64::from_le_bytes(header[10..18].try_into().unwrap());
    ensure!(
        channels == peaks.channels && frames == peaks.total_frames,
        "cache is for another file"
    );

    let mut guard = peaks.levels.write().unwrap();
    let levels = &mut *guard;
    for level in [&mut levels.fine, &mut levels.coarse] {
        let mut bytes = vec![0u8; level.len() * 4];
        r.read_exact(&mut bytes)?;
        for (m, b) in level.iter_mut().zip(bytes.chunks_exact(4)) {
            m.min = i16::from_le_bytes([b[0], b[1]]);
            m.max = i16::from_le_bytes([b[2], b[3]]);
        }
    }
    drop(guard);
    peaks
        .done_frames
        .store(peaks.total_frames, Ordering::Release);
    Ok(())
}

fn save_cache(cache: &Path, peaks: &Peaks) -> Result<()> {
    let dir = cache.parent().context("cache path has no parent")?;
    fs::create_dir_all(dir)?;
    let tmp = cache.with_extension("tmp");
    {
        let mut w = BufWriter::new(fs::File::create(&tmp)?);
        w.write_all(CACHE_MAGIC)?;
        w.write_all(&(peaks.channels as u16).to_le_bytes())?;
        w.write_all(&peaks.total_frames.to_le_bytes())?;
        let levels = peaks.levels.read().unwrap();
        for level in [&levels.fine, &levels.coarse] {
            for m in level.iter() {
                w.write_all(&m.min.to_le_bytes())?;
                w.write_all(&m.max.to_le_bytes())?;
            }
        }
        w.flush()?;
    }
    fs::rename(tmp, cache)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn decibel_scale() {
        assert_eq!(db_scale(1.0), 1.0);
        assert_eq!(db_scale(-1.0), -1.0);
        assert_eq!(db_scale(0.0), 0.0);
        assert!((db_scale(0.01) - 1.0 / 3.0).abs() < 1e-5); // -40 dB
        assert_eq!(db_scale(0.0001), 0.0); // -80 dB
    }

    use super::*;
    use crate::audio::bwf::tests::recorder_like;

    #[test]
    fn summarises_and_round_trips_through_cache() {
        // 40000 frames: more than one coarse bin, last bins partial.
        let frames: Vec<[f32; 2]> = (0..40000)
            .map(|i| match i {
                100 => [0.5, -0.25],
                20000 => [-1.5, 0.75],
                _ => [0.0, 0.0],
            })
            .collect();
        let dir = std::env::temp_dir().join(format!("sc-peaks-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("take.wav");
        fs::write(&wav, recorder_like(&frames)).unwrap();
        let cache = dir.join("take.peaks");

        let peaks = Peaks::new(2, 40000);
        run(&wav, Some(cache.clone()), &peaks, &AtomicBool::new(false)).unwrap();
        assert!(peaks.is_complete());

        let check = |peaks: &Peaks| {
            let r = peaks.read();
            let (min, max) = r.min_max(0, 0.0, 40000.0).unwrap();
            assert!(min == -1.0 && (max - 0.5).abs() < 1e-4); // clamped to full scale
            let (min, max) = r.min_max(1, 0.0, 256.0).unwrap();
            assert!((min + 0.25).abs() < 1e-4 && max == 0.0);
            let (min, max) = r.min_max(1, 30000.0, 40000.0).unwrap();
            assert_eq!((min, max), (0.0, 0.0));
        };
        check(&peaks);

        let cached = Peaks::new(2, 40000);
        load_cache(&cache, &cached).unwrap();
        check(&cached);

        assert!(load_cache(&cache, &Peaks::new(1, 40000)).is_err());
        fs::remove_dir_all(dir).ok();
    }
}
