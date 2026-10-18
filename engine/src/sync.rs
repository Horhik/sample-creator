//! Markup kept on the recorder itself, so it travels with the recordings
//! between devices. Only by the user's explicit choice: one small JSON file
//! per volume (the rule "nothing next to the recordings" has this one opt-in
//! exception).
//!
//! The file lists the recordings of one volume by fingerprint, with paths
//! relative to the volume root (found by name if the roots differ), so a phone (`/storage/XXXX-XXXX`) and a
//! computer (`/run/media/…`) share it. Syncing is per recording, newest edit
//! wins in both directions; deletions travel because the whole markup of a
//! recording is replaced.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::core::{ChannelMode, Fx, Marker, Markup, Part, Region};
use crate::lang::tr;
use crate::store::{FileFull, Library, now_ms};

pub const FILE_NAME: &str = "sample-creator.json";
const FORMAT: &str = "sample-creator-markup";
const VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct StateFile {
    pub format: String,
    pub version: u32,
    pub written_at: i64,
    pub recordings: Vec<RecordingState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordingState {
    /// xxh3 fingerprint as 16 hex digits (JSON numbers lose 64-bit precision).
    pub fingerprint: String,
    /// Relative to the volume root, `/`-separated.
    pub path: String,
    pub alias: Option<String>,
    pub reviewed: bool,
    pub cues_imported: bool,
    pub sample_rate: u32,
    pub channels: u16,
    pub total_frames: u64,
    pub updated_at: i64,
    pub markers: Vec<MarkerState>,
    pub regions: Vec<RegionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkerState {
    pub pos: u64,
    pub name: String,
    pub color: u8,
    pub from_recorder: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionState {
    pub start: u64,
    pub end: u64,
    pub name: String,
    pub color: u8,
    pub export: bool,
    pub channels: String,
    pub speed: u16,
    pub keep_pitch: bool,
    #[serde(default)]
    pub parts: Vec<PartState>,
    #[serde(default)]
    pub fx: Fx,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartState {
    pub start: u64,
    pub end: u64,
    pub fade_in: u64,
    pub fade_out: u64,
    pub overlap: i64,
}

/// What a sync did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// Recordings taken from the file into the app.
    pub imported: usize,
    /// Recordings written from the app to the file.
    pub exported: usize,
    /// Recordings the app knew under a path that is not there, found again.
    pub relinked: usize,
}

const TARGET: &str = "sync.target:";
const LAST: &str = "sync.last:";

/// A volume (or any folder) whose markup is kept in a state file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub root: PathBuf,
    /// Folder holding [`FILE_NAME`].
    pub location: PathBuf,
    /// Last successful sync, ms since the epoch.
    pub last_sync: Option<i64>,
}

impl Target {
    pub fn file(&self) -> PathBuf {
        file_in(&self.location)
    }

    /// The volume is plugged in.
    pub fn available(&self) -> bool {
        self.root.is_dir() && self.location.is_dir()
    }
}

pub fn targets(library: &Library) -> Result<Vec<Target>> {
    Ok(library
        .settings_with_prefix(TARGET)?
        .into_iter()
        .map(|(root, location)| Target {
            last_sync: library
                .setting(&format!("{LAST}{root}"))
                .and_then(|v| v.parse().ok()),
            root: PathBuf::from(root),
            location: PathBuf::from(location),
        })
        .collect())
}

pub fn target(library: &Library, root: &Path) -> Option<Target> {
    targets(library).ok()?.into_iter().find(|t| t.root == root)
}

/// Keeps `root`'s markup in the state file in `location` from now on, and
/// syncs right away. With `must_exist`, refuses when there is no file there
/// (the user said where an existing one is).
pub fn enable(library: &Library, root: &Path, location: &Path, must_exist: bool) -> Result<Report> {
    if !location.starts_with(root) {
        bail!(
            "{}",
            tr(
                "файл разметки должен лежать на этом же носителе",
                "the markup file must be on the same volume"
            )
        );
    }
    let path = file_in(location);
    if must_exist && !path.exists() {
        bail!(
            "{}: {} {FILE_NAME}",
            location.display(),
            tr("нет файла", "no file")
        );
    }
    if path.exists() {
        read(&path)?;
    }
    library.set_setting(
        &format!("{TARGET}{}", root.display()),
        &location.to_string_lossy(),
    )?;
    sync_target(library, root)
}

pub fn disable(library: &Library, root: &Path) -> Result<()> {
    library.delete_setting(&format!("{TARGET}{}", root.display()))?;
    library.delete_setting(&format!("{LAST}{}", root.display()))?;
    Ok(())
}

pub fn sync_target(library: &Library, root: &Path) -> Result<Report> {
    let t = target(library, root).context(tr(
        "для этого носителя нет файла разметки",
        "this volume has no markup file",
    ))?;
    if !t.available() {
        bail!(
            "{}: {}",
            root.display(),
            tr("носитель не подключён", "volume not connected")
        );
    }
    let report = sync(library, &t.root, &t.file())?;
    library.set_setting(&format!("{LAST}{}", root.display()), &now_ms().to_string())?;
    Ok(report)
}

/// Syncs every plugged-in volume that has a state file. Unplugged ones wait.
pub fn sync_all(library: &Library) -> Vec<(PathBuf, Result<Report>)> {
    targets(library)
        .unwrap_or_default()
        .into_iter()
        .filter(Target::available)
        .map(|t| {
            let r = sync_target(library, &t.root);
            (t.root, r)
        })
        .collect()
}

/// Where the state file of a volume lives: `location` is a folder on it.
pub fn file_in(location: &Path) -> PathBuf {
    location.join(FILE_NAME)
}

/// Two-way sync of the recordings under `root` with the state file at `path`.
/// A missing file is created (with everything the app knows about `root`).
pub fn sync(library: &Library, root: &Path, path: &Path) -> Result<Report> {
    let mut report = Report::default();
    let from_file = if path.exists() {
        read(path)?.recordings
    } else {
        Vec::new()
    };
    let local = library.files_under(root)?;
    let by_fp: HashMap<u64, &FileFull> = local.iter().map(|f| (f.fingerprint, f)).collect();

    // File → app, where the file is newer.
    let mut kept: HashMap<u64, RecordingState> = HashMap::new();
    let mut places = Places::new(root, path.parent().unwrap_or(root));
    for rec in from_file {
        let Ok(fp) = u64::from_str_radix(&rec.fingerprint, 16) else {
            continue;
        };
        let local = by_fp.get(&fp);
        let newer = local.is_none_or(|f| rec.updated_at > f.updated_at);
        if let Some(f) = local.filter(|f| !newer && !f.path.is_file()) {
            // Imported earlier to a path that is not there (the other
            // device had another root): put it where the recording is.
            let found = places.find(&rec.path);
            if found.is_file() && found != f.path {
                library.set_path(f.id, &found)?;
                report.relinked += 1;
            }
        }
        if newer {
            let (mut file, markup) = to_library(&rec, fp, root);
            file.path = places.find(&rec.path);
            library.replace_recording(&file, &markup)?;
            report.imported += 1;
        }
        kept.insert(fp, rec);
    }

    // App → file, where the app is newer (or the file lacks the recording).
    let mut changed = false;
    for f in &local {
        let file_newer = kept
            .get(&f.fingerprint)
            .is_some_and(|r| r.updated_at >= f.updated_at);
        // Never-edited recordings only clutter the file.
        if file_newer || (f.updated_at == 0 && !kept.contains_key(&f.fingerprint)) {
            continue;
        }
        kept.insert(f.fingerprint, from_library(library, f, root)?);
        report.exported += 1;
        changed = true;
    }

    if changed || !path.exists() {
        let mut recordings: Vec<RecordingState> = kept.into_values().collect();
        recordings.sort_by(|a, b| a.path.cmp(&b.path));
        write(
            path,
            &StateFile {
                format: FORMAT.into(),
                version: VERSION,
                written_at: now_ms(),
                recordings,
            },
        )?;
    }
    Ok(report)
}

fn not_ours() -> &'static str {
    tr(
        "не файл разметки Sample Creator",
        "not a Sample Creator markup file",
    )
}

pub fn read(path: &Path) -> Result<StateFile> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("{} {}", tr("не прочитать", "cannot read"), path.display()))?;
    let state: StateFile = serde_json::from_str(&text)
        .with_context(|| format!("{}: {}", path.display(), not_ours()))?;
    if state.format != FORMAT {
        bail!("{}: {}", path.display(), not_ours());
    }
    if state.version > VERSION {
        bail!(
            "{}: {}",
            path.display(),
            tr(
                "записан более новой версией приложения",
                "written by a newer version of the app"
            )
        );
    }
    Ok(state)
}

