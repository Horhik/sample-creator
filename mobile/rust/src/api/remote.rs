//! Folders shared by Sample Creator on a computer (see
//! `sample_creator_engine::remote`). A connected share reads like a volume.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::Result;
use sample_creator_engine::{remote, vfs};

use crate::frb_generated::StreamSink;

pub struct RemoteShare {
    /// Where its files live, e.g. `/sc-remote/1a2b3c4d`.
    pub root: String,
    /// The shared folder's name on the computer.
    pub name: String,
}

/// Connects with the code from the computer's QR. Takes a few seconds.
pub fn connect_share(ticket: String) -> Result<RemoteShare> {
    let m = remote::connect(&ticket)?;
    Ok(RemoteShare {
        root: m.root.to_string_lossy().into_owned(),
        name: m.name,
    })
}

pub fn disconnect_share(root: String) {
    remote::disconnect(Path::new(&root));
}

/// Whether a file or folder can be read now (local or on a connected share).
/// Async: a remote one asks over the network.
pub fn path_exists(path: String) -> bool {
    vfs::exists(Path::new(&path))
}

#[flutter_rust_bridge::frb(sync)]
pub fn is_remote_path(path: String) -> bool {
    vfs::is_remote(Path::new(&path))
}

/// Whether a file on a computer is downloaded and opens without waiting.
pub fn has_local_copy(path: String) -> bool {
    remote::has_copy(Path::new(&path))
}

pub struct DownloadProgress {
    pub done: i64,
    pub total: i64,
    /// The copy is complete: the file can be opened.
    pub ready: bool,
    pub error: Option<String>,
}

static DOWNLOAD_CANCEL: AtomicBool = AtomicBool::new(false);

/// Downloads a file from a computer whole before it is opened, so the
/// waveform and playback read from the phone. Progress about ten times a
/// second; the last event is `ready` or carries the error.
pub fn download_remote(path: String, sink: StreamSink<DownloadProgress>) {
    DOWNLOAD_CANCEL.store(false, Ordering::Relaxed);
    std::thread::Builder::new()
        .name("download".into())
        .spawn(move || {
            let mut last: Option<Instant> = None;
            let result = remote::download(
                Path::new(&path),
                &mut |done, total| {
                    if last.is_none_or(|t| t.elapsed() > Duration::from_millis(100)) {
                        last = Some(Instant::now());
                        let _ = sink.add(DownloadProgress {
                            done: done as i64,
                            total: total as i64,
                            ready: false,
                            error: None,
                        });
                    }
                },
                &DOWNLOAD_CANCEL,
            );
            let _ = sink.add(DownloadProgress {
                done: 0,
                total: 0,
                ready: result.is_ok(),
                error: result.err().map(|e| format!("{e:#}")),
            });
        })
        .ok();
}

#[flutter_rust_bridge::frb(sync)]
pub fn cancel_download() {
    DOWNLOAD_CANCEL.store(true, Ordering::Relaxed);
}

/// Plays a recording from the start while a finger holds it in the file
/// list. A file on a computer streams, and what is heard stays as the start
/// of its copy: opening it later downloads only the rest.
#[flutter_rust_bridge::frb(sync)]
pub fn preview_file(path: String) {
    if let Some(p) = crate::state::player() {
        p.load(path.into());
        p.play();
    }
}

/// Stops the list preview and gives the player back to the open recording.
#[flutter_rust_bridge::frb(sync)]
pub fn stop_file_preview() {
    let Some(p) = crate::state::player() else {
        return;
    };
    p.pause();
    let open = crate::state::state()
        .ok()
        .and_then(|s| s.open.lock().unwrap().as_ref().map(|o| o.rec.path.clone()));
    match open {
        Some(path) => p.load(path),
        None => p.unload(),
    }
}
