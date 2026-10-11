//! State shared by the API modules: the library, the player and the open
//! recording. Not part of the Dart API.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use anyhow::{Context, Result};
use sample_creator_engine::audio::{peaks, player::Player};
use sample_creator_engine::core::history::{Change, History, RegionHistory};
use sample_creator_engine::core::{Marker, Region};
use sample_creator_engine::recording::Recording;
use sample_creator_engine::store::Library;

pub struct State {
    pub library: Mutex<Library>,
    pub player: Option<Player>,
    pub open: Mutex<Option<Open>>,
    /// Waveform summaries for the review list, by recording path.
    pub mini_peaks: Mutex<HashMap<PathBuf, peaks::PeakJob>>,
    pub export_cancel: Arc<AtomicBool>,
    /// Undo for the processing page (speed, cuts, fades) across recordings.
    pub processing: Mutex<RegionHistory>,
}

pub struct Open {
    pub rec: Recording,
    pub history: History,
    pub peaks: peaks::PeakJob,
}

pub static STATE: OnceLock<State> = OnceLock::new();

pub fn state() -> Result<&'static State> {
    STATE.get().context("init() was not called")
}

pub fn library() -> Result<MutexGuard<'static, Library>> {
    Ok(state()?.library.lock().unwrap())
}

pub fn player() -> Option<&'static Player> {
    state().ok()?.player.as_ref()
}

/// Dart has no unsigned 64-bit int, so frames cross the bridge as `i64`.
pub fn frame(v: i64) -> u64 {
    v.max(0) as u64
}

pub fn with_open<T>(f: impl FnOnce(&mut Open, &Library) -> Result<T>) -> Result<T> {
    let state = state()?;
    let mut open = state.open.lock().unwrap();
    let open = open.as_mut().context("no recording is open")?;
    let library = state.library.lock().unwrap();
    f(open, &library)
}

/// Applies one user action; returns the id of an item it created.
pub fn commit(changes: Vec<Change>) -> Result<Option<i64>> {
    with_open(|open, library| {
        let applied = open.history.apply(&mut open.rec.markup, library, changes)?;
        Ok(applied.last().and_then(|c| match c {
            Change::Marker {
                before: None,
                after: Some(m),
            } => Some(m.id),
            Change::Region {
                before: None,
                after: Some(r),
            } => Some(r.id),
            _ => None,
        }))
    })
}

pub fn edit_marker(id: i64, f: impl FnOnce(&mut Marker)) -> Result<()> {
    let before = with_open(|o, _| o.rec.markup.marker(id).cloned().context("no such marker"))?;
    let mut after = before.clone();
    f(&mut after);
    if after != before {
        commit(vec![Change::Marker {
            before: Some(before),
            after: Some(after),
        }])?;
    }
    Ok(())
}

pub fn edit_region(id: i64, f: impl FnOnce(&mut Region)) -> Result<()> {
    let before = with_open(|o, _| o.rec.markup.region(id).cloned().context("no such region"))?;
    let mut after = before.clone();
    f(&mut after);
    if after != before {
        commit(vec![Change::Region {
            before: Some(before),
            after: Some(after),
        }])?;
    }
    Ok(())
}
