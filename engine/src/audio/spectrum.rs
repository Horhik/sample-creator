//! What frequencies sound when: a spectrogram of the recording, computed in
//! the background like the peak summary and cached next to it.
//!
//! The waveform only shows loudness, so a duck quacking over running water
//! looks like more of the same noise. Split into bands, the two differ: a
//! quack is a stack of tones in the middle, water is spread evenly. The same
//! analysis gives three views: the spectrogram itself, a waveform coloured by
//! where its energy sits, and a curve that rises where the sound changes
//! (spectral flux), so events stand out of a steady background.
//!
//! One column per `hop` frames (about 20 ms) of the channels mixed down,
//! [`BANDS`] log-spaced bands from 40 Hz up, each stored as a dB level in a
//! byte. A coarse level merges [`COARSE`] columns for zoomed-out views.

use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock, RwLockReadGuard};
use std::thread;

use anyhow::{Context, Result, ensure};
use realfft::RealFftPlanner;

use super::decode;

/// Frequency bands of the spectrogram, lowest first.
pub const BANDS: usize = 64;
/// Columns merged into one coarse column.
const COARSE: usize = 32;
const LOW_HZ: f32 = 40.0;
const HIGH_HZ: f32 = 16_000.0;
/// Range of a stored level: 0 is this many dB below full scale, 255 is 0 dB.
const RANGE_DB: f32 = 120.0;
/// Range shown by the spectrogram, down from the loudest level of the file.
const SHOWN_DB: f32 = 70.0;
/// Edges of the colour bands of the coloured waveform.
const MID_HZ: f32 = 300.0;
const TREBLE_HZ: f32 = 3_000.0;
const CACHE_MAGIC: &[u8; 8] = b"SCSPEC01";

/// Frames per column for a sample rate: a power of two near 20 ms.
pub fn hop_for(rate: u32) -> u64 {
    u64::from(rate / 50).next_power_of_two().clamp(256, 8192)
}

/// One column: band levels, levels of the three colour bands (low, middle,
/// high) and the flux from the column before.
#[derive(Clone, Default)]
struct Columns {
    levels: Vec<u8>,
    colors: Vec<u8>,
    flux: Vec<f32>,
}

impl Columns {
    fn new(n: usize) -> Self {
        Columns {
            levels: vec![0; n * BANDS],
            colors: vec![0; n * 3],
            flux: vec![0.0; n],
        }
    }

    /// Merges column `from` of `src` into column `to` (maximum).
    fn merge(&mut self, to: usize, src: &Columns, from: usize) {
        for b in 0..BANDS {
            let v = &mut self.levels[to * BANDS + b];
            *v = (*v).max(src.levels[from * BANDS + b]);
        }
        for b in 0..3 {
            let v = &mut self.colors[to * 3 + b];
            *v = (*v).max(src.colors[from * 3 + b]);
        }
        self.flux[to] = self.flux[to].max(src.flux[from]);
    }
}

struct Data {
    fine: Columns,
    coarse: Columns,
}

pub struct Spectrum {
    pub hop: u64,
    pub columns: usize,
    total_frames: u64,
    data: RwLock<Data>,
    done: AtomicUsize,
    /// Loudest level so far: the top of the spectrogram's colour scale.
    loudest: AtomicU8,
}

impl Spectrum {
    fn new(rate: u32, total_frames: u64) -> Self {
        let hop = hop_for(rate);
        let columns = total_frames.div_ceil(hop) as usize;
        Spectrum {
            hop,
            columns,
            total_frames,
            data: RwLock::new(Data {
                fine: Columns::new(columns),
                coarse: Columns::new(columns.div_ceil(COARSE)),
            }),
            done: AtomicUsize::new(0),
            loudest: AtomicU8::new(0),
        }
    }

    /// Columns computed so far, from the start of the file. Grows while the
    /// analysis runs: the view redraws when it changes.
    pub fn done(&self) -> usize {
        self.done.load(Ordering::Acquire)
    }

    pub fn done_frames(&self) -> u64 {
        (self.done() as u64 * self.hop).min(self.total_frames)
    }

    pub fn read(&self) -> SpectrumReader<'_> {
        SpectrumReader {
            spectrum: self,
            data: self.data.read().unwrap(),
            done: self.done(),
        }
    }
}

