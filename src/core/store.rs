use std::sync::Mutex;

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension as _, Row, params};

use crate::core::model::{File, FileKind, Item, Job, JobKind, JobState, Preset};

// No FOREIGN KEY clauses. They were originally left out because turso, which this
// replaced, was a from-scratch SQLite reimplementation whose constraint support
// was still moving. Real SQLite would support them, but they cannot be added to
// existing tables, and databases written by the turso build are already out
// there — so the schema stays as it is. Referential integrity is maintained by
// save_job/delete_job, which rewrite a job's children as a unit, in one
// transaction.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS job (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL DEFAULT 'download',
    url        TEXT NOT NULL,
    title      TEXT NOT NULL,
    preset     TEXT NOT NULL,
    state      TEXT NOT NULL,
    error      TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS item (
    id          TEXT PRIMARY KEY,
    job_id      TEXT NOT NULL,
    idx         INTEGER NOT NULL,
    title       TEXT NOT NULL,
    duration    REAL,
    thumb_path  TEXT,
    webpage_url TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS file (
    id        TEXT PRIMARY KEY,
    item_id   TEXT NOT NULL,
    path      TEXT NOT NULL,
    kind      TEXT NOT NULL,
    format_id TEXT,
    bytes     INTEGER
);
CREATE TABLE IF NOT EXISTS preset (
    name         TEXT PRIMARY KEY,
    is_default   INTEGER NOT NULL,
    options_json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS app_setting (
    k TEXT PRIMARY KEY,
    v TEXT NOT NULL
);
"#;

/// Reads a TEXT column, tolerating NULL by yielding an empty string.
///
/// The columns that use this are all NOT NULL, so this should never fire; it is
/// kept because a library written by an older build is not worth crashing over.
fn text_at(row: &Row, idx: usize) -> rusqlite::Result<String> {
    Ok(row.get::<_, Option<String>>(idx)?.unwrap_or_default())
}

/// The library database.
///
/// `rusqlite::Connection` is `Send` but not `Sync`, and the store is shared
/// across worker threads behind an `Arc`, so the connection sits behind a mutex.
/// That also serialises access, which is what SQLite wants from a single
/// connection anyway.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    fn conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        // A poisoned lock means some other writer panicked mid-statement. The
        // connection itself is still usable, and losing the whole library
        // because of one failed write would be worse.
        Ok(self.conn.lock().unwrap_or_else(|e| e.into_inner()))
    }

    /// Opens (creating if absent) the library database and applies the schema.
    pub fn open(path: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        // ponytail: no migration framework — a DB from before `kind` existed
        // just gets the column bolted on. An error here means the column is
        // already present (fresh DB, or already migrated), so it is ignored.
        let _ = conn.execute(
            "ALTER TABLE job ADD COLUMN kind TEXT NOT NULL DEFAULT 'download'",
            [],
        );
        Ok(Self { conn: Mutex::new(conn) })
    }

    /// Writes a job and its full item/file subtree.
    ///
    /// Call this on state transitions only — never on progress ticks. Live
    /// progress belongs in memory on the interface; a write per tick would mean
    /// ~10 disk writes/second per active download.
    pub fn save_job(&self, job: &Job) -> Result<()> {
        let mut conn = self.conn()?;
        // One transaction: the children are deleted and rewritten, and a failure
        // halfway through must not leave the job with half an item list.
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO job (id, kind, url, title, preset, state, error, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(id) DO UPDATE SET
               kind=?2, url=?3, title=?4, preset=?5, state=?6, error=?7, created_at=?8",
            params![
                job.id,
                job.kind.as_str(),
                job.url,
                job.title,
                job.preset,
                job.state.as_str(),
                job.error,
                job.created_at,
            ],
        )?;

        // Children are rewritten wholesale so a shrinking item list cannot leave
        // orphans behind.
        tx.execute(
            "DELETE FROM file WHERE item_id IN (SELECT id FROM item WHERE job_id = ?1)",
            params![job.id],
        )?;
        tx.execute("DELETE FROM item WHERE job_id = ?1", params![job.id])?;

        for item in &job.items {
            tx.execute(
                "INSERT INTO item (id, job_id, idx, title, duration, thumb_path, webpage_url)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    item.id,
                    job.id,
                    item.index,
                    item.title,
                    item.duration,
                    item.thumb_path,
                    item.webpage_url,
                ],
            )?;

            for f in &item.files {
                tx.execute(
                    "INSERT INTO file (id, item_id, path, kind, format_id, bytes)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![f.id, item.id, f.path, f.kind.as_str(), f.format_id, f.bytes],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Deletes a job and its item/file subtree.
    pub fn delete_job(&self, job_id: &str) -> Result<()> {
        let mut conn = self.conn()?;
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM file WHERE item_id IN (SELECT id FROM item WHERE job_id = ?1)",
            params![job_id],
        )?;
        tx.execute("DELETE FROM item WHERE job_id = ?1", params![job_id])?;
        tx.execute("DELETE FROM job WHERE id = ?1", params![job_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Loads every job, newest first, with items and files attached.
    ///
    /// ponytail: N+1 queries (one per job for items, one per item for files).
    /// Fine at library sizes measured in hundreds; if the sidebar ever feels
    /// slow, replace with three flat SELECTs joined in memory.
    pub fn load_jobs(&self) -> Result<Vec<Job>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, kind, url, title, preset, state, error, created_at
             FROM job ORDER BY created_at DESC, id DESC",
        )?;
        let mut jobs = stmt
            .query_map([], |row| {
                Ok(Job {
                    id: text_at(row, 0)?,
                    kind: JobKind::parse(&text_at(row, 1)?),
                    url: text_at(row, 2)?,
                    title: text_at(row, 3)?,
                    preset: text_at(row, 4)?,
                    state: JobState::parse(&text_at(row, 5)?),
                    error: row.get(6)?,
                    created_at: row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                    items: Vec::new(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        // `&conn` rather than re-locking: the mutex is not reentrant, so a
        // nested `self.conn()` here would deadlock.
        for job in &mut jobs {
            job.items = load_items(&conn, &job.id)?;
        }
        Ok(jobs)
    }

    // -- presets -----------------------------------------------------------

    /// Upserts a preset. Options are stored as JSON so adding a field later is
    /// a serde default, not a schema migration.
    pub fn save_preset(&self, preset: &Preset) -> Result<()> {
        let json = serde_json::to_string(&preset.options)?;
        let conn = self.conn()?;
        conn.execute(
            "INSERT INTO preset (name, is_default, options_json) VALUES (?1, ?2, ?3)
             ON CONFLICT(name) DO UPDATE SET is_default=?2, options_json=?3",
            params![preset.name, preset.is_default as i64, json],
        )?;
        // Exactly one preset may be the default.
        if preset.is_default {
            conn.execute(
                "UPDATE preset SET is_default = 0 WHERE name <> ?1",
                params![preset.name],
            )?;
        }
        Ok(())
    }

    pub fn delete_preset(&self, name: &str) -> Result<()> {
        self.conn()?
            .execute("DELETE FROM preset WHERE name = ?1", params![name])?;
        Ok(())
    }

    pub fn load_presets(&self) -> Result<Vec<Preset>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT name, is_default, options_json FROM preset ORDER BY name ASC")?;
        let out = stmt
            .query_map([], |row| {
                let json = text_at(row, 2)?;
                Ok(Preset {
                    name: text_at(row, 0)?,
                    is_default: row.get::<_, Option<i64>>(1)?.unwrap_or(0) != 0,
                    // A preset written by a newer version must not break
                    // startup; fall back to defaults for anything unparseable.
                    options: serde_json::from_str(&json).unwrap_or_default(),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(out)
    }

    // -- settings ----------------------------------------------------------

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn()?.execute(
            "INSERT INTO app_setting (k, v) VALUES (?1, ?2)
             ON CONFLICT(k) DO UPDATE SET v=?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn()?;
        let found = conn
            .query_row(
                "SELECT v FROM app_setting WHERE k = ?1",
                params![key],
                |row| text_at(row, 0),
            )
            .optional()?;
        Ok(found)
    }
}

fn load_items(conn: &Connection, job_id: &str) -> Result<Vec<Item>> {
    let mut stmt = conn.prepare(
        "SELECT id, idx, title, duration, thumb_path, webpage_url
         FROM item WHERE job_id = ?1 ORDER BY idx ASC",
    )?;
    let mut items = stmt
        .query_map(params![job_id], |row| {
            Ok(Item {
                id: text_at(row, 0)?,
                index: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
                title: text_at(row, 2)?,
                duration: row.get(3)?,
                thumb_path: row.get(4)?,
                webpage_url: text_at(row, 5)?,
                files: Vec::new(),
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);

    for item in &mut items {
        item.files = load_files(conn, &item.id)?;
    }
    Ok(items)
}

fn load_files(conn: &Connection, item_id: &str) -> Result<Vec<File>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, kind, format_id, bytes
         FROM file WHERE item_id = ?1 ORDER BY id ASC",
    )?;
    let files = stmt
        .query_map(params![item_id], |row| {
            Ok(File {
                id: text_at(row, 0)?,
                path: text_at(row, 1)?,
                kind: FileKind::parse(&text_at(row, 2)?),
                format_id: row.get(3)?,
                bytes: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::new_id;

    fn sample_job() -> Job {
        let mut job = Job::new("https://example.com/playlist?list=abc", "1080p archive");
        job.title = "Lo-fi beats — ąćęłń 日本語".into(); // non-ASCII round-trip
        job.state = JobState::Done;
        job.items = vec![
            Item {
                id: new_id("itm"),
                index: 0,
                title: "01 Intro".into(),
                duration: Some(212.5),
                thumb_path: Some(r"C:\dl\01.webp".into()),
                webpage_url: "https://example.com/watch?v=1".into(),
                files: vec![
                    File {
                        id: new_id("fil"),
                        path: r"C:\dl\01 Intro.mp4".into(),
                        kind: FileKind::Video,
                        format_id: Some("137+140".into()),
                        bytes: Some(412_000_000),
                    },
                    File {
                        id: new_id("fil"),
                        path: r"C:\dl\01 Intro.en.srt".into(),
                        kind: FileKind::Subtitle,
                        format_id: None,
                        bytes: None,
                    },
                ],
            },
            Item {
                id: new_id("itm"),
                index: 1,
                title: "02 Rain".into(),
                duration: None,
                thumb_path: None,
                webpage_url: "https://example.com/watch?v=2".into(),
                files: vec![],
            },
        ];
        job
    }

    #[test]
    fn job_round_trips_through_sqlite() {
        {
            let dir = std::env::temp_dir().join(new_id("rustydlp-test"));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("library.db");

            let store = Store::open(path.to_str().unwrap()).unwrap();
            let job = sample_job();
            store.save_job(&job).unwrap();

            let loaded = store.load_jobs().unwrap();
            assert_eq!(loaded.len(), 1, "expected exactly one job");
            assert_eq!(loaded[0], job, "job did not survive the round trip");

            // Saving again must not duplicate rows or orphan children.
            store.save_job(&job).unwrap();
            let again = store.load_jobs().unwrap();
            assert_eq!(again.len(), 1, "re-saving duplicated the job");
            assert_eq!(again[0].items.len(), 2);
            assert_eq!(again[0].items[0].files.len(), 2);

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    fn temp_store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(new_id("rustydlp-test"));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.join("library.db").to_str().unwrap()).unwrap();
        (store, dir)
    }

    #[test]
    fn presets_round_trip_and_only_one_stays_default() {
        {
            let (store, dir) = temp_store();

            for p in Preset::seeds("D:/Videos") {
                store.save_preset(&p).unwrap();
            }

            let loaded = store.load_presets().unwrap();
            assert_eq!(loaded.len(), 4);
            assert_eq!(
                loaded.iter().filter(|p| p.is_default).count(),
                1,
                "exactly one preset may be default"
            );

            // Options must survive the JSON round trip intact.
            let best = loaded.iter().find(|p| p.name == "Best (1080p mp4)").unwrap();
            assert_eq!(best.options.max_height, Some(1080));
            assert_eq!(best.options.container.as_deref(), Some("mp4"));
            assert!(best.is_default);

            // Promoting another preset must demote the previous default.
            let mut mp3 = loaded.iter().find(|p| p.name == "MP3 audio").unwrap().clone();
            mp3.is_default = true;
            store.save_preset(&mp3).unwrap();

            let after = store.load_presets().unwrap();
            assert_eq!(after.iter().filter(|p| p.is_default).count(), 1);
            assert!(after.iter().find(|p| p.name == "MP3 audio").unwrap().is_default);
            assert!(!after.iter().find(|p| p.name == "Best (1080p mp4)").unwrap().is_default);

            store.delete_preset("M4A audio").unwrap();
            assert_eq!(store.load_presets().unwrap().len(), 3);

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn settings_round_trip_and_overwrite() {
        {
            let (store, dir) = temp_store();

            assert_eq!(store.get_setting("download_dir").unwrap(), None);
            store.set_setting("download_dir", "D:/Videos").unwrap();
            assert_eq!(
                store.get_setting("download_dir").unwrap().as_deref(),
                Some("D:/Videos")
            );
            store.set_setting("download_dir", "E:/Other").unwrap();
            assert_eq!(
                store.get_setting("download_dir").unwrap().as_deref(),
                Some("E:/Other"),
                "second write must overwrite, not duplicate"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    /// A shrinking item list must not leave orphaned items/files behind.
    #[test]
    fn resaving_with_fewer_items_prunes_children() {
        {
            let dir = std::env::temp_dir().join(new_id("rustydlp-test"));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("library.db");

            let store = Store::open(path.to_str().unwrap()).unwrap();
            let mut job = sample_job();
            store.save_job(&job).unwrap();

            job.items.truncate(1);
            job.items[0].files.truncate(1);
            store.save_job(&job).unwrap();

            let loaded = store.load_jobs().unwrap();
            assert_eq!(loaded[0].items.len(), 1);
            assert_eq!(loaded[0].items[0].files.len(), 1);
            assert_eq!(loaded[0], job);

            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
