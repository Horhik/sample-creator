//! Playback: a cpal output stream fed by a decoder thread. The stream is
//! reopened when its device goes away (headphones, a Bluetooth headset
//! connecting), so playback follows the system's current output.
//!
//! The decoder thread reads the current file from the play cursor, maps channels
//! to the device layout, resamples (linear; for preview only) and queues chunks.
//! Each chunk remembers which source frame it starts at, so the audio callback can
//! publish an exact playhead position, including across loop wrap-arounds.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};

use super::decode::{self, FrameSource};

/// How much audio the decoder keeps queued ahead of the callback.
const QUEUE_SECS: f64 = 0.25;
const DECODE_BLOCK: usize = 2048;

struct Chunk {
    /// Interleaved, device channel layout and rate.
    samples: Vec<f32>,
    read: usize,
    /// Source frame of the first output frame, and source frames per output frame.
    src_start: f64,
    step: f64,
}

struct Shared {
    queue: Mutex<VecDeque<Chunk>>,
    playing: AtomicBool,
    /// Decoder hit the end of the range (and is not looping).
    exhausted: AtomicBool,
    /// Playhead in source frames, as f64 bits.
    position: AtomicU64,
    /// Output gain as f32 bits: a region made louder is heard louder in markup.
    gain: std::sync::atomic::AtomicU32,
    /// Listening volume as f32 bits, on top of `gain`; kept across loads.
    monitor: std::sync::atomic::AtomicU32,
    /// Last audio callback, ms since `epoch`: a stream that stopped asking
    /// for audio while playing is reopened.
    last_fill: AtomicU64,
    epoch: Instant,
    /// The player is gone: the output thread closes the stream.
    closed: AtomicBool,
}

/// Playing, yet the device has not asked for audio this long: the stream is dead.
const STALLED_MS: u64 = 2000;

impl Shared {
    fn now_ms(&self) -> u64 {
        self.epoch.elapsed().as_millis() as u64
    }

    fn touch(&self) {
        self.last_fill.store(self.now_ms(), Ordering::Relaxed);
    }

    fn stalled(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
            && self.now_ms() > self.last_fill.load(Ordering::Relaxed) + STALLED_MS
    }

    fn set_position(&self, frame: f64) {
        self.position.store(frame.to_bits(), Ordering::Relaxed);
    }
}

enum Cmd {
    Load(PathBuf),
    /// Play from memory instead of a file.
    LoadMemory {
        source: decode::MemorySource,
        channels: u16,
        sample_rate: u32,
    },
    Unload,
    /// The output was reopened on a device with another layout.
    Output {
        channels: usize,
        rate: u32,
    },
    Seek(u64),
    /// Play only `[start, end)`; `None` end means to the end of the file.
    Range {
        start: u64,
        end: Option<u64>,
        looping: bool,
    },
}

/// Range of the listening volume ([`Player::set_monitor_db`]).
pub const MONITOR_MIN_DB: f32 = -24.0;
pub const MONITOR_MAX_DB: f32 = 24.0;

pub struct Player {
    shared: Arc<Shared>,
    cmd: Sender<Cmd>,
    /// The file being played, if any.
    current: Mutex<Option<PathBuf>>,
}

impl Player {
    pub fn new() -> Result<Self> {
        let shared = Arc::new(Shared {
            queue: Mutex::new(VecDeque::new()),
            playing: AtomicBool::new(false),
            exhausted: AtomicBool::new(true),
            position: AtomicU64::new(0f64.to_bits()),
            gain: std::sync::atomic::AtomicU32::new(1f32.to_bits()),
            monitor: std::sync::atomic::AtomicU32::new(1f32.to_bits()),
            last_fill: AtomicU64::new(0),
            epoch: Instant::now(),
            closed: AtomicBool::new(false),
        });

        let (cmd, rx) = unbounded();
        let (ready_tx, ready) = bounded(1);
        let output_shared = shared.clone();
        let output_cmd = cmd.clone();
        thread::Builder::new()
            .name("player-output".into())
            .spawn(move || run_output(output_shared, output_cmd, ready_tx))?;
        let (out_channels, out_rate) = ready.recv().context("audio output thread ended")??;

        let decoder_shared = shared.clone();
        thread::Builder::new()
            .name("player-decoder".into())
            .spawn(move || Decoder::new(decoder_shared, out_channels, out_rate).run(rx))?;

        Ok(Player {
            shared,
            cmd,
            current: Mutex::new(None),
        })
    }

