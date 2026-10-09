//! The markup library: one SQLite file in `$XDG_DATA_HOME/sample-creator/`.
//!
//! Recordings are identified by content fingerprint, so markup follows a take
//! when it is renamed or copied off the recorder. The path is only the last
//! place we saw it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};

use crate::core::history;
use crate::core::{ChannelMode, Id, Marker, Markup, Region};

const SCHEMA_VERSION: i64 = 2;

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
            "UPDATE files SET reviewed = ?2 WHERE id = ?1",
            params![file, reviewed],
        )?;
        Ok(())
    }

    /// `None` (or blank) goes back to the file name.
    pub fn set_alias(&self, file: Id, alias: Option<&str>) -> Result<()> {
        let alias = alias.map(str::trim).filter(|a| !a.is_empty());
        self.conn.execute(
            "UPDATE files SET alias = ?2 WHERE id = ?1",
            params![file, alias],
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
            "SELECT id, start, end, name, color, export, channel_mode
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
                })
            })?
            .collect::<rusqlite::Result<_>>()?;
        markup.sort();
        Ok(markup)
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

impl history::Store for Library {
    fn save_marker(&self, m: &Marker) -> Result<Id> {
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

    fn delete_marker(&self, id: Id) -> Result<()> {
        self.conn
            .execute("DELETE FROM markers WHERE id = ?1", [id])?;
        Ok(())
    }

    fn save_region(&self, r: &Region) -> Result<Id> {
        let id = (r.id != 0).then_some(r.id);
        self.conn.execute(
            "INSERT INTO regions (id, file_id, start, end, name, color, export, channel_mode)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET start = excluded.start, end = excluded.end,
                 name = excluded.name, color = excluded.color, export = excluded.export,
                 channel_mode = excluded.channel_mode",
            params![
                id,
                r.file,
                r.start as i64,
                r.end as i64,
                r.name,
                r.color,
                r.export,
                r.channels.to_db()
            ],
        )?;
        Ok(id.unwrap_or_else(|| self.conn.last_insert_rowid()))
    }

    fn delete_region(&self, id: Id) -> Result<()> {
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
        }
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
                 INSERT INTO files (fingerprint, path, sample_rate, channels, total_frames)
                     VALUES (7, '/old.wav', 48000, 2, 10);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        }
        let lib = Library::open(&path).unwrap();
        let rec = lib.file(7, Path::new("/old.wav"), 48000, 2, 10).unwrap();
        assert_eq!(rec.alias, None);
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
    }
}
