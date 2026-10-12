//! Rendered regions kept for listening: processing (splice, speed, effects)
//! runs once per set of settings, then plays straight from memory, or from
//! the cache folder after a restart. Never next to the recordings.

use std::collections::VecDeque;
use std::fs;
use std::hash::Hasher;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::core::Region;

/// Bumped when processing changes, so old renders are not reused.
const VERSION: u32 = 1;
const IN_MEMORY: usize = 6;
const ON_DISK: usize = 40;
const MAGIC: &[u8; 4] = b"SCR1";

#[derive(Debug)]
pub struct Rendered {
    pub channels: u16,
    pub sample_rate: u32,
    /// Interleaved.
    pub samples: Arc<Vec<f32>>,
}

impl Rendered {
    pub fn frames(&self) -> u64 {
        (self.samples.len() / usize::from(self.channels.max(1))) as u64
    }
}

static MEMORY: Mutex<VecDeque<(u64, Arc<Rendered>)>> = Mutex::new(VecDeque::new());

/// What decides the sound: the file as it is on disk and every setting that
/// changes the audio (not the name, colour or export choices).
fn key(path: &Path, r: &Region) -> u64 {
    let mut h = xxhash_rust::xxh3::Xxh3::new();
    h.write_u32(VERSION);
    h.write(path.to_string_lossy().as_bytes());
    if let Some(meta) = crate::vfs::metadata_now(path) {
        h.write_u64(meta.len);
        if let Some(t) = meta.modified
            && let Ok(d) = t.duration_since(std::time::UNIX_EPOCH)
        {
            h.write_u64(d.as_secs());
        }
    }
    h.write(
        format!(
            "{:?}{:?}{}{}{:?}",
            r.start..r.end,
            r.parts,
            r.speed,
            r.keep_pitch,
            r.fx
        )
        .as_bytes(),
    );
    h.digest()
}

fn dir() -> Option<PathBuf> {
    Some(crate::audio::cache_dir()?.join("renders"))
}

/// The region rendered, from the cache when possible.
pub fn rendered(path: &Path, region: &Region) -> Result<Arc<Rendered>> {
    // A file on a computer: look at it now, so the key notices a change
    // (`cached`, called while drawing, only uses what was seen last).
    if crate::vfs::is_remote(path) {
        let _ = crate::vfs::metadata(path);
    }
    let k = key(path, region);
    if let Some(r) = from_memory(k) {
        return Ok(r);
    }
    let r = match from_disk(k) {
        Some(r) => Arc::new(r),
        None => {
            let (info, samples) = super::render_to_memory(path, region)?;
            let r = Arc::new(Rendered {
                channels: info.channels,
                sample_rate: info.sample_rate,
                samples: Arc::new(samples),
            });
            if let Err(err) = to_disk(k, &r) {
                eprintln!("render cache: {err:#}");
            }
            r
        }
    };
    let mut mem = MEMORY.lock().unwrap();
    mem.retain(|(key, _)| *key != k);
    mem.push_front((k, r.clone()));
    mem.truncate(IN_MEMORY);
    Ok(r)
}

/// Already rendered and in memory: for drawing without waiting.
pub fn cached(path: &Path, region: &Region) -> Option<Arc<Rendered>> {
    from_memory(key(path, region))
}

fn from_memory(k: u64) -> Option<Arc<Rendered>> {
    let mut mem = MEMORY.lock().unwrap();
    let i = mem.iter().position(|(key, _)| *key == k)?;
    let item = mem.remove(i)?;
    let r = item.1.clone();
    mem.push_front(item);
    Some(r)
}

fn from_disk(k: u64) -> Option<Rendered> {
    let path = dir()?.join(format!("{k:016x}.f32"));
    let mut f = fs::File::open(path).ok()?;
    let mut head = [0u8; 10];
    f.read_exact(&mut head).ok()?;
    if &head[..4] != MAGIC {
        return None;
    }
    let channels = u16::from_le_bytes([head[4], head[5]]);
    let sample_rate = u32::from_le_bytes([head[6], head[7], head[8], head[9]]);
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).ok()?;
    let samples = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    Some(Rendered {
        channels,
        sample_rate,
        samples: Arc::new(samples),
    })
}

fn to_disk(k: u64, r: &Rendered) -> Result<()> {
    let Some(dir) = dir() else { return Ok(()) };
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!("{k:016x}.part"));
    let mut f = std::io::BufWriter::new(fs::File::create(&tmp)?);
    f.write_all(MAGIC)?;
    f.write_all(&r.channels.to_le_bytes())?;
    f.write_all(&r.sample_rate.to_le_bytes())?;
    for s in r.samples.iter() {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()?;
    drop(f);
    fs::rename(&tmp, dir.join(format!("{k:016x}.f32")))?;
    prune(&dir);
    Ok(())
}

/// Keeps the newest renders only.
fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    if files.len() <= ON_DISK {
        return;
    }
    files.sort();
    for (_, p) in &files[..files.len() - ON_DISK] {
        fs::remove_file(p).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::bwf::tests::recorder_like;
    use crate::core::{ChannelMode, Fx};

    #[test]
    fn renders_once_per_setting() {
        let dir = std::env::temp_dir().join(format!("sc-cache-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        crate::audio::set_cache_dir(dir.join("cache"));
        let src = dir.join("take.wav");
        fs::write(&src, recorder_like(&[[0.25, -0.25]; 4800])).unwrap();
        let mut r = Region {
            id: 1,
            file: 1,
            start: 100,
            end: 2100,
            name: "a".into(),
            color: 0,
            export: true,
            channels: ChannelMode::AsSource,
            speed: 200,
            keep_pitch: false,
            parts: Vec::new(),
            fx: Fx::default(),
        };
        let first = rendered(&src, &r).unwrap();
        assert_eq!(first.frames(), 1000);
        let again = rendered(&src, &r).unwrap();
        assert!(Arc::ptr_eq(&first, &again), "second time comes from memory");
        assert!(cached(&src, &r).is_some());
        // Renaming does not matter; a new setting does.
        r.name = "b".into();
        assert!(cached(&src, &r).is_some());
        r.fx.gain = 60;
        assert!(cached(&src, &r).is_none());
        let louder = rendered(&src, &r).unwrap();
        assert!(louder.samples[0] > first.samples[0]);
        fs::remove_dir_all(dir).ok();
    }
}
