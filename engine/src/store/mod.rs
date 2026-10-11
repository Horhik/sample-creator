//! The markup library: one SQLite file in `$XDG_DATA_HOME/sample-creator/`.
//!
//! Recordings are identified by content fingerprint, so markup follows a take
//! when it is renamed or copied off the recorder. The path is only the last
//! place we saw it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::core::history;
use crate::core::{ChannelMode, Id, Marker, Markup, Part, Region};

const SCHEMA_VERSION: i64 = 6;

pub struct Library {
    conn: Connection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRecord {
    pub id: Id,
    /// User's own name for the take, shown instead of the file name.
    pub alias: Option<String>,
    pub reviewed: bool,
    pub cues_imported: bool,
}

/// A recording as the review page needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub id: Id,
    pub path: PathBuf,
    pub alias: Option<String>,
    pub sample_rate: u32,
    pub channels: u16,
    pub total_frames: u64,
}

impl FileRow {
    /// Alias, or the file name without extension.
    pub fn title(&self) -> String {
        self.alias.clone().unwrap_or_else(|| {
            self.path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        })
    }
}

/// Everything stored about a recording itself, for sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFull {
    pub id: Id,
    pub fingerprint: u64,
    pub path: PathBuf,
    pub alias: Option<String>,
    pub reviewed: bool,
    pub cues_imported: bool,
    pub sample_rate: u32,
    pub channels: u16,
    pub total_frames: u64,
    /// See [`Library::updated_at`].
    pub updated_at: i64,
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// Per-file counters for the file tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileSummary {
    pub alias: Option<String>,
    pub regions: usize,
    pub markers: usize,
    pub reviewed: bool,
}

/// `$XDG_DATA_HOME/sample-creator`
pub fn data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("sample-creator"))
}

