//! Samples of the visible stretch, for the detailed waveform at deep zoom.
//!
//! The peak summary stops at 256 frames per bin; closer than that the
//! detailed view reads the frames themselves. A background thread keeps one
//! window of the file in memory (the view plus a margin on each side), so
//! drawing never waits on the disk: until the window arrives, the summary is
//! drawn instead.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::thread;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, unbounded};

use super::decode::{self, FrameSource};

/// Deepest zoom of the detailed waveform: one sample per pixel.
pub const MIN_FRAMES_PER_PX: f64 = 1.0;

/// Largest window kept, in frames (8 MB for stereo).
const MAX_FRAMES: u64 = 1 << 20;

#[derive(Default)]
struct Window {
    start: u64,
    /// Interleaved.
    samples: Vec<f32>,
}

struct Shared {
    channels: usize,
    total_frames: u64,
    window: RwLock<Window>,
    /// Bumped when a new window is in: the view redraws.
    generation: AtomicU64,
}

/// Handle to the window loader. Dropping it stops the thread.
pub struct Detail {
    shared: Arc<Shared>,
    want: Sender<(u64, u64)>,
}

impl Detail {
    pub fn start(path: &Path, channels: usize, total_frames: u64) -> Detail {
        let shared = Arc::new(Shared {
            channels: channels.max(1),
            total_frames,
            window: RwLock::new(Window::default()),
            generation: AtomicU64::new(0),
        });
        let (want, rx) = unbounded();
        let (worker, path) = (shared.clone(), path.to_path_buf());
        thread::Builder::new()
            .name("wave-detail".into())
            .spawn(move || run(path, &worker, rx))
            .expect("spawn wave detail thread");
        Detail { shared, want }
    }

    /// Frames `[start, end)` are on screen. Cheap; the latest request wins.
    pub fn want(&self, start: f64, end: f64) {
        let start = start.max(0.0) as u64;
        let end = (end.max(0.0).ceil() as u64).min(self.shared.total_frames);
        if end > start && !self.read().covers(start, end) {
            self.want.send((start, end)).ok();
        }
    }

    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    pub fn read(&self) -> DetailReader<'_> {
        DetailReader {
            shared: &self.shared,
            window: self.shared.window.read().unwrap(),
        }
    }
}

pub struct DetailReader<'a> {
    shared: &'a Shared,
    window: RwLockReadGuard<'a, Window>,
}

impl DetailReader<'_> {
    fn frames(&self) -> u64 {
        (self.window.samples.len() / self.shared.channels) as u64
    }

    fn covers(&self, start: u64, end: u64) -> bool {
        let w = &self.window;
        !w.samples.is_empty() && w.start <= start && end <= w.start + self.frames()
    }

    /// Min and max of `channel` over frames `[start, end)` and the frame
    /// right after, so neighbouring columns join into one line even when a
    /// column holds a single sample. `None` when the window does not hold them.
    pub fn min_max(&self, channel: usize, start: f64, end: f64) -> Option<(f32, f32)> {
        let a = start.max(0.0).floor() as u64;
        let b = (end.max(0.0).ceil() as u64 + 1)
            .min(self.shared.total_frames)
            .max(a + 1);
        if !self.covers(a, b) {
            return None;
        }
        let ch = self.shared.channels;
        let from = (a - self.window.start) as usize;
        let to = (b - self.window.start) as usize;
        let (mut lo, mut hi) = (f32::MAX, f32::MIN);
        for frame in self.window.samples[from * ch..to * ch].chunks_exact(ch) {
            let s = frame[channel.min(ch - 1)];
            lo = lo.min(s);
            hi = hi.max(s);
        }
        Some((lo, hi))
    }
}

fn run(path: PathBuf, shared: &Shared, rx: Receiver<(u64, u64)>) {
    let mut source: Option<Box<dyn FrameSource>> = None;
    // Ends when the handle is dropped.
    while let Ok(mut want) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            want = newer;
        }
        let (start, end) = want;
        if shared_covers(shared, start, end) {
            continue;
        }
        let span = end - start;
        if span > MAX_FRAMES {
            continue;
        }
        // Room to scroll either way without waiting.
        let margin = ((MAX_FRAMES - span) / 2).min(span);
        let from = start.saturating_sub(margin);
        let to = (end + margin).min(shared.total_frames);
        match load(&path, &mut source, shared.channels, from, to) {
            Ok(samples) => {
                *shared.window.write().unwrap() = Window {
                    start: from,
                    samples,
                };
                shared.generation.fetch_add(1, Ordering::Release);
            }
            Err(err) => {
                eprintln!("wave detail for {}: {err:#}", path.display());
                source = None;
            }
        }
    }
}

fn shared_covers(shared: &Shared, start: u64, end: u64) -> bool {
    DetailReader {
        shared,
        window: shared.window.read().unwrap(),
    }
    .covers(start, end)
}

fn load(
    path: &Path,
    source: &mut Option<Box<dyn FrameSource>>,
    channels: usize,
    from: u64,
    to: u64,
) -> Result<Vec<f32>> {
    if source.is_none() {
        *source = Some(decode::open(path)?.1);
    }
    let source = source.as_mut().unwrap();
    source.seek(from)?;
    let want = (to - from) as usize;
    let mut samples = Vec::with_capacity(want * channels);
    let mut got = 0;
    while got < want {
        let n = source.read(&mut samples, (want - got).min(65536))?;
        if n == 0 {
            break;
        }
        got += n;
    }
    samples.truncate(got * channels);
    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_samples_of_the_view() {
        let dir = std::env::temp_dir().join(format!("sc-detail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ramp.wav");
        // Mono 16-bit: frame i holds i (as a fraction of full scale).
        let frames = 10_000u32;
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + frames * 2).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&48000u32.to_le_bytes());
        wav.extend_from_slice(&96000u32.to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(frames * 2).to_le_bytes());
        for i in 0..frames {
            wav.extend_from_slice(&(i as i16).to_le_bytes());
        }
        std::fs::write(&path, wav).unwrap();

        let detail = Detail::start(&path, 1, u64::from(frames));
        assert!(detail.read().min_max(0, 100.0, 101.0).is_none());
        detail.want(100.0, 200.0);
        for _ in 0..200 {
            if detail.generation() > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let (lo, hi) = detail.read().min_max(0, 100.0, 101.0).unwrap();
        // Frame 100 and the one after it.
        assert_eq!((lo * 32768.0).round(), 100.0);
        assert_eq!((hi * 32768.0).round(), 101.0);
        std::fs::remove_dir_all(dir).ok();
    }
}
