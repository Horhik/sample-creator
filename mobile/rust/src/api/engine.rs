//! One open recording at a time, like the desktop editor. Positions are frames.
//! Calls marked `sync` are cheap and are polled by the UI every frame.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use flutter_rust_bridge::frb;
use sample_creator_engine::audio::{self, detail::Detail, peaks, player::Player};
use sample_creator_engine::core::history::{Change, History};
use sample_creator_engine::core::{ChannelMode, Marker, PALETTE, Region, color_name};
use sample_creator_engine::recording::{self, Recording};
use sample_creator_engine::store::Library;

use crate::state::{
    Open, STATE, State, commit, edit_marker, edit_region, frame, library, player, state, with_open,
};

/// Interface language for the engine's own texts (colours, formats, errors).
#[frb(sync)]
pub fn set_language(russian: bool) {
    sample_creator_engine::lang::set_russian(russian);
}

#[frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

/// Opens the library in the app's private storage. Safe to call again.
pub fn init(data_dir: String, cache_dir: String) -> Result<()> {
    if STATE.get().is_some() {
        return Ok(());
    }
    let data_dir = PathBuf::from(data_dir);
    std::fs::create_dir_all(&data_dir)?;
    audio::set_cache_dir(PathBuf::from(cache_dir));
    let library = Library::open(&data_dir.join("library.db"))?;
    let player = Player::new()
        .inspect_err(|err| eprintln!("audio output unavailable: {err:#}"))
        .ok();
    let _ = STATE.set(State {
        library: Mutex::new(library),
        player,
        open: Mutex::new(None),
        mini_peaks: Mutex::new(HashMap::new()),
        export_cancel: Arc::new(AtomicBool::new(false)),
        processing: Mutex::new(Default::default()),
    });
    Ok(())
}

// ---- Files ----

pub struct Entry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    /// User's own name for the recording, if any.
    pub alias: Option<String>,
    pub regions: u32,
    pub markers: u32,
    pub reviewed: bool,
}