    /// A new source replaces the current one. A file on another computer may
    /// be waiting on the network: its reads give up, so the decoder gets to
    /// the next command at once.
    fn replace_source(&self, next: Option<PathBuf>) {
        let old = std::mem::replace(&mut *self.current.lock().unwrap(), next);
        if let Some(old) = old
            && crate::vfs::is_remote(&old)
        {
            crate::remote::interrupt_reads(&old);
        }
    }

    /// Also resets the gain: whoever loads decides how loud it plays.
    pub fn load(&self, path: PathBuf) {
        self.pause();
        self.replace_source(Some(path.clone()));
        self.set_gain(1.0);
        self.shared.set_position(0.0);
        self.send(Cmd::Load(path));
    }

    /// Plays `samples` (interleaved) instead of a file; positions are frames into it.
    /// Also resets the gain (rendered audio already has its own).
    pub fn load_memory(&self, samples: Arc<Vec<f32>>, channels: u16, sample_rate: u32) {
        self.pause();
        self.replace_source(None);
        self.set_gain(1.0);
        self.shared.set_position(0.0);
        self.send(Cmd::LoadMemory {
            source: decode::MemorySource::new(samples, usize::from(channels)),
            channels,
            sample_rate,
        });
    }

    pub fn unload(&self) {
        self.pause();
        self.replace_source(None);
        self.send(Cmd::Unload);
    }

    pub fn play(&self) {
        self.shared.playing.store(true, Ordering::Relaxed);
    }

    pub fn pause(&self) {
        self.shared.playing.store(false, Ordering::Relaxed);
    }