/// What a stretch of the recording looks like, one entry per pixel column.
#[derive(Clone, Debug, Default)]
pub struct SpectrumView {
    pub columns: usize,
    /// `column * BANDS + band`, lowest band first: brightness 0..=255 on the
    /// file's scale (the loudest level is 255). 0 where not computed yet.
    pub levels: Vec<u8>,
    /// Colour of the waveform at each column, `0xRRGGBB`, from where its
    /// energy sits: red low, green middle, blue high. `None` where not
    /// computed yet.
    pub colors: Vec<Option<u32>>,
    /// The sound changing (spectral flux) 0..=1 over a steady background,
    /// scaled to the stretch shown. `None` where not computed yet.
    pub events: Vec<Option<f32>>,
}

pub struct SpectrumReader<'a> {
    spectrum: &'a Spectrum,
    data: RwLockReadGuard<'a, Data>,
    done: usize,
}

impl SpectrumReader<'_> {
    /// `columns` pixel columns from frame `start`, `fpp` frames each.
    pub fn view(&self, start: f64, fpp: f64, columns: usize) -> SpectrumView {
        let sp = self.spectrum;
        let hop = sp.hop as f64;
        // Zoomed far out a pixel spans many columns: merge coarse ones.
        let coarse = fpp / hop >= COARSE as f64;
        let (src, per) = if coarse {
            (&self.data.coarse, (sp.hop * COARSE as u64) as f64)
        } else {
            (&self.data.fine, hop)
        };
        let done = if coarse {
            // A coarse column counts once all of it is in, or the file ends.
            if self.done >= sp.columns {
                src.flux.len()
            } else {
                self.done / COARSE
            }
        } else {
            self.done
        };

        let top = f32::from(sp.loudest.load(Ordering::Relaxed));
        let span = SHOWN_DB / RANGE_DB * 255.0;
        let floor = top - span;

        let mut view = SpectrumView {
            columns,
            levels: vec![0; columns * BANDS],
            colors: vec![None; columns],
            events: vec![None; columns],
        };
        let mut flux = vec![f32::NAN; columns];
        let mut band = [0u8; BANDS];
        for (x, fx) in flux.iter_mut().enumerate() {
            let a = start + x as f64 * fpp;
            let b = a + fpp;
            if a < 0.0 {
                continue;
            }
            let first = (a / per) as usize;
            let last = ((b / per).ceil() as usize).max(first + 1).min(done);
            if first >= last {
                continue;
            }
            band.fill(0);
            let (mut colors, mut f) = ([0u8; 3], 0f32);
            for c in first..last {
                for (v, &l) in band.iter_mut().zip(&src.levels[c * BANDS..(c + 1) * BANDS]) {
                    *v = (*v).max(l);
                }
                for (v, &l) in colors.iter_mut().zip(&src.colors[c * 3..c * 3 + 3]) {
                    *v = (*v).max(l);
                }
                f = f.max(src.flux[c]);
            }
            for (out, &l) in view.levels[x * BANDS..(x + 1) * BANDS].iter_mut().zip(&band) {
                *out = ((f32::from(l) - floor) / span * 255.0).clamp(0.0, 255.0) as u8;
            }
            view.colors[x] = Some(wave_color(colors));
            *fx = f;
        }

        // Events: flux over its typical value in the stretch, so a steady
        // noise stays low and whatever breaks it rises.
        let mut known: Vec<f32> = flux.iter().copied().filter(|f| !f.is_nan()).collect();
        if !known.is_empty() {
            known.sort_by(f32::total_cmp);
            let base = known[(known.len() - 1) / 2];
            let peak = known[known.len() - 1];
            // A floor: a steady stretch does not blow its jitter up into events.
            let scale = (peak - base).max(1.5);
            for (e, &f) in view.events.iter_mut().zip(&flux) {
                if !f.is_nan() {
                    *e = Some(((f - base) / scale).clamp(0.0, 1.0));
                }
            }
        }
        view
    }
}

/// Colour of a column from the levels of its low, middle and high bands:
/// the bands are weighed so the strongest one dominates.
fn wave_color(levels: [u8; 3]) -> u32 {
    const HUES: [[f32; 3]; 3] = [[235.0, 70.0, 60.0], [70.0, 205.0, 90.0], [70.0, 145.0, 255.0]];
    // Natural sound has more energy low: lift the upper bands a little.
    const LIFT_DB: [f32; 3] = [0.0, 4.0, 8.0];
    let db = |i: usize| f32::from(levels[i]) / 255.0 * RANGE_DB + LIFT_DB[i];
    let top = (0..3).map(db).fold(f32::MIN, f32::max);
    let weights: Vec<f32> = (0..3).map(|i| ((db(i) - top) / 5.0).exp()).collect();
    let sum: f32 = weights.iter().sum();
    let mut rgb = [0u32; 3];
    for (c, out) in rgb.iter_mut().enumerate() {
        let v: f32 = (0..3).map(|i| HUES[i][c] * weights[i]).sum::<f32>() / sum;
        *out = v.round().clamp(0.0, 255.0) as u32;
    }
    (rgb[0] << 16) | (rgb[1] << 8) | rgb[2]
}

