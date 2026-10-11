//! Everything across recordings: the region review, previews and export.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use flutter_rust_bridge::frb;
use sample_creator_engine::audio::peaks;
use sample_creator_engine::core::history::Store;
use sample_creator_engine::core::parts::{output_to_source, positions, spliced_len};
use sample_creator_engine::core::{ChannelMode, Region};
use sample_creator_engine::export::{self, OutputFormat, Source};
use sample_creator_engine::store::FileRow;

use super::engine::PeaksView;
use crate::frb_generated::StreamSink;
use crate::state::{edit_region, frame, library, player, state};

// ---- Settings ----

/// A remembered value: last folder, export folder, name template…
#[frb(sync)]
pub fn get_setting(key: String) -> Option<String> {
    library().ok()?.setting(&key)
}

pub fn set_setting(key: String, value: String) -> Result<()> {
    library()?.set_setting(&key, &value)
}

// ---- Review ----

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Channels {
    AsSource,
    Left,
    Right,
    SplitMono,
}

impl From<ChannelMode> for Channels {
    fn from(m: ChannelMode) -> Self {
        match m {
            ChannelMode::AsSource => Channels::AsSource,
            ChannelMode::Left => Channels::Left,
            ChannelMode::Right => Channels::Right,
            ChannelMode::SplitMono => Channels::SplitMono,
        }
    }
}

impl From<Channels> for ChannelMode {
    fn from(m: Channels) -> Self {
        match m {
            Channels::AsSource => ChannelMode::AsSource,
            Channels::Left => ChannelMode::Left,
            Channels::Right => ChannelMode::Right,
            Channels::SplitMono => ChannelMode::SplitMono,
        }
    }
}

pub struct ReviewRegion {
    pub id: i64,
    pub file_id: i64,
    pub path: String,
    /// Alias or file stem.
    pub file_title: String,
    pub sample_rate: u32,
    pub file_channels: u16,
    pub total_frames: i64,
    /// The recording can be read right now (the recorder may be unplugged).
    pub available: bool,
    pub start: i64,
    pub end: i64,
    pub name: String,
    pub color: u8,
    pub export: bool,
    pub channels: Channels,
    pub speed: u16,
    pub keep_pitch: bool,
    /// Parts in splice order (the whole region when not cut).
    pub parts: Vec<PartView>,
    /// Cut, rearranged or faded.
    pub edited: bool,
    /// Frames after splicing and the speed change.
    pub output_frames: i64,
    /// Frames after splicing, before the speed change: the output timeline.
    pub spliced_frames: i64,
}

pub struct PartView {
    /// Source frames.
    pub start: i64,
    pub end: i64,
    pub fade_in: i64,
    pub fade_out: i64,
    /// Over the previous part; negative is a pause.
    pub overlap: i64,
    /// Where it starts on the output timeline (before the speed change).
    pub out_start: i64,
}

/// Every region in the library, grouped by recording and sorted by start.
pub fn all_regions() -> Result<Vec<ReviewRegion>> {
    let rows = library()?.all_regions()?;
    Ok(rows
        .into_iter()
        .map(|(f, r)| ReviewRegion {
            parts: {
                let parts = r.effective_parts();
                parts
                    .iter()
                    .zip(positions(&parts))
                    .map(|(p, at)| PartView {
                        start: p.start as i64,
                        end: p.end as i64,
                        fade_in: p.fade_in as i64,
                        fade_out: p.fade_out as i64,
                        overlap: p.overlap,
                        out_start: at as i64,
                    })
                    .collect()
            },
            spliced_frames: spliced_len(&r.effective_parts()) as i64,
            edited: r.is_edited(),
            output_frames: r.output_len() as i64,
            id: r.id,
            file_id: f.id,
            path: f.path.to_string_lossy().into_owned(),
            file_title: f.title(),
            sample_rate: f.sample_rate,
            file_channels: f.channels,
            total_frames: f.total_frames as i64,
            available: f.path.exists(),
            start: r.start as i64,
            end: r.end as i64,
            name: r.name,
            color: r.color,
            export: r.export,
            channels: r.channels.into(),
            speed: r.speed,
            keep_pitch: r.keep_pitch,
        })
        .collect())
}

