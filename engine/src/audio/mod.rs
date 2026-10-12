pub mod bwf;
pub mod decode;
pub mod peaks;
pub mod player;

use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Extensions the app can open. Matching is case-insensitive: the recorder writes
/// `.wav`, but files copied through other tools often end up as `.WAV`.
pub const SUPPORTED_EXTENSIONS: &[&str] = &["wav", "bwf", "mp3", "flac"];

pub fn is_supported(path: impl AsRef<Path>) -> bool {
    path.as_ref()
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            SUPPORTED_EXTENSIONS
                .iter()
                .any(|known| ext.eq_ignore_ascii_case(known))
        })
}

/// Identity of a recording that survives renames and copying off the recorder:
/// size plus the first and last megabyte. Cheap even over a slow USB card reader.
pub fn fingerprint(path: &Path) -> io::Result<u64> {
    const SPAN: u64 = 1 << 20;
    let mut file = crate::vfs::open(path)?;
    let len = file.len()?;

    let mut hasher = xxhash_rust::xxh3::Xxh3::new();
    hasher.update(&len.to_le_bytes());
    let mut buf = vec![0u8; SPAN.min(len) as usize];
    file.read_exact(&mut buf)?;
    hasher.update(&buf);
    if len > SPAN {
        let tail = SPAN.min(len - SPAN);
        file.seek(SeekFrom::Start(len - tail))?;
        buf.resize(tail as usize, 0);
        file.read_exact(&mut buf)?;
        hasher.update(&buf);
    }
    Ok(hasher.digest())
}

static CACHE_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Overrides [`cache_dir`] where there is no XDG, e.g. the app cache on Android.
/// Only the first call counts.
pub fn set_cache_dir(dir: PathBuf) {
    let _ = CACHE_DIR.set(dir);
}

/// `$XDG_CACHE_HOME/sample-creator`, unless [`set_cache_dir`] said otherwise.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = CACHE_DIR.get() {
        return Some(dir.clone());
    }
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("sample-creator"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_extensions_case_insensitively() {
        assert!(is_supported("261008_0065.wav"));
        assert!(is_supported("/media/RECORDER/MUSIC/TAKE.WAV"));
        assert!(is_supported("take.Flac"));
        assert!(!is_supported("notes.txt"));
        assert!(!is_supported("wav"));
        assert!(!is_supported("archive.wav.zip"));
    }
}