    /// Starts playing; if the previous run reached the end, starts over from `restart_at`.
    pub fn toggle(&self, restart_at: u64) {
        if self.is_playing() {
            self.pause();
            return;
        }
        if self.is_finished() {
            self.seek(restart_at);
        }
        self.play();
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    /// Everything up to the end of the range has been played.
    pub fn is_finished(&self) -> bool {
        self.shared.exhausted.load(Ordering::Relaxed)
            && self.shared.queue.lock().unwrap().is_empty()
    }

    pub fn seek(&self, frame: u64) {
        // Publish right away so a paused playhead jumps without waiting for the decoder.
        self.shared.set_position(frame as f64);
        self.shared.exhausted.store(false, Ordering::Relaxed);
        self.send(Cmd::Seek(frame));
    }

    pub fn set_range(&self, start: u64, end: Option<u64>, looping: bool) {
        self.send(Cmd::Range {
            start,
            end,
            looping,
        });
    }

    /// Volume of what plays from now on, as a factor (1.0 = as recorded).
    pub fn set_gain(&self, gain: f32) {
        self.shared
            .gain
            .store(gain.max(0.0).to_bits(), Ordering::Relaxed);
    }

    /// Listening volume in dB, on top of everything else and kept across
    /// loads. Loud enough clips, which is the point when a take is quiet.
    pub fn set_monitor_db(&self, db: f32) {
        let factor = 10f32.powf(db.clamp(MONITOR_MIN_DB, MONITOR_MAX_DB) / 20.0);
        self.shared.monitor.store(factor.to_bits(), Ordering::Relaxed);
    }

    /// Playhead in source frames.
    pub fn position(&self) -> u64 {
        f64::from_bits(self.shared.position.load(Ordering::Relaxed)) as u64
    }

    fn send(&self, cmd: Cmd) {
        // The decoder thread lives as long as the process; a send can't fail in practice.
        self.cmd.send(cmd).ok();
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.shared.closed.store(true, Ordering::Relaxed);
    }
}

/// Owns the output stream. Opens it on the default device, and again on
/// whatever is the default then when the device goes away or the stream
/// stops asking for audio while playing. The first layout goes to `ready`,
/// later changes to the decoder.
fn run_output(shared: Arc<Shared>, cmd: Sender<Cmd>, ready: Sender<Result<(usize, u32)>>) {
    let (broken_tx, broken) = unbounded::<()>();
    let mut ready = Some(ready);
    let mut layout = None;
    while !shared.closed.load(Ordering::Relaxed) {
        match open_output(&shared, broken_tx.clone()) {
            Ok((stream, channels, rate)) => {
                if let Some(ready) = ready.take() {
                    ready.send(Ok((channels, rate))).ok();
                } else if layout != Some((channels, rate)) {
                    cmd.send(Cmd::Output { channels, rate }).ok();
                }
                layout = Some((channels, rate));
                shared.touch();
                loop {
                    match broken.recv_timeout(Duration::from_millis(500)) {
                        Ok(()) => break,
                        Err(RecvTimeoutError::Timeout) => {
                            if shared.closed.load(Ordering::Relaxed) {
                                return;
                            }
                            if shared.stalled() {
                                eprintln!("audio output stalled, reopening");
                                break;
                            }
                        }
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                drop(stream);
                while broken.try_recv().is_ok() {}
            }
            Err(err) => {
                if let Some(ready) = ready.take() {
                    ready.send(Err(err)).ok();
                    return;
                }
                eprintln!("audio output: {err:#}");
            }
        }
        // Let the system settle on the new device before asking for it.
        thread::sleep(Duration::from_millis(300));
    }
}

fn open_output(shared: &Arc<Shared>, broken: Sender<()>) -> Result<(cpal::Stream, usize, u32)> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no audio output device")?;
    let supported = device.default_output_config()?;
    let format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = usize::from(config.channels);
    let rate = config.sample_rate;
    let stream = match format {
        cpal::SampleFormat::F32 => build_stream::<f32>(&device, config, shared.clone(), broken),
        cpal::SampleFormat::I16 => build_stream::<i16>(&device, config, shared.clone(), broken),
        cpal::SampleFormat::I32 => build_stream::<i32>(&device, config, shared.clone(), broken),
        cpal::SampleFormat::U16 => build_stream::<u16>(&device, config, shared.clone(), broken),
        other => anyhow::bail!("unsupported device sample format {other}"),
    }?;
    stream.play()?;
    Ok((stream, channels, rate))
}

fn build_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    shared: Arc<Shared>,
    broken: Sender<()>,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = usize::from(config.channels);
    let stream = device.build_output_stream(
        config,
        move |out: &mut [T], _| fill(out, channels, &shared),
        move |err| {
            eprintln!("audio stream: {err}");
            if matches!(
                err.kind(),
                cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::HostUnavailable
            ) {
                broken.send(()).ok();
            }
        },
        None,
    )?;
    Ok(stream)
}

fn fill<T: SizedSample + FromSample<f32>>(out: &mut [T], channels: usize, shared: &Shared) {
    let silence = T::from_sample(0.0f32);
    shared.touch();
    if !shared.playing.load(Ordering::Relaxed) {
        out.fill(silence);
        return;
    }

    let gain = f32::from_bits(shared.gain.load(Ordering::Relaxed))
        * f32::from_bits(shared.monitor.load(Ordering::Relaxed));
    let mut queue = shared.queue.lock().unwrap();
    let mut written = 0;
    while written < out.len() {
        let Some(chunk) = queue.front_mut() else {
            break;
        };
        let n = (out.len() - written).min(chunk.samples.len() - chunk.read);
        for (o, s) in out[written..written + n]
            .iter_mut()
            .zip(&chunk.samples[chunk.read..])
        {
            // Clamped: a boosted region must not wrap around in integer output.
            *o = T::from_sample((*s * gain).clamp(-1.0, 1.0));
        }
        written += n;
        chunk.read += n;
        let pos = chunk.src_start + (chunk.read / channels) as f64 * chunk.step;
        shared.set_position(pos);
        if chunk.read == chunk.samples.len() {
            queue.pop_front();
        }
    }
    let ran_out = queue.is_empty() && shared.exhausted.load(Ordering::Relaxed);
    drop(queue);

    out[written..].fill(silence);
    if ran_out {
        shared.playing.store(false, Ordering::Relaxed);
    }
}

struct Decoder {
    shared: Arc<Shared>,
    out_channels: usize,
    out_rate: u32,

    source: Option<Box<dyn FrameSource>>,
    src_channels: usize,
    src_rate: u32,
    total_frames: u64,
    step: f64,