/// Folders and supported recordings in `path`, folders first, hidden ones skipped.
pub fn list_dir(path: String) -> Result<Vec<Entry>> {
    // The folder may be on another computer: read it before taking the library.
    let items = sample_creator_engine::vfs::read_dir(Path::new(&path))
        .with_context(|| format!("cannot read {path}"))?;
    let library = library()?;
    let mut entries = Vec::new();
    for item in items {
        let (name, path, is_dir) = (item.name, item.path, item.is_dir);
        if !is_dir && !audio::is_supported(&path) {
            continue;
        }
        let summary = if is_dir {
            None
        } else {
            library.summary_at(&path).ok().flatten()
        };
        let summary = summary.unwrap_or_default();
        entries.push(Entry {
            name,
            path: path.to_string_lossy().into_owned(),
            is_dir,
            alias: summary.alias,
            regions: summary.regions as u32,
            markers: summary.markers as u32,
            reviewed: summary.reviewed,
        });
    }
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

pub struct FileInfo {
    pub path: String,
    /// Alias, or the file name without extension.
    pub title: String,
    pub channels: u16,
    pub sample_rate: u32,
    pub total_frames: i64,
    pub encoding: String,
    /// `yyyy-mm-dd hh:mm:ss` of the first sample, from the recorder's `bext`.
    pub recorded_at: Option<String>,
}

pub fn open_file(path: String) -> Result<FileInfo> {
    let state = state()?;
    let path = PathBuf::from(path);
    let rec = recording::open(&*library()?, &path)?;
    let job = peaks::start(&path, usize::from(rec.info.channels), rec.info.total_frames);
    let detail = Detail::start(&path, usize::from(rec.info.channels), rec.info.total_frames);
    if let Some(p) = &state.player {
        p.load(path.clone());
    }
    let info = file_info(&rec);
    *state.open.lock().unwrap() = Some(Open {
        rec,
        history: History::default(),
        peaks: job,
        detail,
    });
    Ok(info)
}

fn file_info(rec: &Recording) -> FileInfo {
    let recorded_at = rec.info.bext.as_ref().and_then(|b| {
        if b.origination_date.is_empty() {
            return None;
        }
        // TimeReference is the true start; `origination_time` is when the file closed.
        let secs = b.time_reference / u64::from(rec.info.sample_rate.max(1));
        Some(format!(
            "{} {:02}:{:02}:{:02}",
            b.origination_date,
            secs / 3600 % 24,
            secs / 60 % 60,
            secs % 60
        ))
    });
    FileInfo {
        path: rec.path.to_string_lossy().into_owned(),
        title: rec.record.alias.clone().unwrap_or_else(|| stem(&rec.path)),
        channels: rec.info.channels,
        sample_rate: rec.info.sample_rate,
        total_frames: rec.info.total_frames as i64,
        encoding: rec.info.encoding.clone(),
        recorded_at,
    }
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

pub fn close_file() -> Result<()> {
    let state = state()?;
    if let Some(p) = &state.player {
        p.unload();
    }
    *state.open.lock().unwrap() = None;
    Ok(())
}

pub fn set_alias(alias: String) -> Result<()> {
    let alias = alias.trim();
    let alias = (!alias.is_empty()).then_some(alias);
    with_open(|open, library| {
        library.set_alias(open.rec.record.id, alias)?;
        open.rec.record.alias = alias.map(str::to_owned);
        Ok(())
    })
}

// ---- Waveform ----

pub struct PeaksView {
    /// Fraction of the file already summarised, 0..=1.
    pub done: f64,
    /// Per channel, per column: min then max. NaN where not computed yet.
    pub data: Vec<f32>,
}

/// Fraction of the open recording already summarised, 0..=1.
#[frb(sync)]
pub fn peaks_done() -> f64 {
    with_open(|open, _| {
        let total = open.rec.info.total_frames.max(1);
        Ok(open.peaks.peaks.done_frames() as f64 / total as f64)
    })
    .unwrap_or(0.0)
}

/// Min/max of `columns` equal slices of frames `[start, end)`.
#[frb(sync)]
pub fn peaks(start: f64, end: f64, columns: u32) -> Result<PeaksView> {
    with_open(|open, _| {
        let p = &open.peaks.peaks;
        let channels = usize::from(open.rec.info.channels);
        let total = open.rec.info.total_frames.max(1);
        let reader = p.read();
        let step = (end - start) / f64::from(columns.max(1));
        let mut data = Vec::with_capacity(channels * columns as usize * 2);
        for ch in 0..channels {
            for col in 0..columns {
                let a = start + step * f64::from(col);
                let (lo, hi) = reader
                    .min_max(ch, a, a + step)
                    .unwrap_or((f32::NAN, f32::NAN));
                data.push(lo);
                data.push(hi);
            }
        }
        Ok(PeaksView {
            done: p.done_frames() as f64 / total as f64,
            data,
        })
    })
}

/// Like [`peaks`], for the detailed waveform: closer than the summary goes,
/// from the samples themselves once they are read (asked for here, read in
/// the background; [`detail_generation`] changes when they arrive).
#[frb(sync)]
pub fn wave_detail(start: f64, end: f64, columns: u32) -> Result<PeaksView> {
    with_open(|open, _| {
        let p = &open.peaks.peaks;
        let channels = usize::from(open.rec.info.channels);
        let total = open.rec.info.total_frames.max(1);
        let step = (end - start) / f64::from(columns.max(1));
        let close = step < peaks::FINE as f64;
        if close {
            open.detail.want(start, end);
        }
        let reader = p.read();
        let detail = close.then(|| open.detail.read());
        let mut data = Vec::with_capacity(channels * columns as usize * 2);
        for ch in 0..channels {
            for col in 0..columns {
                let a = start + step * f64::from(col);
                let (lo, hi) = detail
                    .as_ref()
                    .and_then(|d| d.min_max(ch, a, a + step))
                    .or_else(|| reader.min_max(ch, a, a + step))
                    .unwrap_or((f32::NAN, f32::NAN));
                data.push(lo);
                data.push(hi);
            }
        }
        Ok(PeaksView {
            done: p.done_frames() as f64 / total as f64,
            data,
        })
    })
}

/// Changes when new samples for the detailed waveform are in.
#[frb(sync)]
pub fn detail_generation() -> i64 {
    with_open(|open, _| Ok(open.detail.generation() as i64)).unwrap_or(0)
}

// ---- Markup ----

pub struct MarkerView {
    pub id: i64,
    pub pos: i64,
    pub name: String,
    pub color: u8,
    pub from_recorder: bool,
}

pub struct RegionView {
    pub id: i64,
    pub start: i64,
    pub end: i64,
    pub name: String,
    pub color: u8,
    pub export: bool,
    /// Percent, 100 = normal.
    pub speed: u16,
    pub keep_pitch: bool,
    /// Tenths of a dB.
    pub gain: i16,
    pub normalize: bool,
}

pub struct MarkupView {
    pub markers: Vec<MarkerView>,
    pub regions: Vec<RegionView>,
    pub can_undo: bool,
    pub can_redo: bool,
}

#[frb(sync)]
pub fn markup() -> Result<MarkupView> {
    with_open(|open, _| {
        let m = &open.rec.markup;
        Ok(MarkupView {
            markers: m
                .markers
                .iter()
                .map(|m| MarkerView {
                    id: m.id,
                    pos: m.pos as i64,
                    name: m.name.clone(),
                    color: m.color,
                    from_recorder: m.from_recorder,
                })
                .collect(),
            regions: m
                .regions
                .iter()
                .map(|r| RegionView {
                    id: r.id,
                    start: r.start as i64,
                    end: r.end as i64,
                    name: r.name.clone(),
                    color: r.color,
                    export: r.export,
                    speed: r.speed,
                    keep_pitch: r.keep_pitch,
                    gain: r.fx.gain,
                    normalize: r.fx.normalize,
                })
                .collect(),
            can_undo: open.history.can_undo(),
            can_redo: open.history.can_redo(),
        })
    })
}

#[derive(Clone, Copy)]
pub enum ItemKind {
    Marker,
    Region,
}

pub fn add_marker(pos: i64, color: u8) -> Result<i64> {
    let pos = frame(pos);
    let file = with_open(|o, _| Ok(o.rec.record.id))?;
    commit(vec![Change::add_marker(Marker {
        id: 0,
        file,
        pos,
        name: String::new(),
        color: color % PALETTE.len() as u8,
        from_recorder: false,
    })])?
    .context("marker was not created")
}

pub fn add_region(start: i64, end: i64, color: u8) -> Result<i64> {
    let (start, end) = (frame(start), frame(end));
    let (file, rate) = with_open(|o, _| Ok((o.rec.record.id, o.rec.info.sample_rate)))?;
    let (start, end) = (start.min(end), start.max(end));
    // Same rule as the desktop: a sliver shorter than 10 ms is a mis-tap.
    if end - start < u64::from(rate) / 100 {
        bail!("region is shorter than 10 ms");
    }
    commit(vec![Change::add_region(Region {
        id: 0,
        file,
        start,
        end,
        name: String::new(),
        color: color % PALETTE.len() as u8,
        export: true,
        channels: ChannelMode::AsSource,
        speed: 100,
        keep_pitch: false,
        fx: Default::default(),
        parts: Vec::new(),
    })])?
    .context("region was not created")
}

pub fn rename(kind: ItemKind, id: i64, name: String) -> Result<()> {
    let name = name.trim().to_owned();
    match kind {
        ItemKind::Marker => edit_marker(id, |m| m.name = name),
        ItemKind::Region => edit_region(id, |r| r.name = name),
    }
}

pub fn set_color(kind: ItemKind, id: i64, color: u8) -> Result<()> {
    let color = color % PALETTE.len() as u8;
    match kind {
        ItemKind::Marker => edit_marker(id, |m| m.color = color),
        ItemKind::Region => edit_region(id, |r| r.color = color),
    }
}

pub fn move_marker(id: i64, pos: i64) -> Result<()> {
    edit_marker(id, |m| m.pos = frame(pos))
}

pub fn set_region_bounds(id: i64, start: i64, end: i64) -> Result<()> {
    let (start, end) = (frame(start), frame(end));
    let (start, end) = (start.min(end), start.max(end).max(start.min(end) + 1));
    edit_region(id, |r| {
        r.start = start;
        r.end = end;
    })
}

/// Region volume in tenths of a dB (above zero: louder than the source).
pub fn set_region_gain(id: i64, gain: i16) -> Result<()> {
    use sample_creator_engine::core::{GAIN_MAX, GAIN_MIN};
    edit_region(id, |r| r.fx.gain = gain.clamp(GAIN_MIN, GAIN_MAX))
}

pub fn set_region_export(id: i64, export: bool) -> Result<()> {
    edit_region(id, |r| r.export = export)
}

pub fn set_region_speed(id: i64, speed: u16, keep_pitch: bool) -> Result<()> {
    use sample_creator_engine::core::{SPEED_MAX, SPEED_MIN};
    edit_region(id, |r| {
        r.speed = speed.clamp(SPEED_MIN, SPEED_MAX);
        r.keep_pitch = keep_pitch;
    })
}

pub fn delete(kind: ItemKind, id: i64) -> Result<()> {
    let change = with_open(|o, _| {
        let m = &o.rec.markup;
        Ok(match kind {
            ItemKind::Marker => Change::Marker {
                before: Some(m.marker(id).cloned().context("no such marker")?),
                after: None,
            },
            ItemKind::Region => Change::Region {
                before: Some(m.region(id).cloned().context("no such region")?),
                after: None,
            },
        })
    })?;
    commit(vec![change]).map(drop)
}

pub fn undo() -> Result<bool> {
    with_open(|o, library| o.history.undo(&mut o.rec.markup, library))
}

pub fn redo() -> Result<bool> {
    with_open(|o, library| o.history.redo(&mut o.rec.markup, library))
}

pub struct PaletteColor {
    pub name: String,
    /// 0xRRGGBB
    pub rgb: u32,
}

#[frb(sync)]
pub fn palette() -> Vec<PaletteColor> {
    PALETTE
        .iter()
        .enumerate()
        .map(|(i, (_, [r, g, b]))| PaletteColor {
            name: color_name(i).to_owned(),
            rgb: u32::from(*r) << 16 | u32::from(*g) << 8 | u32::from(*b),
        })
        .collect()
}

// ---- Playback ----

#[frb(sync)]
pub fn play() {
    if let Some(p) = player() {
        p.play();
    }
}

#[frb(sync)]
pub fn pause() {
    if let Some(p) = player() {
        p.pause();
    }
}

#[frb(sync)]
pub fn seek(pos: i64) {
    if let Some(p) = player() {
        p.seek(frame(pos));
    }
}

/// Play only `[start, end)`, optionally in a loop; `end = None` plays to the end.
#[frb(sync)]
pub fn set_range(start: i64, end: Option<i64>, looping: bool) {
    if let Some(p) = player() {
        p.set_range(frame(start), end.map(frame), looping);
    }
}

/// Volume of what plays, as a factor: a louder region is heard louder.
/// Listening volume in dB (−24…+24) for everything played; export is
/// not affected. Above 0 loud parts clip.
#[frb(sync)]
pub fn set_listening_db(db: f64) {
    if let Some(p) = crate::state::player() {
        p.set_monitor_db(db as f32);
    }
}

#[frb(sync)]
pub fn set_gain(gain: f32) {
    if let Some(p) = player() {
        p.set_gain(gain);
    }
}

#[frb(sync)]
pub fn position() -> i64 {
    player().map_or(0, |p| p.position() as i64)
}

#[frb(sync)]
pub fn is_playing() -> bool {
    player().is_some_and(|p| p.is_playing() && !p.is_finished())
}

#[frb(sync)]
pub fn has_audio_output() -> bool {
    player().is_some()
}