/// Spectrogram colours: dark violet through red and orange to pale yellow
/// (like "magma"), `0xRRGGBB` for each brightness.
pub fn colormap() -> [u32; 256] {
    const STOPS: [(f32, [f32; 3]); 6] = [
        (0.0, [0.0, 0.0, 4.0]),
        (0.2, [40.0, 11.0, 84.0]),
        (0.45, [140.0, 41.0, 129.0]),
        (0.65, [222.0, 73.0, 104.0]),
        (0.85, [254.0, 159.0, 109.0]),
        (1.0, [252.0, 253.0, 191.0]),
    ];
    let mut out = [0u32; 256];
    for (i, o) in out.iter_mut().enumerate() {
        let t = i as f32 / 255.0;
        let k = STOPS.windows(2).position(|w| t <= w[1].0).unwrap_or(4);
        let ((t0, c0), (t1, c1)) = (STOPS[k], STOPS[k + 1]);
        let f = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
        let ch = |j: usize| (c0[j] + (c1[j] - c0[j]) * f).round() as u32;
        *o = (ch(0) << 16) | (ch(1) << 8) | ch(2);
    }
    out
}

/// Handle to a running (or finished) analysis. Dropping it stops the worker.
pub struct SpectrumJob {
    pub spectrum: Arc<Spectrum>,
    cancel: Arc<AtomicBool>,
}

impl Drop for SpectrumJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub fn start(path: &Path, rate: u32, total_frames: u64) -> SpectrumJob {
    let spectrum = Arc::new(Spectrum::new(rate, total_frames));
    let cancel = Arc::new(AtomicBool::new(false));
    let (worker, worker_cancel, path) = (spectrum.clone(), cancel.clone(), path.to_path_buf());
    thread::Builder::new()
        .name("spectrum".into())
        .spawn(move || {
            let cache = cache_path(&path);
            if let Err(err) = run(&path, rate, cache, &worker, &worker_cancel) {
                eprintln!("spectrum for {}: {err:#}", path.display());
            }
        })
        .expect("spawn spectrum thread");
    SpectrumJob { spectrum, cancel }
}

/// Which bands each FFT bin feeds, for a window of `n` at `rate`.
struct Bands {
    /// `(first bin, last bin + 1)` per band.
    bins: Vec<(usize, usize)>,
    /// Colour band of each band.
    color: Vec<usize>,
}

impl Bands {
    fn new(n: usize, rate: u32) -> Self {
        let bin_hz = rate as f32 / n as f32;
        let high = HIGH_HZ.min(rate as f32 / 2.0 * 0.95).max(LOW_HZ * 2.0);
        let edge = |i: usize| LOW_HZ * (high / LOW_HZ).powf(i as f32 / BANDS as f32);
        let last_bin = n / 2;
        let mut bins = Vec::with_capacity(BANDS);
        let mut color = Vec::with_capacity(BANDS);
        for b in 0..BANDS {
            let (lo, hi) = (edge(b), edge(b + 1));
            let mut first = (lo / bin_hz).ceil() as usize;
            let mut end = (hi / bin_hz).ceil() as usize;
            if end <= first {
                // Narrower than a bin: the nearest one.
                first = ((lo + hi) / 2.0 / bin_hz).round() as usize;
                end = first + 1;
            }
            bins.push((first.min(last_bin), end.min(last_bin + 1)));
            let centre = (lo * hi).sqrt();
            color.push(if centre < MID_HZ {
                0
            } else if centre < TREBLE_HZ {
                1
            } else {
                2
            });
        }
        Bands { bins, color }
    }
}

fn to_level(power: f32) -> u8 {
    let db = 10.0 * power.max(1e-20).log10();
    ((db + RANGE_DB) / RANGE_DB * 255.0).round().clamp(0.0, 255.0) as u8
}

