//! Where recordings are read from: the local disk, or a folder shared by
//! another Sample Creator over iroh (see [`crate::remote`]). Remote paths look
//! like `/sc-remote/<id>/MUSIC/take.wav`, so the rest of the engine keeps
//! working with paths.

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::remote::{self, RemoteFile};

/// The folder all remote shares hang off.
pub const REMOTE_ROOT: &str = "/sc-remote";

/// A remote share's id and the path inside it, if `path` is remote.
pub fn split_remote(path: &Path) -> Option<(&str, String)> {
    let rest = path.strip_prefix(REMOTE_ROOT).ok()?;
    let mut parts = rest.components();
    let id = parts.next()?.as_os_str().to_str()?;
    let inner = parts
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/");
    Some((id, inner))
}

pub fn is_remote(path: &Path) -> bool {
    path.starts_with(REMOTE_ROOT)
}

/// An open recording: a local file or a remote one read in blocks.
pub enum VFile {
    Local(std::fs::File),
    Remote(RemoteFile),
}

pub fn open(path: &Path) -> io::Result<VFile> {
    match split_remote(path) {
        Some((id, inner)) => remote::open(id, &inner),
        None => Ok(VFile::Local(std::fs::File::open(path)?)),
    }
}

pub struct Meta {
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub is_dir: bool,
}

pub fn metadata(path: &Path) -> io::Result<Meta> {
    match split_remote(path) {
        Some((id, inner)) => remote::metadata(id, &inner),
        None => {
            let m = std::fs::metadata(path)?;
            Ok(Meta {
                len: m.len(),
                modified: m.modified().ok(),
                is_dir: m.is_dir(),
            })
        }
    }
}

pub fn exists(path: &Path) -> bool {
    metadata(path).is_ok()
}

pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Visible entries of a folder (hidden ones skipped).
pub fn read_dir(path: &Path) -> io::Result<Vec<Entry>> {
    if let Some((id, inner)) = split_remote(path) {
        return remote::list(id, &inner).map(|entries| {
            entries
                .into_iter()
                .map(|(name, is_dir)| Entry {
                    path: path.join(&name),
                    name,
                    is_dir,
                })
                .collect()
        });
    }
    let mut out = Vec::new();
    for item in std::fs::read_dir(path)? {
        let item = item?;
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = item.path();
        out.push(Entry {
            is_dir: path.is_dir(),
            path,
            name,
        });
    }
    Ok(out)
}

impl VFile {
    pub fn len(&self) -> io::Result<u64> {
        match self {
            VFile::Local(f) => Ok(f.metadata()?.len()),
            VFile::Remote(f) => Ok(f.len()),
        }
    }

    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }
}

impl Read for VFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            VFile::Local(f) => f.read(buf),
            VFile::Remote(f) => f.read(buf),
        }
    }
}

impl Seek for VFile {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        match self {
            VFile::Local(f) => f.seek(pos),
            VFile::Remote(f) => f.seek(pos),
        }
    }
}

impl symphonia::core::io::MediaSource for VFile {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.len().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_paths() {
        let p = Path::new("/sc-remote/ab12/MUSIC/take.wav");
        assert_eq!(split_remote(p), Some(("ab12", "MUSIC/take.wav".into())));
        assert_eq!(split_remote(Path::new("/sc-remote/ab12")), Some(("ab12", String::new())));
        assert_eq!(split_remote(Path::new("/home/me/take.wav")), None);
    }
}