/// Through a temporary file and a rename, so an unplugged recorder never
/// holds half a file.
fn write(path: &Path, state: &StateFile) -> Result<()> {
    let tmp = path.with_extension("json.part");
    let text = serde_json::to_string_pretty(state)?;
    std::fs::write(&tmp, text)
        .with_context(|| format!("{} {}", tr("не записать", "cannot write"), tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("{} {}", tr("не записать", "cannot write"), path.display()))?;
    Ok(())
}

fn relative(path: &Path, root: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Where a recording listed in the file is on this device. Paths are kept
/// relative to the volume root, but another device may have taken a folder
/// as the root (the desktop tree starts wherever it was opened), which shifts
/// them. Then the recording is looked up by name under the root, preferring
/// the longest matching tail of the path; the fingerprint corrects the rest
/// when the recording is opened.
struct Places<'a> {
    root: &'a Path,
    location: &'a Path,
    by_name: Option<HashMap<String, Vec<PathBuf>>>,
}

impl<'a> Places<'a> {
    fn new(root: &'a Path, location: &'a Path) -> Self {
        Places {
            root,
            location,
            by_name: None,
        }
    }

    fn find(&mut self, rel: &str) -> PathBuf {
        let parts: Vec<&str> = rel.split('/').filter(|p| !p.is_empty()).collect();
        let join = |base: &Path| {
            parts
                .iter()
                .fold(base.to_path_buf(), |p, part| p.join(part))
        };
        let direct = join(self.root);
        if direct.is_file() {
            return direct;
        }
        let near = join(self.location);
        if near.is_file() {
            return near;
        }
        let Some(name) = parts.last() else {
            return direct;
        };
        let root = self.root;
        let by_name = self.by_name.get_or_insert_with(|| {
            let mut found = HashMap::new();
            collect_audio(root, 0, &mut found);
            found
        });
        let tail = |p: &Path| {
            p.components()
                .rev()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .zip(parts.iter().rev())
                .take_while(|(a, b)| a == *b)
                .count()
        };
        by_name
            .get(*name)
            .and_then(|paths| paths.iter().max_by_key(|p| tail(p)))
            .cloned()
            .unwrap_or(direct)
    }
}

fn collect_audio(dir: &Path, depth: usize, found: &mut HashMap<String, Vec<PathBuf>>) {
    let Ok(items) = std::fs::read_dir(dir) else {
        return;
    };
    for item in items.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = item.path();
        match item.file_type() {
            Ok(t) if t.is_dir() && depth < 8 => collect_audio(&path, depth + 1, found),
            Ok(t) if t.is_file() && crate::audio::is_supported(&path) => {
                found.entry(name).or_default().push(path)
            }
            _ => {}
        }
    }
}

