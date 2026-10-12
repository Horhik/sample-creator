//! Turning planned regions into files. No GTK: [`plan`] decides names and
//! channel layout, [`render`] streams one region from its source into WAVs.

pub mod wav;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};

use crate::audio::bwf::{Bext, SampleFormat};
use crate::audio::decode::{self, FrameSource, MemorySource};
use crate::core::{ChannelMode, Region};
use crate::dsp;
use crate::lang::{is_russian, tr};
use wav::WavWriter;

pub const DEFAULT_TEMPLATE: &str = "{name}";

/// Placeholders understood in the name template, for the UI.
pub fn template_help() -> &'static str {
    tr(
        "{name} — имя региона, {source} — имя записи (псевдоним или файл), {index} — номер региона в записи, {color} — цвет",
        "{name} — region name, {source} — recording name (alias or file), {index} — region number in the recording, {color} — colour",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    /// Same encoding as the source WAV; 24-bit for compressed sources.
    #[default]
    AsSource,
    Int24,
    Float32,
}

impl OutputFormat {
    pub const ALL: [OutputFormat; 3] = [
        OutputFormat::AsSource,
        OutputFormat::Int24,
        OutputFormat::Float32,
    ];

    pub fn label(self) -> &'static str {
        match self {
            OutputFormat::AsSource => tr("как в исходнике", "same as source"),
            OutputFormat::Int24 => tr("WAV 24 бит", "WAV 24-bit"),
            OutputFormat::Float32 => tr("WAV 32 бит float", "WAV 32-bit float"),
        }
    }

    fn resolve(self, source: Option<SampleFormat>) -> SampleFormat {
        match self {
            OutputFormat::AsSource => source.unwrap_or(SampleFormat::Int(24)),
            OutputFormat::Int24 => SampleFormat::Int(24),
            OutputFormat::Float32 => SampleFormat::Float(32),
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            OutputFormat::AsSource => "source",
            OutputFormat::Int24 => "int24",
            OutputFormat::Float32 => "float32",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "int24" => OutputFormat::Int24,
            "float32" => OutputFormat::Float32,
            _ => OutputFormat::AsSource,
        }
    }
}

/// Which source channels go into one output file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    All,
    One(usize),
}

/// A region with what we know about its recording.
#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    /// Alias or file stem.
    pub title: String,
    pub channels: u16,
}

#[derive(Debug, Clone)]
pub struct Planned {
    pub source: Source,
    pub region: Region,
    /// 1-based position of the region in its recording.
    pub index: usize,
    pub outputs: Vec<(PathBuf, Pick)>,
}

impl Planned {
    /// Frames that will be written, over all outputs (after the speed change).
    pub fn frames(&self) -> u64 {
        self.region.output_len() * self.outputs.len() as u64
    }
}

/// Decides output paths. `regions` must be grouped by source and sorted by start
/// so `{index}` is stable. Names never collide with each other or with files
/// already in `dest`.
pub fn plan(regions: &[(Source, Region)], template: &str, dest: &Path) -> Vec<Planned> {
    let mut taken: HashSet<String> = HashSet::new();
    let mut index_in_source = 0;
    let mut last_source: Option<&Path> = None;
    let mut planned = Vec::new();

    for (source, region) in regions {
        if last_source != Some(source.path.as_path()) {
            index_in_source = 0;
            last_source = Some(&source.path);
        }
        index_in_source += 1;

        let base = file_name(template, source, region, index_in_source);
        let picks: Vec<(String, Pick)> = match (region.channels, source.channels) {
            (_, 1) | (ChannelMode::AsSource, _) => vec![(base, Pick::All)],
            (ChannelMode::Left, _) => vec![(base, Pick::One(0))],
            (ChannelMode::Right, _) => vec![(base, Pick::One(1))],
            (ChannelMode::SplitMono, _) => vec![
                (format!("{base}_L"), Pick::One(0)),
                (format!("{base}_R"), Pick::One(1)),
            ],
        };
        let outputs = picks
            .into_iter()
            .map(|(name, pick)| (unique_path(dest, &name, &mut taken), pick))
            .collect();
        planned.push(Planned {
            source: source.clone(),
            region: region.clone(),
            index: index_in_source,
            outputs,
        });
    }
    planned
}