    range_start: u64,
    range_end: Option<u64>,
    looping: bool,

    /// Next source frame to read from the file.
    cursor: u64,
    /// Source position of the next output frame (fractional when resampling).
    res_pos: f64,
    /// Last frame of the previous block, for interpolation across block edges.
    carry: Vec<f32>,
    buf: Vec<f32>,
}

impl Decoder {
    fn new(shared: Arc<Shared>, out_channels: usize, out_rate: u32) -> Self {
        Decoder {
            shared,
            out_channels,
            out_rate,
            source: None,
            src_channels: 0,
            src_rate: out_rate,
            total_frames: 0,
            step: 1.0,
            range_start: 0,
            range_end: None,
            looping: false,
            cursor: 0,
            res_pos: 0.0,
            carry: Vec::new(),
            buf: Vec::new(),
        }
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        loop {
            let full = self.queued_secs() >= QUEUE_SECS;
            let idle = self.source.is_none() || self.shared.exhausted.load(Ordering::Relaxed);
            let cmd = if full || idle {
                match rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(cmd) => Some(cmd),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => None,
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
                }
            } else {
                rx.try_recv().ok()
            };

            if let Some(cmd) = cmd {
                self.handle(cmd);
                continue;
            }
            if !full
                && !idle
                && let Err(err) = self.decode_block()
            {
                eprintln!("playback: {err:#}");
                self.shared.exhausted.store(true, Ordering::Relaxed);
            }
        }
    }

    fn queued_secs(&self) -> f64 {
        let queue = self.shared.queue.lock().unwrap();
        let samples: usize = queue.iter().map(|c| c.samples.len() - c.read).sum();
        samples as f64 / self.out_channels as f64 / f64::from(self.out_rate)
    }

    fn handle(&mut self, cmd: Cmd) {
        match cmd {
            Cmd::Load(path) => {
                self.source = None;
                self.range_start = 0;
                self.range_end = None;
                self.looping = false;
                match decode::open(&path) {
                    Ok((info, source)) => {
                        self.source = Some(source);
                        self.src_channels = usize::from(info.channels);
                        self.src_rate = info.sample_rate;
                        self.total_frames = info.total_frames;
                        self.step = f64::from(info.sample_rate) / f64::from(self.out_rate);
                        self.reposition(0);
                    }
                    Err(err) => {
                        eprintln!("playback: {err:#}");
                        self.flush();
                    }
                }
            }
            Cmd::LoadMemory {
                source,
                channels,
                sample_rate,
            } => {
                self.range_start = 0;
                self.range_end = None;
                self.looping = false;
                self.total_frames = source.frames();
                self.source = Some(Box::new(source));
                self.src_channels = usize::from(channels);
                self.src_rate = sample_rate;
                self.step = f64::from(sample_rate) / f64::from(self.out_rate);
                self.reposition(0);
            }
            Cmd::Unload => {
                self.source = None;
                self.flush();
            }
            Cmd::Output { channels, rate } => {
                // What is queued has the old layout: decode again from the playhead.
                self.out_channels = channels;
                self.out_rate = rate;
                self.step = f64::from(self.src_rate) / f64::from(rate);
                let at = f64::from_bits(self.shared.position.load(Ordering::Relaxed)) as u64;
                if self.source.is_some() {
                    let exhausted = self.shared.exhausted.load(Ordering::Relaxed);
                    self.reposition(at);
                    if exhausted {
                        self.shared.exhausted.store(true, Ordering::Relaxed);
                    }
                } else {
                    self.flush();
                }
            }
            Cmd::Seek(frame) => self.reposition(frame),
            Cmd::Range {
                start,
                end,
                looping,
            } => {
                self.range_start = start;
                self.range_end = end;
                self.looping = looping;
            }
        }
    }

    fn flush(&self) {
        self.shared.queue.lock().unwrap().clear();
        self.shared.exhausted.store(true, Ordering::Relaxed);
    }

    fn reposition(&mut self, frame: u64) {
        let frame = frame.min(self.total_frames);
        self.shared.queue.lock().unwrap().clear();
        self.shared.set_position(frame as f64);
        self.cursor = frame;
        self.res_pos = frame as f64;
        self.carry.clear();
        if let Some(source) = &mut self.source
            && let Err(err) = source.seek(frame)
        {
            eprintln!("playback seek: {err:#}");
        }
        self.shared
            .exhausted
            .store(self.source.is_none(), Ordering::Relaxed);
    }