fn channels_key(m: ChannelMode) -> &'static str {
    match m {
        ChannelMode::AsSource => "as_source",
        ChannelMode::Left => "left",
        ChannelMode::Right => "right",
        ChannelMode::SplitMono => "split_mono",
    }
}

fn channels_from_key(k: &str) -> ChannelMode {
    match k {
        "left" => ChannelMode::Left,
        "right" => ChannelMode::Right,
        "split_mono" => ChannelMode::SplitMono,
        _ => ChannelMode::AsSource,
    }
}

fn from_library(library: &Library, f: &FileFull, root: &Path) -> Result<RecordingState> {
    let markup = library.markup(f.id)?;
    Ok(RecordingState {
        fingerprint: format!("{:016x}", f.fingerprint),
        path: relative(&f.path, root),
        alias: f.alias.clone(),
        reviewed: f.reviewed,
        cues_imported: f.cues_imported,
        sample_rate: f.sample_rate,
        channels: f.channels,
        total_frames: f.total_frames,
        updated_at: f.updated_at,
        markers: markup
            .markers
            .iter()
            .map(|m| MarkerState {
                pos: m.pos,
                name: m.name.clone(),
                color: m.color,
                from_recorder: m.from_recorder,
            })
            .collect(),
        regions: markup
            .regions
            .iter()
            .map(|r| RegionState {
                start: r.start,
                end: r.end,
                name: r.name.clone(),
                color: r.color,
                export: r.export,
                channels: channels_key(r.channels).into(),
                speed: r.speed,
                keep_pitch: r.keep_pitch,
                fx: r.fx,
                parts: r
                    .parts
                    .iter()
                    .map(|p| PartState {
                        start: p.start,
                        end: p.end,
                        fade_in: p.fade_in,
                        fade_out: p.fade_out,
                        overlap: p.overlap,
                    })
                    .collect(),
            })
            .collect(),
    })
}