fn file_name(template: &str, source: &Source, region: &Region, index: usize) -> String {
    let template = if region.name.trim().is_empty() && template.contains("{name}") {
        // An unnamed region would otherwise come out as "" or "_".
        "{source}_{index}"
    } else {
        template
    };
    let color = crate::core::color_name(usize::from(region.color));
    let name = template
        .replace("{name}", region.name.trim())
        .replace("{source}", &source.title)
        .replace("{index}", &format!("{index:02}"))
        .replace("{color}", color);
    sanitize(&name)
}

/// Safe on Linux and on FAT/exFAT cards and Windows shares.
fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    let limited: String = trimmed.chars().take(200).collect();
    if limited.is_empty() {
        "sample".into()
    } else {
        limited
    }
}

/// Names are compared case-insensitively, including against files on disk.
fn unique_path(dest: &Path, name: &str, taken: &mut HashSet<String>) -> PathBuf {
    let mut n = 1;
    loop {
        let candidate = if n == 1 {
            name.to_string()
        } else {
            format!("{name}_{n}")
        };
        let path = dest.join(format!("{candidate}.wav"));
        let key = candidate.to_lowercase();
        if path.exists() {
            // Remember it case-insensitively too: samples often end up on FAT cards.
            taken.insert(key);
        } else if taken.insert(key) {
            return path;
        }
        n += 1;
    }
}