fn find_region(id: i64) -> Result<(FileRow, Region)> {
    library()?
        .all_regions()?
        .into_iter()
        .find(|(_, r)| r.id == id)
        .context("no such region")
}

/// Edits a region from any recording. The open one goes through its undo
/// history; others are saved directly, like on the desktop review page.
fn update_region(id: i64, f: impl FnOnce(&mut Region)) -> Result<()> {
    let open_file = state()?
        .open
        .lock()
        .unwrap()
        .as_ref()
        .map(|o| o.rec.record.id);
    let (file, mut region) = find_region(id)?;
    if open_file == Some(file.id) {
        return edit_region(id, f);
    }
    f(&mut region);
    library()?.save_region(&region)?;
    Ok(())
}

/// A processing edit (speed, cuts, fades): saved through the processing undo
/// history. The open recording's markup is reloaded so the editor sees it.
fn process_region(id: i64, f: impl FnOnce(&mut Region)) -> Result<()> {
    let (file, before) = find_region(id)?;
    let mut after = before.clone();
    f(&mut after);
    let state = state()?;
    {
        let library = library()?;
        state
            .processing
            .lock()
            .unwrap()
            .apply(&*library, before, after)?;
    }
    reload_open(file.id)
}

fn reload_open(file: i64) -> Result<()> {
    let state = state()?;
    let mut open = state.open.lock().unwrap();
    if let Some(o) = open.as_mut().filter(|o| o.rec.record.id == file) {
        o.rec.markup = library()?.markup(file)?;
    }
    Ok(())
}

/// Undoes the last processing edit; returns the region it touched.
pub fn processing_undo() -> Result<Option<i64>> {
    let state = state()?;
    let region = {
        let library = library()?;
        state.processing.lock().unwrap().undo(&*library)?
    };
    if let Some(r) = &region {
        reload_open(r.file)?;
    }
    Ok(region.map(|r| r.id))
}

pub fn processing_redo() -> Result<Option<i64>> {
    let state = state()?;
    let region = {
        let library = library()?;
        state.processing.lock().unwrap().redo(&*library)?
    };
    if let Some(r) = &region {
        reload_open(r.file)?;
    }
    Ok(region.map(|r| r.id))
}

#[frb(sync)]
pub fn processing_can_undo() -> bool {
    state().is_ok_and(|s| s.processing.lock().unwrap().can_undo())
}

#[frb(sync)]
pub fn processing_can_redo() -> bool {
    state().is_ok_and(|s| s.processing.lock().unwrap().can_redo())
}

pub fn review_set_export(id: i64, export: bool) -> Result<()> {
    update_region(id, |r| r.export = export)
}

pub fn review_set_channels(id: i64, channels: Channels) -> Result<()> {
    update_region(id, |r| r.channels = channels.into())
}

pub fn review_set_speed(id: i64, speed: u16, keep_pitch: bool) -> Result<()> {
    use sample_creator_engine::core::{SPEED_MAX, SPEED_MIN};
    process_region(id, |r| {
        r.speed = speed.clamp(SPEED_MIN, SPEED_MAX);
        r.keep_pitch = keep_pitch;
    })
}

// ---- Cutting and splicing ----

/// Cuts the part under `at` in two. Returns false when `at` is on a part edge
/// or in a left-out piece.
pub fn split_region(id: i64, at: i64) -> Result<bool> {
    let mut done = false;
    process_region(id, |r| done = r.split_at(frame(at)))?;
    Ok(done)
}

/// Cuts the part heard at output frame `t` (the later one where parts overlap).
pub fn split_at_output(id: i64, t: i64) -> Result<bool> {
    let mut done = false;
    process_region(id, |r| {
        if let Some((i, at)) = output_to_source(&r.effective_parts(), frame(t)) {
            done = r.split_part(i, at);
        }
    })?;
    Ok(done)
}

