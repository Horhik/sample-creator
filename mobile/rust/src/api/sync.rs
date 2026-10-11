//! Markup kept on the recorder itself (see `sample_creator_engine::sync`).

use std::path::Path;

use anyhow::Result;
use flutter_rust_bridge::frb;
use sample_creator_engine::core::history::History;
use sample_creator_engine::sync::{self, Report};

use crate::state::{library, state};

pub struct SyncInfo {
    /// Folder holding the state file.
    pub location: String,
    pub file: String,
    /// The volume is plugged in.
    pub available: bool,
    /// ms since the epoch.
    pub last_sync: Option<i64>,
}

pub struct SyncReport {
    pub imported: u32,
    pub exported: u32,
    /// One line per volume that failed.
    pub errors: Vec<String>,
}

#[frb(sync)]
pub fn state_file_name() -> String {
    sync::FILE_NAME.to_owned()
}

#[frb(sync)]
pub fn sync_info(root: String) -> Option<SyncInfo> {
    let t = sync::target(&*library().ok()?, Path::new(&root))?;
    Some(SyncInfo {
        location: t.location.to_string_lossy().into_owned(),
        file: t.file().to_string_lossy().into_owned(),
        available: t.available(),
        last_sync: t.last_sync,
    })
}

/// Starts keeping `root`'s markup in `location` (a folder on the volume) and
/// syncs. With `must_exist` the file has to be there already.
pub fn enable_sync(root: String, location: String, must_exist: bool) -> Result<SyncReport> {
    let report = {
        let lib = library()?;
        sync::enable(&lib, Path::new(&root), Path::new(&location), must_exist)?
    };
    after(&[report], Vec::new())
}

pub fn disable_sync(root: String) -> Result<()> {
    sync::disable(&*library()?, Path::new(&root))
}

pub fn sync_volume(root: String) -> Result<SyncReport> {
    let report = sync::sync_target(&*library()?, Path::new(&root))?;
    after(&[report], Vec::new())
}

/// Every plugged-in volume with a state file; called on mount, on leaving the
/// app and when switching recordings.
pub fn sync_all_volumes() -> Result<SyncReport> {
    let results = sync::sync_all(&*library()?);
    let mut reports = Vec::new();
    let mut errors = Vec::new();
    for (root, r) in results {
        match r {
            Ok(r) => reports.push(r),
            Err(e) => errors.push(format!("{}: {e:#}", root.display())),
        }
    }
    after(&reports, errors)
}

/// Imported markup replaced regions (new ids): undo histories are stale, and
/// the open recording must show what is now stored.
fn after(reports: &[Report], errors: Vec<String>) -> Result<SyncReport> {
    let imported: usize = reports.iter().map(|r| r.imported).sum();
    let exported: usize = reports.iter().map(|r| r.exported).sum();
    if imported > 0 {
        let state = state()?;
        *state.processing.lock().unwrap() = Default::default();
        let mut open = state.open.lock().unwrap();
        if let Some(o) = open.as_mut() {
            let lib = library()?;
            if let Ok(markup) = lib.markup(o.rec.record.id)
                && markup != o.rec.markup
            {
                o.rec.markup = markup;
                o.history = History::default();
            }
        }
    }
    Ok(SyncReport {
        imported: imported as u32,
        exported: exported as u32,
        errors,
    })
}