fn run(
    path: &Path,
    rate: u32,
    cache: Option<PathBuf>,
    sp: &Spectrum,
    cancel: &AtomicBool,
) -> Result<()> {
    if let Some(cache) = &cache
        && load_cache(cache, sp).is_ok()
    {
        return Ok(());
    }
    if crate::vfs::is_remote(path) && !crate::remote::has_copy(path) {
        anyhow::bail!("not downloaded yet");
    }

    let (_, mut source) = decode::open(path)?;
    let hop = sp.hop as usize;
    let n = hop * 2;
    let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
    let mut input = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    let mut scratch = fft.make_scratch_vec();
    let window: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
        .collect();
    // A full-scale sine comes out at 0 dB.
    let norm = 4.0 / (window.iter().sum::<f32>().powi(2));
    let bands = Bands::new(n, rate);

    // Column c is centred on frame c * hop + hop / 2.
    let mut mono: Vec<f32> = vec![0.0; hop / 2];
    let mut buf = Vec::new();
    let mut ended = false;
    let mut col = 0usize;
    let mut prev = [0u8; BANDS];
    let mut loudest = 0u8;
    // Columns computed but not yet published (published in batches).
    let mut batch = Columns::new(0);
    let block = hop * 64;

    while col < sp.columns && !cancel.load(Ordering::Relaxed) {
        if !ended && mono.len() < n + block {
            buf.clear();
            let read = source.read(&mut buf, block)?;
            if let Some(channels) = buf.len().checked_div(read) {
                let k = 1.0 / channels as f32;
                mono.extend(buf.chunks_exact(channels).map(|f| f.iter().sum::<f32>() * k));
            } else {
                ended = true;
            }
            continue;
        }
        if mono.len() < n {
            mono.resize(n, 0.0);
        }

        for (dst, (&s, &w)) in input.iter_mut().zip(mono.iter().zip(&window)) {
            *dst = s * w;
        }
        fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch)
            .map_err(|e| anyhow::anyhow!("fft: {e}"))?;

        let mut levels = [0u8; BANDS];
        let mut color_power = [0f32; 3];
        for (b, &(first, end)) in bands.bins.iter().enumerate() {
            let power: f32 = spectrum[first..end].iter().map(|c| c.norm_sqr()).sum::<f32>()
                * norm
                / (end - first) as f32;
            levels[b] = to_level(power);
            color_power[bands.color[b]] += power;
        }
        let flux = levels
            .iter()
            .zip(&prev)
            .map(|(&l, &p)| f32::from(l.saturating_sub(p)))
            .sum::<f32>()
            / BANDS as f32
            * RANGE_DB
            / 255.0;
        prev = levels;
        loudest = loudest.max(*levels.iter().max().unwrap());

        batch.levels.extend_from_slice(&levels);
        batch.colors.extend(color_power.iter().map(|&p| to_level(p)));
        batch.flux.push(if col == 0 { 0.0 } else { flux });
        col += 1;
        mono.drain(..hop.min(mono.len()));

        if batch.flux.len() >= 256 || col >= sp.columns {
            publish(sp, &batch, col);
            sp.loudest.store(loudest, Ordering::Relaxed);
            batch = Columns::new(0);
        }
    }

    if col >= sp.columns
        && let Some(cache) = cache
        && let Err(err) = save_cache(&cache, sp)
    {
        eprintln!("cannot write spectrum cache {}: {err:#}", cache.display());
    }
    Ok(())
}

/// Stores `batch`, the columns just before `end`, and the coarse columns they feed.
fn publish(sp: &Spectrum, batch: &Columns, end: usize) {
    let n = batch.flux.len();
    let first = end - n;
    let mut data = sp.data.write().unwrap();
    let data = &mut *data;
    data.fine.levels[first * BANDS..end * BANDS].copy_from_slice(&batch.levels);
    data.fine.colors[first * 3..end * 3].copy_from_slice(&batch.colors);
    data.fine.flux[first..end].copy_from_slice(&batch.flux);
    for c in first..end {
        data.coarse.merge(c / COARSE, &data.fine, c);
    }
    sp.done.store(end, Ordering::Release);
}

fn cache_path(path: &Path) -> Option<PathBuf> {
    let fingerprint = super::fingerprint(path).ok()?;
    Some(
        super::cache_dir()?
            .join("spectrum")
            .join(format!("{fingerprint:016x}.spectrum")),
    )
}