/// Slides part `index` over the previous one by `overlap` frames (negative:
/// a pause between them).
pub fn set_part_overlap(id: i64, index: u32, overlap: i64) -> Result<()> {
    process_region(id, |r| {
        r.set_overlap(index as usize, overlap);
    })
}

pub fn remove_part(id: i64, index: u32) -> Result<()> {
    process_region(id, |r| {
        r.remove_part(index as usize);
    })
}

pub fn move_part(id: i64, index: u32, up: bool) -> Result<()> {
    process_region(id, |r| {
        r.move_part(index as usize, up);
    })
}

/// Fades in milliseconds.
pub fn set_part_fades(id: i64, index: u32, fade_in_ms: u32, fade_out_ms: u32) -> Result<()> {
    let (file, _) = find_region(id)?;
    let rate = u64::from(file.sample_rate);
    let frames = |ms: u32| u64::from(ms) * rate / 1000;
    process_region(id, |r| {
        r.set_fades(index as usize, frames(fade_in_ms), frames(fade_out_ms));
    })
}

/// Moves a part's edges (frames of the recording), e.g. dragged handles.
pub fn set_part_bounds(id: i64, index: u32, start: i64, end: i64) -> Result<()> {
    process_region(id, |r| {
        r.set_part_bounds(index as usize, frame(start), frame(end));
    })
}

pub fn reset_parts(id: i64) -> Result<()> {
    process_region(id, Region::reset_parts)
}

pub fn review_rename(id: i64, name: String) -> Result<()> {
    let name = name.trim().to_owned();
    update_region(id, |r| r.name = name)
}

/// Mini waveform of `[start, end)` in `path`. Summaries start on first use
/// and stay until [`review_close`].
#[frb(sync)]
pub fn mini_peaks(
    path: String,
    channels: u16,
    total_frames: i64,
    start: i64,
    end: i64,
    columns: u32,
) -> PeaksView {
    let path = PathBuf::from(path);
    let Ok(state) = state() else {
        return PeaksView {
            done: 0.0,
            data: Vec::new(),
        };
    };
    let mut jobs = state.mini_peaks.lock().unwrap();
    let job = jobs
        .entry(path.clone())
        .or_insert_with(|| peaks::start(&path, usize::from(channels), frame(total_frames)));
    let p = &job.peaks;
    let reader = p.read();
    let (start, end) = (start as f64, end as f64);
    let step = (end - start) / f64::from(columns.max(1));
    let mut data = Vec::with_capacity(usize::from(channels) * columns as usize * 2);
    for ch in 0..usize::from(channels) {
        for col in 0..columns {
            let a = start + step * f64::from(col);
            let (lo, hi) = reader
                .min_max(ch, a, a + step)
                .unwrap_or((f32::NAN, f32::NAN));
            data.push(lo);
            data.push(hi);
        }
    }
    PeaksView {
        done: p.done_frames() as f64 / frame(total_frames).max(1) as f64,
        data,
    }
}

/// Leaving the review: stop summarising recordings nobody looks at.
pub fn review_close() -> Result<()> {
    state()?.mini_peaks.lock().unwrap().clear();
    Ok(())
}

// ---- Previews ----

/// Plays `[start, end)` of any recording. Call `reattach` afterwards to give
/// the player back to the open recording.
#[frb(sync)]
pub fn preview_source(path: String, start: i64, end: i64) {
    if let Some(p) = player() {
        p.load(PathBuf::from(path));
        p.set_range(frame(start), Some(frame(end)), false);
        p.seek(frame(start));
        p.play();
    }
}

/// Renders the region (splice, then speed) and plays it from memory, from
/// output-timeline frame `from` (before the speed change). The player's
/// position is then in rendered frames: multiply by speed / 100 to get back
/// to the timeline.
pub fn preview_processed(id: i64, from: i64) -> Result<()> {
    let (file, region) = find_region(id)?;
    let (info, samples) = export::render_to_memory(&file.path, &region)?;
    if let Some(p) = player() {
        p.load_memory(Arc::new(samples), info.channels, info.sample_rate);
        let at = frame(from) * 100 / u64::from(region.speed.max(1));
        p.set_range(at, None, false);
        p.seek(at);
        p.play();
    }
    Ok(())
}