/// Parts as `start:end:fade_in:fade_out:overlap` joined by `;`; empty for none.
fn parts_to_db(parts: &[Part]) -> String {
    parts
        .iter()
        .map(|p| {
            format!(
                "{}:{}:{}:{}:{}",
                p.start, p.end, p.fade_in, p.fade_out, p.overlap
            )
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn parts_from_db(text: &str) -> Vec<Part> {
    let mut legacy = false;
    let mut parts: Vec<Part> = text
        .split(';')
        .filter_map(|p| {
            let fields: Vec<&str> = p.split(':').collect();
            let n = |i: usize| fields.get(i)?.parse::<u64>().ok();
            let overlap = match fields.get(4) {
                Some(v) => v.parse::<i64>().ok()?,
                None => {
                    legacy = true;
                    0
                }
            };
            Some(Part {
                start: n(0)?,
                end: n(1)?,
                fade_in: n(2)?,
                fade_out: n(3)?,
                overlap,
            })
        })
        .collect();
    // Written before overlaps were their own: they came from the meeting
    // fades, the shorter of the two.
    if legacy {
        for i in 1..parts.len() {
            let (fo, fi) = (parts[i - 1].fade_out, parts[i].fade_in);
            parts[i].overlap = fo.min(fi) as i64;
        }
    }
    parts
}

impl Library {
    pub fn open_default() -> Result<Self> {
        let dir = data_dir().context("neither XDG_DATA_HOME nor HOME is set")?;
        std::fs::create_dir_all(&dir)?;
        Self::open(&dir.join("library.db"))
    }

    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("cannot open library {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;",
        )?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE files (
                     id            INTEGER PRIMARY KEY,
                     fingerprint   INTEGER NOT NULL UNIQUE,
                     path          TEXT NOT NULL,
                     sample_rate   INTEGER NOT NULL,
                     channels      INTEGER NOT NULL,
                     total_frames  INTEGER NOT NULL,
                     reviewed      INTEGER NOT NULL DEFAULT 0,
                     cues_imported INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX files_path ON files(path);
                 CREATE TABLE markers (
                     id            INTEGER PRIMARY KEY,
                     file_id       INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                     pos           INTEGER NOT NULL,
                     name          TEXT NOT NULL,
                     color         INTEGER NOT NULL,
                     from_recorder INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX markers_file ON markers(file_id);
                 CREATE TABLE regions (
                     id            INTEGER PRIMARY KEY,
                     file_id       INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                     start         INTEGER NOT NULL,
                     end           INTEGER NOT NULL,
                     name          TEXT NOT NULL,
                     color         INTEGER NOT NULL,
                     export        INTEGER NOT NULL DEFAULT 1,
                     channel_mode  INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE INDEX regions_file ON regions(file_id);
                 PRAGMA user_version = 1;
                 COMMIT;",
            )?;
        }
        if version < 2 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE files ADD COLUMN alias TEXT;
                 PRAGMA user_version = 2;
                 COMMIT;",
            )?;
        }
        if version < 3 {
            conn.execute_batch(
                "BEGIN;
                 CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 PRAGMA user_version = 3;
                 COMMIT;",
            )?;
        }
        if version < 4 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE regions ADD COLUMN speed INTEGER NOT NULL DEFAULT 100;
                 ALTER TABLE regions ADD COLUMN keep_pitch INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 4;
                 COMMIT;",
            )?;
        }
        if version < 5 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE regions ADD COLUMN parts TEXT NOT NULL DEFAULT '';
                 PRAGMA user_version = 5;
                 COMMIT;",
            )?;
        }
        if version < 6 {
            conn.execute_batch(
                "BEGIN;
                 ALTER TABLE files ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 6;
                 COMMIT;",
            )?;
        }
        anyhow::ensure!(
            version <= SCHEMA_VERSION,
            "library was written by a newer version (schema {version})"
        );
        Ok(Library { conn })
    }

    /// Finds the recording by fingerprint (creating it on first sight) and
    /// remembers where it is now.
    pub fn file(
        &self,
        fingerprint: u64,
        path: &Path,
        sample_rate: u32,
        channels: u16,
        total_frames: u64,
    ) -> Result<FileRecord> {
        let fp = fingerprint as i64;
        let path = path.to_string_lossy();
        self.conn.execute(
            "INSERT INTO files (fingerprint, path, sample_rate, channels, total_frames)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(fingerprint) DO UPDATE SET path = excluded.path",
            params![fp, path, sample_rate, channels, total_frames as i64],
        )?;
        Ok(self.conn.query_row(
            "SELECT id, reviewed, cues_imported, alias FROM files WHERE fingerprint = ?1",
            [fp],
            |r| {
                Ok(FileRecord {
                    id: r.get(0)?,
                    reviewed: r.get(1)?,
                    cues_imported: r.get(2)?,
                    alias: r.get(3)?,
                })
            },
        )?)
    }

    pub fn set_reviewed(&self, file: Id, reviewed: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET reviewed = ?2, updated_at = ?3 WHERE id = ?1",
            params![file, reviewed, now_ms()],
        )?;
        Ok(())
    }

    /// When the recording's markup was last changed by the user (ms since the
    /// epoch; 0 for never). Sync uses it to decide which side is newer.
    pub fn updated_at(&self, file: Id) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT updated_at FROM files WHERE id = ?1", [file], |r| {
                r.get(0)
            })?)
    }

    pub fn set_updated_at(&self, file: Id, at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET updated_at = ?2 WHERE id = ?1",
            params![file, at],
        )?;
        Ok(())
    }

    /// Recordings whose last known path is under `root`.
    pub fn files_under(&self, root: &Path) -> Result<Vec<FileFull>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, fingerprint, path, alias, reviewed, cues_imported, sample_rate,
                    channels, total_frames, updated_at
             FROM files ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(FileFull {
                id: r.get(0)?,
                fingerprint: r.get::<_, i64>(1)? as u64,
                path: PathBuf::from(r.get::<_, String>(2)?),
                alias: r.get(3)?,
                reviewed: r.get(4)?,
                cues_imported: r.get(5)?,
                sample_rate: r.get(6)?,
                channels: r.get(7)?,
                total_frames: r.get::<_, i64>(8)? as u64,
                updated_at: r.get(9)?,
            })
        })?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter(|f| f.path.starts_with(root))
            .collect())
    }

    /// Replaces everything known about a recording with `file` and `markup`
    /// (ids in `markup` are ignored), in one transaction. Creates the
    /// recording if its fingerprint is new. Returns its id.
    pub fn replace_recording(&self, file: &FileFull, markup: &Markup) -> Result<Id> {
        let mut id = 0;
        history::Store::transaction(self, &mut || {
            self.conn.execute(
                "INSERT INTO files (fingerprint, path, sample_rate, channels, total_frames,
                                    alias, reviewed, cues_imported, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(fingerprint) DO UPDATE SET path = excluded.path,
                     alias = excluded.alias, reviewed = excluded.reviewed,
                     cues_imported = excluded.cues_imported, updated_at = excluded.updated_at",
                params![
                    file.fingerprint as i64,
                    file.path.to_string_lossy(),
                    file.sample_rate,
                    file.channels,
                    file.total_frames as i64,
                    file.alias,
                    file.reviewed,
                    file.cues_imported,
                    file.updated_at
                ],
            )?;
            id = self.conn.query_row(
                "SELECT id FROM files WHERE fingerprint = ?1",
                [file.fingerprint as i64],
                |r| r.get(0),
            )?;
            self.conn
                .execute("DELETE FROM markers WHERE file_id = ?1", [id])?;
            self.conn
                .execute("DELETE FROM regions WHERE file_id = ?1", [id])?;
            for m in &markup.markers {
                self.save_marker_raw(&Marker {
                    id: 0,
                    file: id,
                    ..m.clone()
                })?;
            }
            for r in &markup.regions {
                self.save_region_raw(&Region {
                    id: 0,
                    file: id,
                    ..r.clone()
                })?;
            }
            Ok(())
        })?;
        Ok(id)
    }

    /// `None` (or blank) goes back to the file name.
    pub fn set_alias(&self, file: Id, alias: Option<&str>) -> Result<()> {
        let alias = alias.map(str::trim).filter(|a| !a.is_empty());
        self.conn.execute(
            "UPDATE files SET alias = ?2, updated_at = ?3 WHERE id = ?1",
            params![file, alias, now_ms()],
        )?;
        Ok(())
    }

    pub fn set_cues_imported(&self, file: Id) -> Result<()> {
        self.conn
            .execute("UPDATE files SET cues_imported = 1 WHERE id = ?1", [file])?;
        Ok(())
    }

    pub fn markup(&self, file: Id) -> Result<Markup> {
        let mut markup = Markup {
            file,
            ..Default::default()
        };
        let mut stmt = self.conn.prepare_cached(
            "SELECT id, pos, name, color, from_recorder FROM markers WHERE file_id = ?1",
        )?;
        markup.markers = stmt
            .query_map([file], |r| {
                Ok(Marker {
                    id: r.get(0)?,
                    file,
                    pos: r.get::<_, i64>(1)? as u64,
                    name: r.get(2)?,
                    color: r.get(3)?,
                    from_recorder: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?;

        let mut stmt = self.conn.prepare_cached(
            "SELECT id, start, end, name, color, export, channel_mode, speed, keep_pitch, parts
             FROM regions WHERE file_id = ?1",
        )?;
        markup.regions = stmt
            .query_map([file], |r| {
                Ok(Region {
                    id: r.get(0)?,
                    file,
                    start: r.get::<_, i64>(1)? as u64,
                    end: r.get::<_, i64>(2)? as u64,
                    name: r.get(3)?,
                    color: r.get(4)?,
                    export: r.get(5)?,
                    channels: ChannelMode::from_db(r.get(6)?),
                    speed: r.get(7)?,
                    keep_pitch: r.get(8)?,
                    parts: parts_from_db(&r.get::<_, String>(9)?),
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        markup.sort();
        Ok(markup)
    }

    /// Every region in the library with its recording, grouped by recording
    /// (ordered by path) and sorted by start within it.
    pub fn all_regions(&self) -> Result<Vec<(FileRow, Region)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT f.id, f.path, f.alias, f.sample_rate, f.channels, f.total_frames,
                    r.id, r.start, r.end, r.name, r.color, r.export, r.channel_mode,
                    r.speed, r.keep_pitch, r.parts
             FROM regions r JOIN files f ON f.id = r.file_id
             ORDER BY f.path, f.id, r.start, r.end, r.id",
        )?;
        let rows = stmt.query_map([], |r| {
            let file = FileRow {
                id: r.get(0)?,
                path: PathBuf::from(r.get::<_, String>(1)?),
                alias: r.get(2)?,
                sample_rate: r.get(3)?,
                channels: r.get(4)?,
                total_frames: r.get::<_, i64>(5)? as u64,
            };
            let region = Region {
                id: r.get(6)?,
                file: file.id,
                start: r.get::<_, i64>(7)? as u64,
                end: r.get::<_, i64>(8)? as u64,
                name: r.get(9)?,
                color: r.get(10)?,
                export: r.get(11)?,
                channels: ChannelMode::from_db(r.get(12)?),
                speed: r.get(13)?,
                keep_pitch: r.get(14)?,
                parts: parts_from_db(&r.get::<_, String>(15)?),
            };
            Ok((file, region))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn setting(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_setting(&self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Settings whose key starts with `prefix`, with the prefix cut off.
    pub fn settings_with_prefix(&self, prefix: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT key, value FROM settings ORDER BY key")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get(1)?)))?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(k, v)| Some((k.strip_prefix(prefix)?.to_owned(), v)))
            .collect())
    }

    /// Counters for the file at `path`, if we have ever seen it.
    pub fn summary_at(&self, path: &Path) -> Result<Option<FileSummary>> {
        Ok(self
            .conn
            .prepare_cached(
                "SELECT f.alias, f.reviewed,
                        (SELECT count(*) FROM regions WHERE file_id = f.id),
                        (SELECT count(*) FROM markers WHERE file_id = f.id AND from_recorder = 0)
                 FROM files f WHERE f.path = ?1
                 ORDER BY f.id DESC LIMIT 1",
            )?
            .query_row([path.to_string_lossy()], |r| {
                Ok(FileSummary {
                    alias: r.get(0)?,
                    reviewed: r.get(1)?,
                    regions: r.get::<_, i64>(2)? as usize,
                    markers: r.get::<_, i64>(3)? as usize,
                })
            })
            .optional()?)
    }
}

impl Library {
    /// The user changed this recording's markup just now.
    fn touch(&self, file: Id) -> Result<()> {
        self.set_updated_at(file, now_ms())
    }

    fn touch_owner(&self, table: &str, id: Id) -> Result<()> {
        self.conn.execute(
            &format!(
                "UPDATE files SET updated_at = ?2
                 WHERE id = (SELECT file_id FROM {table} WHERE id = ?1)"
            ),
            params![id, now_ms()],
        )?;
        Ok(())
    }

    fn save_marker_raw(&self, m: &Marker) -> Result<Id> {
        let id = (m.id != 0).then_some(m.id);
        self.conn.execute(
            "INSERT INTO markers (id, file_id, pos, name, color, from_recorder)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(id) DO UPDATE SET pos = excluded.pos, name = excluded.name,
                 color = excluded.color, from_recorder = excluded.from_recorder",
            params![id, m.file, m.pos as i64, m.name, m.color, m.from_recorder],
        )?;
        Ok(id.unwrap_or_else(|| self.conn.last_insert_rowid()))
    }

    fn save_region_raw(&self, r: &Region) -> Result<Id> {
        let id = (r.id != 0).then_some(r.id);
        self.conn.execute(
            "INSERT INTO regions (id, file_id, start, end, name, color, export, channel_mode,
                                  speed, keep_pitch, parts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET start = excluded.start, end = excluded.end,
                 name = excluded.name, color = excluded.color, export = excluded.export,
                 channel_mode = excluded.channel_mode, speed = excluded.speed,
                 keep_pitch = excluded.keep_pitch, parts = excluded.parts",
            params![
                id,
                r.file,
                r.start as i64,
                r.end as i64,
                r.name,
                r.color,
                r.export,
                r.channels.to_db(),
                r.speed,
                r.keep_pitch,
                parts_to_db(&r.parts)
            ],
        )?;
        Ok(id.unwrap_or_else(|| self.conn.last_insert_rowid()))
    }
}

impl history::Store for Library {
    fn save_marker(&self, m: &Marker) -> Result<Id> {
        let id = self.save_marker_raw(m)?;
        self.touch(m.file)?;
        Ok(id)
    }

    fn delete_marker(&self, id: Id) -> Result<()> {
        self.touch_owner("markers", id)?;
        self.conn
            .execute("DELETE FROM markers WHERE id = ?1", [id])?;
        Ok(())
    }

    fn save_region(&self, r: &Region) -> Result<Id> {
        let id = self.save_region_raw(r)?;
        self.touch(r.file)?;
        Ok(id)
    }

    fn delete_region(&self, id: Id) -> Result<()> {
        self.touch_owner("regions", id)?;
        self.conn
            .execute("DELETE FROM regions WHERE id = ?1", [id])?;
        Ok(())
    }

    fn transaction(&self, f: &mut dyn FnMut() -> Result<()>) -> Result<()> {
        self.conn.execute_batch("BEGIN")?;
        match f() {
            Ok(()) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(())
            }
            Err(err) => {
                self.conn.execute_batch("ROLLBACK").ok();
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::history::{Change, History};

    fn region(file: Id, start: u64) -> Region {
        Region {
            id: 0,
            file,
            start,
            end: start + 100,
            name: "kick".into(),
            color: 1,
            export: true,
            channels: ChannelMode::SplitMono,
            speed: 150,
            keep_pitch: true,
            parts: vec![
                Part {
                    fade_out: 5,
                    ..Part::whole(start + 10, start + 50)
                },
                Part {
                    overlap: -3,
                    ..Part::whole(start + 60, start + 100)
                },
            ],
        }
    }

    #[test]
    fn legacy_parts_get_overlaps_from_their_fades() {
        let parts = parts_from_db("0:100:0:20;100:200:30:0");
        assert_eq!(parts[1].overlap, 20);
        assert_eq!(parts_from_db(&parts_to_db(&parts)), parts);
    }

    #[test]
    fn file_identity_follows_fingerprint_not_path() {
        let lib = Library::open_in_memory().unwrap();
        let a = lib
            .file(42, Path::new("/rec/A.wav"), 48000, 2, 1000)
            .unwrap();
        let moved = lib
            .file(42, Path::new("/home/me/A.wav"), 48000, 2, 1000)
            .unwrap();
        let other = lib
            .file(43, Path::new("/rec/B.wav"), 48000, 2, 1000)
            .unwrap();
        assert_eq!(a.id, moved.id);
        assert_ne!(a.id, other.id);
        assert!(
            lib.summary_at(Path::new("/home/me/A.wav"))
                .unwrap()
                .is_some()
        );

        lib.set_alias(a.id, Some("  bass jam ")).unwrap();
        let again = lib
            .file(42, Path::new("/home/me/A.wav"), 48000, 2, 1000)
            .unwrap();
        assert_eq!(again.alias.as_deref(), Some("bass jam"));
        lib.set_alias(a.id, Some("   ")).unwrap();
        let summary = lib
            .summary_at(Path::new("/home/me/A.wav"))
            .unwrap()
            .unwrap();
        assert_eq!(summary.alias, None);
    }

    #[test]
    fn migrates_a_version_1_library() {
        let dir = std::env::temp_dir().join(format!("sc-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("library.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE files (id INTEGER PRIMARY KEY, fingerprint INTEGER NOT NULL UNIQUE,
                     path TEXT NOT NULL, sample_rate INTEGER NOT NULL, channels INTEGER NOT NULL,
                     total_frames INTEGER NOT NULL, reviewed INTEGER NOT NULL DEFAULT 0,
                     cues_imported INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE markers (id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL,
                     pos INTEGER NOT NULL, name TEXT NOT NULL, color INTEGER NOT NULL,
                     from_recorder INTEGER NOT NULL DEFAULT 0);
                 CREATE TABLE regions (id INTEGER PRIMARY KEY, file_id INTEGER NOT NULL,
                     start INTEGER NOT NULL, end INTEGER NOT NULL, name TEXT NOT NULL,
                     color INTEGER NOT NULL, export INTEGER NOT NULL DEFAULT 1,
                     channel_mode INTEGER NOT NULL DEFAULT 0);
                 INSERT INTO regions (file_id, start, end, name, color) VALUES (1, 0, 5, 'old', 0);
                 INSERT INTO files (fingerprint, path, sample_rate, channels, total_frames)
                     VALUES (7, '/old.wav', 48000, 2, 10);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let lib = Library::open(&path).unwrap();
        let rec = lib.file(7, Path::new("/old.wav"), 48000, 2, 10).unwrap();
        assert_eq!(rec.alias, None);
        let old = &lib.markup(rec.id).unwrap().regions[0];
        assert_eq!((old.speed, old.keep_pitch), (100, false));
        assert!(old.parts.is_empty());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn edits_round_trip_through_sqlite() {
        let lib = Library::open_in_memory().unwrap();
        let file = lib
            .file(1, Path::new("/a.wav"), 48000, 2, 10_000)
            .unwrap()
            .id;
        let mut markup = lib.markup(file).unwrap();
        let mut history = History::default();

        history
            .apply(
                &mut markup,
                &lib,
                vec![
                    Change::add_region(region(file, 500)),
                    Change::add_region(region(file, 100)),
                    Change::add_marker(Marker {
                        id: 0,
                        file,
                        pos: 7,
                        name: "MARK_01".into(),
                        color: 2,
                        from_recorder: true,
                    }),
                ],
            )
            .unwrap();
        assert_eq!(lib.markup(file).unwrap(), markup);

        let summary = lib.summary_at(Path::new("/a.wav")).unwrap().unwrap();
        assert_eq!((summary.regions, summary.markers), (2, 0));

        history.undo(&mut markup, &lib).unwrap();
        assert_eq!(
            lib.markup(file).unwrap(),
            Markup {
                file,
                ..Default::default()
            }
        );
        history.redo(&mut markup, &lib).unwrap();
        assert_eq!(lib.markup(file).unwrap(), markup);
        assert_eq!(markup.regions[0].channels, ChannelMode::SplitMono);

        let other = lib.file(2, Path::new("/0.wav"), 44100, 1, 10).unwrap().id;
        history
            .apply(
                &mut markup,
                &lib,
                vec![Change::add_region(region(other, 3))],
            )
            .unwrap();
        let all = lib.all_regions().unwrap();
        let order: Vec<(&str, u64)> = all
            .iter()
            .map(|(f, r)| (f.path.to_str().unwrap(), r.start))
            .collect();
        assert_eq!(order, [("/0.wav", 3), ("/a.wav", 100), ("/a.wav", 500)]);
        assert_eq!(all[0].0.title(), "0");
    }

    #[test]
    fn settings_round_trip() {
        let lib = Library::open_in_memory().unwrap();
        assert_eq!(lib.setting("export.dir"), None);
        lib.set_setting("export.dir", "/a").unwrap();
        lib.set_setting("export.dir", "/b").unwrap();
        assert_eq!(lib.setting("export.dir").as_deref(), Some("/b"));
    }
}