fn load_cache(cache: &Path, sp: &Spectrum) -> Result<()> {
    let mut r = BufReader::new(fs::File::open(cache)?);
    let mut header = [0u8; 8 + 8 + 8];
    r.read_exact(&mut header)?;
    ensure!(&header[..8] == CACHE_MAGIC, "bad magic");
    let hop = u64::from_le_bytes(header[8..16].try_into().unwrap());
    let frames = u64::from_le_bytes(header[16..24].try_into().unwrap());
    ensure!(
        hop == sp.hop && frames == sp.total_frames,
        "cache is for another file"
    );
    let mut fine = Columns::new(sp.columns);
    r.read_exact(&mut fine.levels)?;
    r.read_exact(&mut fine.colors)?;
    let mut bytes = vec![0u8; sp.columns * 4];
    r.read_exact(&mut bytes)?;
    for (f, b) in fine.flux.iter_mut().zip(bytes.chunks_exact(4)) {
        *f = f32::from_le_bytes(b.try_into().unwrap());
    }
    let loudest = fine.levels.iter().copied().max().unwrap_or(0);
    let mut coarse = Columns::new(sp.columns.div_ceil(COARSE));
    for c in 0..sp.columns {
        coarse.merge(c / COARSE, &fine, c);
    }
    *sp.data.write().unwrap() = Data { fine, coarse };
    sp.loudest.store(loudest, Ordering::Relaxed);
    sp.done.store(sp.columns, Ordering::Release);
    Ok(())
}

fn save_cache(cache: &Path, sp: &Spectrum) -> Result<()> {
    let dir = cache.parent().context("cache path has no parent")?;
    fs::create_dir_all(dir)?;
    let tmp = cache.with_extension("tmp");
    {
        let mut w = BufWriter::new(fs::File::create(&tmp)?);
        w.write_all(CACHE_MAGIC)?;
        w.write_all(&sp.hop.to_le_bytes())?;
        w.write_all(&sp.total_frames.to_le_bytes())?;
        let data = sp.data.read().unwrap();
        w.write_all(&data.fine.levels)?;
        w.write_all(&data.fine.colors)?;
        for f in &data.fine.flux {
            w.write_all(&f.to_le_bytes())?;
        }
        w.flush()?;
    }
    fs::rename(tmp, cache)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::bwf::tests::recorder_like;

    #[test]
    fn finds_a_tone_and_its_start() {
        // 2 s at 48 kHz: noise-free silence, then a 1 kHz tone from 1 s.
        let rate = 48_000;
        let frames: Vec<[f32; 2]> = (0..rate * 2)
            .map(|i| {
                let s = if i >= rate {
                    0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / rate as f32).sin()
                } else {
                    0.0
                };
                [s, s]
            })
            .collect();
        let dir = std::env::temp_dir().join(format!("sc-spectrum-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");
        fs::write(&wav, recorder_like(&frames)).unwrap();
        let cache = dir.join("tone.spectrum");

        let total = u64::from(rate) * 2;
        let sp = Spectrum::new(rate, total);
        run(&wav, rate, Some(cache.clone()), &sp, &AtomicBool::new(false)).unwrap();
        assert_eq!(sp.done(), sp.columns);

        let check = |sp: &Spectrum| {
            // Quarters of the file: silence, silence, the tone starts, tone.
            let v = sp.read().view(0.0, total as f64 / 4.0, 4);
            assert!(v.levels[..BANDS].iter().all(|&l| l == 0));
            let b = (0..BANDS).max_by_key(|&b| v.levels[3 * BANDS + b]).unwrap();
            let edge = |i: usize| LOW_HZ * (HIGH_HZ / LOW_HZ).powf(i as f32 / BANDS as f32);
            assert!(edge(b) <= 1000.0 && 1000.0 < edge(b + 1) * 1.1, "band {b}");
            // A middle tone is green.
            let c = v.colors[3].unwrap();
            assert!((c >> 8 & 0xff) > (c >> 16) && (c >> 8 & 0xff) > (c & 0xff));
            // The tone's start is the event; the steady tone is not.
            assert_eq!(v.events[2], Some(1.0));
            assert_eq!(v.events[3], Some(0.0));
        };
        check(&sp);

        let cached = Spectrum::new(rate, total);
        load_cache(&cache, &cached).unwrap();
        check(&cached);
        assert!(load_cache(&cache, &Spectrum::new(rate, total - 1)).is_err());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn colormap_runs_dark_to_light() {
        let map = colormap();
        let luma = |c: u32| (c >> 16) + (c >> 8 & 0xff) + (c & 0xff);
        assert!(luma(map[0]) < 20 && luma(map[255]) > 650);
    }
}