fn to_library(rec: &RecordingState, fingerprint: u64, root: &Path) -> (FileFull, Markup) {
    let path = rec
        .path
        .split('/')
        .fold(root.to_path_buf(), |p, part| p.join(part));
    let file = FileFull {
        id: 0,
        fingerprint,
        path,
        alias: rec.alias.clone(),
        reviewed: rec.reviewed,
        cues_imported: rec.cues_imported,
        sample_rate: rec.sample_rate,
        channels: rec.channels,
        total_frames: rec.total_frames,
        updated_at: rec.updated_at,
    };
    let markup = Markup {
        file: 0,
        markers: rec
            .markers
            .iter()
            .map(|m| Marker {
                id: 0,
                file: 0,
                pos: m.pos,
                name: m.name.clone(),
                color: m.color,
                from_recorder: m.from_recorder,
            })
            .collect(),
        regions: rec
            .regions
            .iter()
            .map(|r| Region {
                id: 0,
                file: 0,
                start: r.start,
                end: r.end,
                name: r.name.clone(),
                color: r.color,
                export: r.export,
                channels: channels_from_key(&r.channels),
                speed: r.speed,
                keep_pitch: r.keep_pitch,
                fx: r.fx,
                parts: r
                    .parts
                    .iter()
                    .map(|p| Part {
                        start: p.start,
                        end: p.end,
                        fade_in: p.fade_in,
                        fade_out: p.fade_out,
                        overlap: p.overlap,
                    })
                    .collect(),
            })
            .collect(),
    };
    (file, markup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::history::{Change, History};

    fn region(file: i64, start: u64, name: &str) -> Region {
        Region {
            id: 0,
            file,
            start,
            end: start + 100,
            name: name.into(),
            color: 2,
            export: true,
            channels: ChannelMode::Left,
            speed: 150,
            keep_pitch: true,
            fx: Default::default(),
            parts: vec![Part {
                overlap: 0,
                ..Part::whole(start, start + 50)
            }],
        }
    }

    fn add(lib: &Library, file: i64, r: Region) {
        let mut markup = lib.markup(file).unwrap();
        History::default()
            .apply(&mut markup, lib, vec![Change::add_region(r)])
            .unwrap();
    }

    #[test]
    fn markup_travels_between_devices_by_fingerprint() {
        let dir = std::env::temp_dir().join(format!("sc-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let state = file_in(&dir);

        // Phone: the recorder is /storage/AAAA.
        let phone = Library::open_in_memory().unwrap();
        let phone_root = Path::new("/storage/AAAA");
        let f = phone
            .file(42, &phone_root.join("MUSIC/take.wav"), 48000, 2, 1000)
            .unwrap();
        add(&phone, f.id, region(f.id, 10, "riff"));
        phone.set_alias(f.id, Some("репетиция")).unwrap();
        // Recordings never edited stay out of the file.
        phone
            .file(7, &phone_root.join("MUSIC/other.wav"), 48000, 2, 10)
            .unwrap();
        let r = sync(&phone, phone_root, &state).unwrap();
        assert_eq!(
            r,
            Report {
                imported: 0,
                exported: 1,
                ..Default::default()
            }
        );

        // Computer: the same card is /run/media/me/DR40.
        let pc = Library::open_in_memory().unwrap();
        let pc_root = Path::new("/run/media/me/DR40");
        let r = sync(&pc, pc_root, &state).unwrap();
        assert_eq!(
            r,
            Report {
                imported: 1,
                exported: 0,
                ..Default::default()
            }
        );
        let rec = pc
            .file(42, &pc_root.join("MUSIC/take.wav"), 48000, 2, 1000)
            .unwrap();
        assert_eq!(rec.alias.as_deref(), Some("репетиция"));
        let markup = pc.markup(rec.id).unwrap();
        assert_eq!(markup.regions.len(), 1);
        assert_eq!(markup.regions[0].name, "riff");
        assert_eq!(markup.regions[0].parts.len(), 1);
        assert_eq!(markup.regions[0].channels, ChannelMode::Left);

        // Computer edits later (a deletion and an addition) → back to the phone.
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut pc_markup = pc.markup(rec.id).unwrap();
        let old = pc_markup.regions[0].clone();
        History::default()
            .apply(
                &mut pc_markup,
                &pc,
                vec![
                    Change::Region {
                        before: Some(old),
                        after: None,
                    },
                    Change::add_region(region(rec.id, 500, "solo")),
                ],
            )
            .unwrap();
        assert_eq!(sync(&pc, pc_root, &state).unwrap().exported, 1);
        assert_eq!(sync(&phone, phone_root, &state).unwrap().imported, 1);
        let names: Vec<String> = phone
            .markup(f.id)
            .unwrap()
            .regions
            .iter()
            .map(|r| r.name.clone())
            .collect();
        assert_eq!(names, ["solo"]);

        // Nothing new: nothing moves.
        assert_eq!(sync(&phone, phone_root, &state).unwrap(), Report::default());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn finds_recordings_when_roots_differ() {
        let dir = std::env::temp_dir().join(format!("sc-sync-roots-{}", std::process::id()));
        let card = dir.join("card");
        std::fs::create_dir_all(card.join("MUSIC")).unwrap();
        std::fs::create_dir_all(card.join("OTHER")).unwrap();
        std::fs::write(card.join("MUSIC/take.wav"), b"").unwrap();
        std::fs::write(card.join("OTHER/solo.wav"), b"").unwrap();
        let state = file_in(&card.join("MUSIC"));

        // The computer synced with the tree opened at MUSIC: "take.wav".
        let pc = Library::open_in_memory().unwrap();
        let pc_root = card.join("MUSIC");
        let f = pc
            .file(42, &pc_root.join("take.wav"), 48000, 2, 1000)
            .unwrap();
        add(&pc, f.id, region(f.id, 10, "riff"));
        pc.set_alias(f.id, Some("репетиция")).unwrap();
        sync(&pc, &pc_root, &state).unwrap();

        // The phone has the whole card as the root.
        let phone = Library::open_in_memory().unwrap();
        assert_eq!(sync(&phone, &card, &state).unwrap().imported, 1);
        let at = phone
            .summary_at(&card.join("MUSIC/take.wav"))
            .unwrap()
            .unwrap();
        assert_eq!(at.alias.as_deref(), Some("репетиция"));

        // Imported earlier to a path that is not there: put right on the next sync.
        let old = Library::open_in_memory().unwrap();
        let wrong = old.file(42, &card.join("take.wav"), 48000, 2, 1000).unwrap();
        old.set_updated_at(wrong.id, i64::MAX).unwrap();
        assert_eq!(sync(&old, &card, &state).unwrap().relinked, 1);
        assert!(old.summary_at(&card.join("MUSIC/take.wav")).unwrap().is_some());

        // Elsewhere in the tree: found by name.
        let mut places = Places::new(&card, &card);
        assert_eq!(places.find("x/OTHER/solo.wav"), card.join("OTHER/solo.wav"));
        assert_eq!(places.find("solo.wav"), card.join("OTHER/solo.wav"));
        assert_eq!(places.find("MUSIC/gone.wav"), card.join("MUSIC/gone.wav"));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn targets_are_remembered_per_volume() {
        let dir = std::env::temp_dir().join(format!("sc-sync-t-{}", std::process::id()));
        let notes = dir.join("NOTES");
        std::fs::create_dir_all(&notes).unwrap();
        let lib = Library::open_in_memory().unwrap();
        assert!(enable(&lib, &dir, Path::new("/elsewhere"), false).is_err());
        assert!(
            enable(&lib, &dir, &notes, true).is_err(),
            "no file there yet"
        );
        enable(&lib, &dir, &notes, false).unwrap();
        assert!(file_in(&notes).exists());
        let t = target(&lib, &dir).unwrap();
        assert_eq!(t.location, notes);
        assert!(t.last_sync.is_some());
        assert_eq!(sync_all(&lib).len(), 1);
        disable(&lib, &dir).unwrap();
        assert!(target(&lib, &dir).is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn rejects_foreign_files() {
        let dir = std::env::temp_dir().join(format!("sc-sync-bad-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = file_in(&dir);
        std::fs::write(
            &path,
            r#"{"format":"other","version":1,"written_at":0,"recordings":[]}"#,
        )
        .unwrap();
        assert!(read(&path).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
