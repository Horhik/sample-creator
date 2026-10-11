//! Opening a recording: register it in the library and load its markup.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::audio::decode::{self, AudioInfo};
use crate::core::history::{Change, History};
use crate::core::{Marker, Markup, RECORDER_MARK_COLOR};
use crate::store::{FileRecord, Library};

pub struct Recording {
    pub path: PathBuf,
    pub info: AudioInfo,
    pub record: FileRecord,
    pub markup: Markup,
}

/// Finds the recording in the library (adding it on first sight) and returns
/// its markup. Recorder marks become ordinary markers once; after that they
/// are the user's to edit.
pub fn open(library: &Library, path: &Path) -> Result<Recording> {
    let info = decode::probe(path)?;
    let fingerprint = crate::audio::fingerprint(path)?;
    let mut record = library.file(
        fingerprint,
        path,
        info.sample_rate,
        info.channels,
        info.total_frames,
    )?;
    let mut markup = library.markup(record.id)?;

    if !record.cues_imported {
        // Derived from the file, not a user edit: keep the edit time as it was,
        // so a never-touched recording never wins a sync.
        let edited = library.updated_at(record.id)?;
        let imported = info
            .cues
            .iter()
            .map(|c| {
                Change::add_marker(Marker {
                    id: 0,
                    file: record.id,
                    pos: c.frame,
                    name: c.label.clone().unwrap_or_default(),
                    color: RECORDER_MARK_COLOR,
                    from_recorder: true,
                })
            })
            .collect();
        History::default().apply(&mut markup, library, imported)?;
        library.set_cues_imported(record.id)?;
        library.set_updated_at(record.id, edited)?;
        record.cues_imported = true;
    }

    Ok(Recording {
        path: path.to_path_buf(),
        info,
        record,
        markup,
    })
}