/// Writes all outputs of one planned region. `progress` gets frames written so far
/// (summed over outputs). Returns the files written.
pub fn render(
    item: &Planned,
    format: OutputFormat,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Result<Vec<PathBuf>> {
    let (info, mut source) = decode::open(&item.source.path).with_context(|| {
        format!(
            "{} {}",
            tr("не открыть", "cannot open"),
            item.source.path.display()
        )
    })?;
    let region = &item.region;
    if region.end > info.total_frames || region.is_empty() {
        if is_russian() {
            bail!(
                "регион «{}» выходит за конец записи {}",
                region.name,
                item.source.path.display()
            );
        }
        bail!(
            "region \"{}\" runs past the end of {}",
            region.name,
            item.source.path.display()
        );
    }
    let sample_format = format.resolve(info.sample_format);
    let channels = usize::from(info.channels);
    let bext = output_bext(&item.source.path, &info, region);

    // A processed region is short: render it into memory once and write from there.
    let (mut source, offset, len): (Box<dyn FrameSource>, u64, u64) = if region.is_processed() {
        let samples = process(read_region(&mut *source, region, channels)?, &info, region);
        let mem = MemorySource::new(std::sync::Arc::new(samples), channels);
        let frames = mem.frames();
        (Box::new(mem), 0, frames)
    } else {
        (source, region.start, region.len())
    };

    let mut written = Vec::new();
    let mut done_before = 0u64;
    for (path, pick) in &item.outputs {
        let out_channels = match pick {
            Pick::All => info.channels,
            Pick::One(_) => 1,
        };
        let mut writer = WavWriter::create(
            path,
            sample_format,
            out_channels,
            info.sample_rate,
            Some(&bext),
        )?;
        source.seek(offset)?;

        let mut buf = Vec::new();
        let mut picked = Vec::new();
        let mut done = 0u64;
        while done < len {
            if cancel.load(Ordering::Relaxed) {
                writer.abort();
                bail!("{}", tr("отменено", "cancelled"));
            }
            buf.clear();
            let want = (len - done).min(65536) as usize;
            let read = source.read(&mut buf, want)?;
            if read == 0 {
                writer.abort();
                bail!(
                    "{} {}",
                    item.source.path.display(),
                    tr(
                        "— запись оборвалась раньше конца региона",
                        "ends before the region does"
                    )
                );
            }
            match pick {
                Pick::All => writer.write(&buf)?,
                Pick::One(ch) => {
                    let ch = (*ch).min(channels - 1);
                    picked.clear();
                    picked.extend(buf.iter().skip(ch).step_by(channels));
                    writer.write(&picked)?;
                }
            }
            done += read as u64;
            progress(done_before + done);
        }
        written.push(writer.finish()?);
        done_before += len;
    }
    Ok(written)
}

/// The region as it will sound after export (speed applied), all channels,
/// interleaved, at the source rate. Used for preview.
/// Outcome of [`render_all`].
#[derive(Debug, Default)]
pub struct Report {
    /// Files written.
    pub written: usize,
    /// One line per region that failed.
    pub errors: Vec<String>,
    pub cancelled: bool,
}

/// Recordings among `regions` that cannot be read right now, e.g. on a
/// recorder that is not plugged in.
pub fn missing_sources(regions: &[(Source, Region)]) -> Vec<PathBuf> {
    let missing: std::collections::BTreeSet<_> = regions
        .iter()
        .filter(|(s, _)| !s.path.exists())
        .map(|(s, _)| s.path.clone())
        .collect();
    missing.into_iter().collect()
}

/// Renders every planned region in turn. `progress` gets the frames written so
/// far over the whole run (see [`Planned::frames`] for the total). A failed
/// region does not stop the others.
pub fn render_all(
    planned: &[Planned],
    format: OutputFormat,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(u64),
) -> Report {
    let mut report = Report::default();
    let mut done_before = 0;
    for item in planned {
        let mut item_progress = |n: u64| progress(done_before + n);
        match render(item, format, cancel, &mut item_progress) {
            Ok(files) => report.written += files.len(),
            Err(err) => {
                if cancel.load(Ordering::Relaxed) {
                    report.cancelled = true;
                    break;
                }
                report.errors.push(format!("{}: {err:#}", item.region.name));
            }
        }
        done_before += item.frames();
    }
    report
}

pub fn render_to_memory(path: &Path, region: &Region) -> Result<(decode::AudioInfo, Vec<f32>)> {
    let (info, mut source) = decode::open(path)?;
    let channels = usize::from(info.channels);
    let samples = read_region(&mut *source, region, channels)?;
    let samples = process(samples, &info, region);
    Ok((info, samples))
}

fn read_region(source: &mut dyn FrameSource, region: &Region, channels: usize) -> Result<Vec<f32>> {
    let mut samples = Vec::with_capacity(region.len() as usize * channels);
    source.seek(region.start)?;
    let mut left = region.len();
    while left > 0 {
        let read = source.read(&mut samples, left.min(65536) as usize)?;
        if read == 0 {
            bail!(
                "{}",
                tr(
                    "запись оборвалась раньше конца региона",
                    "the recording ends before the region does"
                )
            );
        }
        left -= read as u64;
    }
    Ok(samples)
}

fn process(samples: Vec<f32>, info: &decode::AudioInfo, region: &Region) -> Vec<f32> {
    if !region.is_processed() {
        return samples;
    }
    let channels = usize::from(info.channels);
    let samples = if region.is_edited() {
        dsp::splice(&samples, channels, region.start, &region.effective_parts())
    } else {
        samples
    };
    let samples = if region.speed == crate::core::SPEED_NORMAL {
        samples
    } else {
        dsp::change_speed(
            &samples,
            channels,
            info.sample_rate,
            f64::from(region.speed) / 100.0,
            region.keep_pitch,
        )
    };
    dsp::fx::apply(samples, channels, info.sample_rate, &region.fx)
}

/// The exported file points back at where it came from.
fn output_bext(path: &Path, info: &decode::AudioInfo, region: &Region) -> Bext {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let rate = f64::from(info.sample_rate);
    let source = info.bext.clone().unwrap_or_default();
    Bext {
        description: format!(
            "{file} {:.3}-{:.3}s",
            region.start as f64 / rate,
            region.end as f64 / rate
        ),
        originator: "sample-creator".into(),
        origination_date: source.origination_date,
        origination_time: source.origination_time,
        time_reference: source.time_reference + region.start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::bwf::{self, tests::recorder_like};

    fn region(name: &str, start: u64, end: u64, channels: ChannelMode) -> Region {
        Region {
            id: 1,
            file: 1,
            start,
            end,
            name: name.into(),
            color: 3,
            export: true,
            channels,
            speed: 100,
            keep_pitch: false,
            fx: Default::default(),
            parts: Vec::new(),
        }
    }

    fn source(path: &str, title: &str, channels: u16) -> Source {
        Source {
            path: path.into(),
            title: title.into(),
            channels,
        }
    }

    #[test]
    fn names_from_template_with_collisions_and_fallbacks() {
        let dir = std::env::temp_dir().join(format!("sc-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("kick.wav"), b"").unwrap();

        let a = source("/rec/A.wav", "jam", 2);
        let b = source("/rec/B.wav", "B", 1);
        let regions = vec![
            (a.clone(), region("kick", 0, 10, ChannelMode::AsSource)),
            (a.clone(), region("Kick", 20, 30, ChannelMode::AsSource)),
            (a.clone(), region("", 40, 50, ChannelMode::SplitMono)),
            (a.clone(), region("a/b: c?", 60, 70, ChannelMode::Right)),
            (b.clone(), region("", 0, 10, ChannelMode::SplitMono)),
        ];
        let plan = plan(&regions, DEFAULT_TEMPLATE, &dir);
        let names: Vec<Vec<(String, Pick)>> = plan
            .iter()
            .map(|p| {
                p.outputs
                    .iter()
                    .map(|(path, pick)| {
                        (
                            path.file_name().unwrap().to_string_lossy().into_owned(),
                            *pick,
                        )
                    })
                    .collect()
            })
            .collect();
        assert_eq!(
            names,
            vec![
                vec![("kick_2.wav".into(), Pick::All)], // kick.wav exists on disk
                vec![("Kick_3.wav".into(), Pick::All)], // case-insensitive clash
                vec![
                    ("jam_03_L.wav".into(), Pick::One(0)),
                    ("jam_03_R.wav".into(), Pick::One(1)),
                ],
                vec![("a_b_ c_.wav".into(), Pick::One(1))],
                vec![("B_01.wav".into(), Pick::All)], // mono source: nothing to split
            ]
        );
        assert_eq!(plan[4].index, 1);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn renders_region_with_channel_pick_and_bext() {
        let dir = std::env::temp_dir().join(format!("sc-render-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("take.wav");
        let frames: Vec<[f32; 2]> = (0..100).map(|i| [i as f32 / 100.0, -1.5]).collect();
        std::fs::write(&src, recorder_like(&frames)).unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let regions = vec![(
            source(src.to_str().unwrap(), "take", 2),
            region("hit", 10, 20, ChannelMode::SplitMono),
        )];
        let planned = plan(&regions, "{name}", &out);
        let mut last = 0;
        let files = render(
            &planned[0],
            OutputFormat::AsSource,
            &AtomicBool::new(false),
            &mut |n| last = n,
        )
        .unwrap();
        assert_eq!(last, 20);
        assert_eq!(files.len(), 2);

        let (info, mut left) = decode::open(&files[0]).unwrap();
        assert_eq!((info.channels, info.total_frames), (1, 10));
        assert_eq!(info.sample_format, Some(SampleFormat::Float(32)));
        let mut samples = Vec::new();
        left.read(&mut samples, 100).unwrap();
        assert_eq!(samples[0], 0.10);
        assert_eq!(samples[9], 0.19);

        // 32-bit float keeps the over; the right channel is all -1.5.
        let (_, mut right) = decode::open(&files[1]).unwrap();
        samples.clear();
        right.read(&mut samples, 100).unwrap();
        assert!(samples.iter().all(|&s| s == -1.5));

        let header = bwf::read_info(&mut std::fs::File::open(&files[0]).unwrap()).unwrap();
        let bext = header.bext.unwrap();
        assert_eq!(bext.time_reference, 3938686848 + 10);
        assert_eq!(bext.origination_date, "2026-10-08");
        assert!(!out.join("hit_L.wav.part").exists());

        // Integer output clamps.
        let planned = plan(&regions, "{name}_int", &out);
        let files = render(
            &planned[0],
            OutputFormat::Int24,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let (info, mut right) = decode::open(&files[1]).unwrap();
        assert_eq!(info.sample_format, Some(SampleFormat::Int(24)));
        samples.clear();
        right.read(&mut samples, 100).unwrap();
        assert!(samples.iter().all(|&s| (s + 1.0).abs() < 1e-6));

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn exports_cut_and_spliced_parts() {
        let dir = std::env::temp_dir().join(format!("sc-parts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("take.wav");
        // Left channel counts frames so the splice order is visible.
        let frames: Vec<[f32; 2]> = (0..4800).map(|i| [i as f32 / 8192.0, 0.0]).collect();
        std::fs::write(&src, recorder_like(&frames)).unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        let mut r = region("cut", 1000, 4000, ChannelMode::Left);
        r.split_at(2000);
        r.split_at(3000);
        r.remove_part(1); // drop 2000..3000
        r.move_part(1, true); // 3000..4000 first, then 1000..2000
        let planned = plan(
            &[(source(src.to_str().unwrap(), "take", 2), r)],
            "{name}",
            &out,
        );
        assert_eq!(planned[0].frames(), 2000);
        let files = render(
            &planned[0],
            OutputFormat::Float32,
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        let (info, mut left) = decode::open(&files[0]).unwrap();
        assert_eq!(info.total_frames, 2000);
        let mut samples = Vec::new();
        left.read(&mut samples, 2000).unwrap();
        let frame = |v: f32| (v * 8192.0).round() as u64;
        assert_eq!(frame(samples[0]), 3000);
        assert_eq!(frame(samples[999]), 3999);
        assert_eq!(frame(samples[1000]), 1000);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn exports_with_speed_change() {
        let dir = std::env::temp_dir().join(format!("sc-speed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("take.wav");
        std::fs::write(&src, recorder_like(&[[0.25, -0.25]; 4800])).unwrap();
        let out = dir.join("out");
        std::fs::create_dir_all(&out).unwrap();

        for (keep_pitch, name) in [(false, "fast"), (true, "stretched")] {
            let mut r = region(name, 1000, 4000, ChannelMode::Left);
            r.speed = 200;
            r.keep_pitch = keep_pitch;
            let planned = plan(
                &[(source(src.to_str().unwrap(), "take", 2), r)],
                "{name}",
                &out,
            );
            assert_eq!(planned[0].frames(), 1500);
            let mut last = 0;
            let files = render(
                &planned[0],
                OutputFormat::AsSource,
                &AtomicBool::new(false),
                &mut |n| last = n,
            )
            .unwrap();
            assert_eq!(last, 1500);
            let (info, mut left) = decode::open(&files[0]).unwrap();
            assert_eq!((info.channels, info.total_frames), (1, 1500));
            let mut samples = Vec::new();
            left.read(&mut samples, 2000).unwrap();
            // DC stays DC: the middle is untouched by edge effects.
            assert!(
                (samples[750] - 0.25).abs() < 1e-3,
                "{name}: {}",
                samples[750]
            );
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn cancel_leaves_no_files() {
        let dir = std::env::temp_dir().join(format!("sc-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("take.wav");
        std::fs::write(&src, recorder_like(&[[0.0, 0.0]; 50])).unwrap();
        let regions = vec![(
            source(src.to_str().unwrap(), "take", 2),
            region("x", 0, 50, ChannelMode::AsSource),
        )];
        let planned = plan(&regions, "{name}", &dir);
        assert!(
            render(
                &planned[0],
                OutputFormat::AsSource,
                &AtomicBool::new(true),
                &mut |_| {}
            )
            .is_err()
        );
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["take.wav"]);
        std::fs::remove_dir_all(dir).ok();
    }
}