    fn end(&self) -> u64 {
        self.range_end
            .unwrap_or(self.total_frames)
            .min(self.total_frames)
    }

    fn decode_block(&mut self) -> Result<()> {
        let end = self.end();
        let want = end.saturating_sub(self.cursor).min(DECODE_BLOCK as u64) as usize;
        let source = self.source.as_mut().expect("checked by caller");

        self.buf.clear();
        let read = if want > 0 {
            source.read(&mut self.buf, want)?
        } else {
            0
        };
        if read == 0 {
            if self.looping && end > self.range_start {
                let start = self.range_start;
                // Wrap without flushing what is queued, so the loop is seamless.
                self.cursor = start;
                self.res_pos = start as f64;
                self.carry.clear();
                self.source.as_mut().unwrap().seek(start)?;
            } else {
                self.shared.exhausted.store(true, Ordering::Relaxed);
            }
            return Ok(());
        }

        let block_start = self.cursor;
        self.cursor += read as u64;
        let chunk = self.render(block_start, read);
        if !chunk.samples.is_empty() {
            self.shared.queue.lock().unwrap().push_back(chunk);
        }
        Ok(())
    }

    /// Converts `self.buf` (source frames starting at `block_start`) to device layout and rate.
    fn render(&mut self, block_start: u64, frames: usize) -> Chunk {
        let src_ch = self.src_channels;
        let out_ch = self.out_channels;
        let src_start = self.res_pos;
        let mut samples =
            Vec::with_capacity((frames as f64 / self.step) as usize * out_ch + out_ch);

        if self.step == 1.0 {
            for frame in self.buf.chunks_exact(src_ch) {
                map_channels(frame, out_ch, &mut samples);
            }
            self.res_pos += frames as f64;
            return Chunk {
                samples,
                read: 0,
                src_start,
                step: 1.0,
            };
        }

        if self.carry.is_empty() {
            self.carry = self.buf[..src_ch].to_vec();
        }
        // Frame k (absolute) lives at index k - (block_start - 1) of [carry, buf...].
        let frame_at = |k: u64| -> &[f32] {
            if k < block_start {
                &self.carry
            } else {
                let i = (k - block_start) as usize * src_ch;
                &self.buf[i..i + src_ch]
            }
        };
        let block_end = block_start + frames as u64;
        let mut mixed = vec![0f32; src_ch];
        while self.res_pos + 1.0 < block_end as f64 {
            let i = self.res_pos.floor() as u64;
            let t = (self.res_pos - i as f64) as f32;
            let (a, b) = (frame_at(i), frame_at(i + 1));
            for c in 0..src_ch {
                mixed[c] = a[c] + (b[c] - a[c]) * t;
            }
            map_channels(&mixed, out_ch, &mut samples);
            self.res_pos += self.step;
        }
        self.carry = self.buf[(frames - 1) * src_ch..frames * src_ch].to_vec();

        Chunk {
            samples,
            read: 0,
            src_start,
            step: self.step,
        }
    }
}

/// Mono goes to every output; otherwise channel n plays on output n, extras are dropped.
fn map_channels(frame: &[f32], out_ch: usize, out: &mut Vec<f32>) {
    match (frame.len(), out_ch) {
        (1, _) => out.extend(std::iter::repeat_n(frame[0], out_ch)),
        (_, 1) => out.push(frame.iter().sum::<f32>() / frame.len() as f32),
        _ => out.extend((0..out_ch).map(|c| frame.get(c).copied().unwrap_or(0.0))),
    }
}

#[cfg(test)]
mod tests {
    use super::map_channels;

    #[test]
    fn channel_mapping() {
        let mut out = Vec::new();
        map_channels(&[0.5], 2, &mut out);
        map_channels(&[0.1, 0.2], 2, &mut out);
        map_channels(&[0.1, 0.2, 0.3, 0.4], 2, &mut out);
        assert_eq!(out, [0.5, 0.5, 0.1, 0.2, 0.1, 0.2]);
    }
}
