//! Folders shared by Sample Creator on a computer (see
//! `sample_creator_engine::remote`). A connected share reads like a volume.

use std::path::Path;

use anyhow::Result;
use sample_creator_engine::{remote, vfs};

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