#[frb(sync)]
pub fn stop_preview() {
    if let Some(p) = player() {
        p.pause();
    }
}

/// Loads the open recording back into the player after a preview.
#[frb(sync)]
pub fn reattach() {
    let Ok(state) = state() else { return };
    let open = state.open.lock().unwrap();
    if let (Some(o), Some(p)) = (open.as_ref(), player()) {
        p.load(o.rec.path.clone());
    }
}

// ---- Export ----

#[frb(sync)]
pub fn template_help() -> String {
    export::template_help().to_owned()
}

#[frb(sync)]
pub fn default_template() -> String {
    export::DEFAULT_TEMPLATE.to_owned()
}

pub struct FormatOption {
    pub key: String,
    pub label: String,
}

#[frb(sync)]
pub fn output_formats() -> Vec<FormatOption> {
    OutputFormat::ALL
        .iter()
        .map(|f| FormatOption {
            key: f.key().to_owned(),
            label: f.label().to_owned(),
        })
        .collect()
}

pub struct ExportProgress {
    pub done: i64,
    pub total: i64,
    pub finished: bool,
    pub written: u32,
    pub errors: Vec<String>,
    pub cancelled: bool,
}

fn export_list(ids: &[i64]) -> Result<Vec<(Source, Region)>> {
    Ok(library()?
        .all_regions()?
        .into_iter()
        .filter(|(_, r)| ids.contains(&r.id))
        .map(|(f, r)| {
            let source = Source {
                path: f.path.clone(),
                title: f.title(),
                channels: f.channels,
            };
            (source, r)
        })
        .collect())
}

/// Recordings among the regions that cannot be read right now.
pub fn export_missing(ids: Vec<i64>) -> Result<Vec<String>> {
    Ok(export::missing_sources(&export_list(&ids)?)
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

/// Writes the regions into `dest`, reporting progress and then the result.
pub fn export_regions(
    ids: Vec<i64>,
    dest: String,
    template: String,
    format: String,
    sink: StreamSink<ExportProgress>,
) -> Result<()> {
    let state = state()?;
    let dest = PathBuf::from(dest);
    std::fs::create_dir_all(&dest).with_context(|| {
        format!(
            "{} {}",
            sample_creator_engine::lang::tr("не создать", "cannot create"),
            dest.display()
        )
    })?;
    let regions = export_list(&ids)?;
    let planned = export::plan(&regions, &template, &dest);
    let total = planned.iter().map(|p| p.frames()).sum::<u64>() as i64;
    let format = OutputFormat::from_key(&format);
    let cancel = state.export_cancel.clone();
    cancel.store(false, Ordering::Relaxed);
    library()?.set_setting("export.dir", &dest.to_string_lossy())?;

    std::thread::Builder::new()
        .name("export".into())
        .spawn(move || {
            let mut last = Instant::now();
            let report = export::render_all(&planned, format, &cancel, &mut |done| {
                if last.elapsed() > Duration::from_millis(100) {
                    let _ = sink.add(ExportProgress {
                        done: done as i64,
                        total,
                        finished: false,
                        written: 0,
                        errors: Vec::new(),
                        cancelled: false,
                    });
                    last = Instant::now();
                }
            });
            let _ = sink.add(ExportProgress {
                done: total,
                total,
                finished: true,
                written: report.written as u32,
                errors: report.errors,
                cancelled: report.cancelled,
            });
        })?;
    Ok(())
}

#[frb(sync)]
pub fn cancel_export() {
    if let Ok(s) = state() {
        s.export_cancel.store(true, Ordering::Relaxed);
    }
}

/// Where exported files would land, for a quick look before exporting.
pub fn export_preview(ids: Vec<i64>, dest: String, template: String) -> Result<Vec<String>> {
    let regions = export_list(&ids)?;
    let planned = export::plan(&regions, &template, Path::new(&dest));
    Ok(planned
        .iter()
        .flat_map(|p| p.outputs.iter())
        .map(|(path, _)| {
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
        .collect())
}
