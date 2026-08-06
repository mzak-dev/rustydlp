use crate::model::{File, FileKind, Item, Job, JobState, Preset};
use anyhow::Result;
use turso::{Builder, Connection, Value, params::params_from_iter};

// No FOREIGN KEY clauses: turso is a from-scratch SQLite reimplementation and
// constraint support is still moving. Referential integrity is maintained by
// save_job/delete_job, which always rewrite a job's children as a unit.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS job (
    id         TEXT PRIMARY KEY,
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

fn text(s: impl Into<String>) -> Value {
    Value::Text(s.into())
}

fn opt_text(s: &Option<String>) -> Value {
    match s {
        Some(v) => Value::Text(v.clone()),
        None => Value::Null,
    }
}

fn opt_int(v: Option<i64>) -> Value {
    match v {
        Some(v) => Value::Integer(v),
        None => Value::Null,
    }
}

fn opt_real(v: Option<f64>) -> Value {
    match v {
        Some(v) => Value::Real(v),
        None => Value::Null,
    }
}

/// Reads a nullable TEXT column, mapping SQL NULL to None.
fn get_opt_text(row: &turso::Row, idx: usize) -> Result<Option<String>> {
    Ok(match row.get_value(idx)? {
        Value::Text(s) => Some(s),
        _ => None,
    })
}

fn get_text(row: &turso::Row, idx: usize) -> Result<String> {
    Ok(get_opt_text(row, idx)?.unwrap_or_default())
}

fn get_opt_int(row: &turso::Row, idx: usize) -> Result<Option<i64>> {
    Ok(match row.get_value(idx)? {
        Value::Integer(i) => Some(i),
        _ => None,
    })
}

fn get_opt_real(row: &turso::Row, idx: usize) -> Result<Option<f64>> {
    Ok(match row.get_value(idx)? {
        Value::Real(f) => Some(f),
        Value::Integer(i) => Some(i as f64),
        _ => None,
    })
}

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if absent) the library database and applies the schema.
    pub async fn open(path: &str) -> Result<Self> {
        let db = Builder::new_local(path).build().await?;
        let conn = db.connect()?;
        conn.execute_batch(SCHEMA).await?;
        Ok(Self { conn })
    }

    /// Writes a job and its full item/file subtree.
    ///
    /// Call this on state transitions only — never on progress ticks. Live
    /// progress belongs in memory on the gpui entity; a write per tick would
    /// mean ~10 disk writes/second per active download.
    pub async fn save_job(&self, job: &Job) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO job (id, url, title, preset, state, error, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                   url=?2, title=?3, preset=?4, state=?5, error=?6, created_at=?7",
                params_from_iter(vec![
                    text(&job.id),
                    text(&job.url),
                    text(&job.title),
                    text(&job.preset),
                    text(job.state.as_str()),
                    opt_text(&job.error),
                    Value::Integer(job.created_at),
                ]),
            )
            .await?;

        // Children are rewritten wholesale so a shrinking item list can't leave
        // orphans behind.
        self.conn
            .execute(
                "DELETE FROM file WHERE item_id IN (SELECT id FROM item WHERE job_id = ?1)",
                params_from_iter(vec![text(&job.id)]),
            )
            .await?;
        self.conn
            .execute(
                "DELETE FROM item WHERE job_id = ?1",
                params_from_iter(vec![text(&job.id)]),
            )
            .await?;

        for item in &job.items {
            self.conn
                .execute(
                    "INSERT INTO item (id, job_id, idx, title, duration, thumb_path, webpage_url)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params_from_iter(vec![
                        text(&item.id),
                        text(&job.id),
                        Value::Integer(item.index),
                        text(&item.title),
                        opt_real(item.duration),
                        opt_text(&item.thumb_path),
                        text(&item.webpage_url),
                    ]),
                )
                .await?;

            for f in &item.files {
                self.conn
                    .execute(
                        "INSERT INTO file (id, item_id, path, kind, format_id, bytes)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        params_from_iter(vec![
                            text(&f.id),
                            text(&item.id),
                            text(&f.path),
                            text(f.kind.as_str()),
                            opt_text(&f.format_id),
                            opt_int(f.bytes),
                        ]),
                    )
                    .await?;
            }
        }
        Ok(())
    }

    /// Deletes a job and its item/file subtree.
    pub async fn delete_job(&self, job_id: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM file WHERE item_id IN (SELECT id FROM item WHERE job_id = ?1)",
                params_from_iter(vec![text(job_id)]),
            )
            .await?;
        self.conn
            .execute(
                "DELETE FROM item WHERE job_id = ?1",
                params_from_iter(vec![text(job_id)]),
            )
            .await?;
        self.conn
            .execute(
                "DELETE FROM job WHERE id = ?1",
                params_from_iter(vec![text(job_id)]),
            )
            .await?;
        Ok(())
    }

    /// Loads every job, newest first, with items and files attached.
    ///
    /// ponytail: N+1 queries (one per job for items, one per item for files).
    /// Fine at library sizes measured in hundreds; if the sidebar ever feels
    /// slow, replace with three flat SELECTs joined in memory.
    pub async fn load_jobs(&self) -> Result<Vec<Job>> {
        let mut jobs = Vec::new();
        let mut rows = self
            .conn
            .query(
                "SELECT id, url, title, preset, state, error, created_at
                 FROM job ORDER BY created_at DESC, id DESC",
                (),
            )
            .await?;

        while let Some(row) = rows.next().await? {
            jobs.push(Job {
                id: get_text(&row, 0)?,
                url: get_text(&row, 1)?,
                title: get_text(&row, 2)?,
                preset: get_text(&row, 3)?,
                state: JobState::parse(&get_text(&row, 4)?),
                error: get_opt_text(&row, 5)?,
                created_at: get_opt_int(&row, 6)?.unwrap_or(0),
                items: Vec::new(),
            });
        }

        for job in &mut jobs {
            job.items = self.load_items(&job.id).await?;
        }
        Ok(jobs)
    }

    async fn load_items(&self, job_id: &str) -> Result<Vec<Item>> {
        let mut items = Vec::new();
        let mut rows = self
            .conn
            .query(
                "SELECT id, idx, title, duration, thumb_path, webpage_url
                 FROM item WHERE job_id = ?1 ORDER BY idx ASC",
                params_from_iter(vec![text(job_id)]),
            )
            .await?;

        while let Some(row) = rows.next().await? {
            items.push(Item {
                id: get_text(&row, 0)?,
                index: get_opt_int(&row, 1)?.unwrap_or(0),
                title: get_text(&row, 2)?,
                duration: get_opt_real(&row, 3)?,
                thumb_path: get_opt_text(&row, 4)?,
                webpage_url: get_text(&row, 5)?,
                files: Vec::new(),
            });
        }

        for item in &mut items {
            item.files = self.load_files(&item.id).await?;
        }
        Ok(items)
    }

    // -- presets -----------------------------------------------------------

    /// Upserts a preset. Options are stored as JSON so adding a field later is
    /// a serde default, not a schema migration.
    pub async fn save_preset(&self, preset: &Preset) -> Result<()> {
        let json = serde_json::to_string(&preset.options)?;
        self.conn
            .execute(
                "INSERT INTO preset (name, is_default, options_json) VALUES (?1, ?2, ?3)
                 ON CONFLICT(name) DO UPDATE SET is_default=?2, options_json=?3",
                params_from_iter(vec![
                    text(&preset.name),
                    Value::Integer(preset.is_default as i64),
                    text(json),
                ]),
            )
            .await?;
        if preset.is_default {
            self.clear_other_defaults(&preset.name).await?;
        }
        Ok(())
    }

    /// Exactly one preset may be the default.
    async fn clear_other_defaults(&self, keep: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE preset SET is_default = 0 WHERE name <> ?1",
                params_from_iter(vec![text(keep)]),
            )
            .await?;
        Ok(())
    }

    pub async fn delete_preset(&self, name: &str) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM preset WHERE name = ?1",
                params_from_iter(vec![text(name)]),
            )
            .await?;
        Ok(())
    }

    pub async fn load_presets(&self) -> Result<Vec<Preset>> {
        let mut out = Vec::new();
        let mut rows = self
            .conn
            .query(
                "SELECT name, is_default, options_json FROM preset ORDER BY name ASC",
                (),
            )
            .await?;

        while let Some(row) = rows.next().await? {
            let json = get_text(&row, 2)?;
            // A preset written by a newer version must not break startup;
            // fall back to defaults for anything unparseable.
            let options = serde_json::from_str(&json).unwrap_or_default();
            out.push(Preset {
                name: get_text(&row, 0)?,
                is_default: get_opt_int(&row, 1)?.unwrap_or(0) != 0,
                options,
            });
        }
        Ok(out)
    }

    // -- settings ----------------------------------------------------------

    pub async fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.conn
            .execute(
                "INSERT INTO app_setting (k, v) VALUES (?1, ?2)
                 ON CONFLICT(k) DO UPDATE SET v=?2",
                params_from_iter(vec![text(key), text(value)]),
            )
            .await?;
        Ok(())
    }

    pub async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let mut rows = self
            .conn
            .query(
                "SELECT v FROM app_setting WHERE k = ?1",
                params_from_iter(vec![text(key)]),
            )
            .await?;
        match rows.next().await? {
            Some(row) => Ok(Some(get_text(&row, 0)?)),
            None => Ok(None),
        }
    }

    async fn load_files(&self, item_id: &str) -> Result<Vec<File>> {
        let mut files = Vec::new();
        let mut rows = self
            .conn
            .query(
                "SELECT id, path, kind, format_id, bytes
                 FROM file WHERE item_id = ?1 ORDER BY id ASC",
                params_from_iter(vec![text(item_id)]),
            )
            .await?;

        while let Some(row) = rows.next().await? {
            files.push(File {
                id: get_text(&row, 0)?,
                path: get_text(&row, 1)?,
                kind: FileKind::parse(&get_text(&row, 2)?),
                format_id: get_opt_text(&row, 3)?,
                bytes: get_opt_int(&row, 4)?,
            });
        }
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::new_id;

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

    /// Deliberately driven by pollster, NOT tokio: passing proves turso's futures
    /// are runtime-agnostic and will therefore run on gpui's BackgroundExecutor.
    #[test]
    fn job_round_trips_through_turso() {
        pollster::block_on(async {
            let dir = std::env::temp_dir().join(new_id("rustydlp-test"));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("library.db");

            let store = Store::open(path.to_str().unwrap()).await.unwrap();
            let job = sample_job();
            store.save_job(&job).await.unwrap();

            let loaded = store.load_jobs().await.unwrap();
            assert_eq!(loaded.len(), 1, "expected exactly one job");
            assert_eq!(loaded[0], job, "job did not survive the round trip");

            // Saving again must not duplicate rows or orphan children.
            store.save_job(&job).await.unwrap();
            let again = store.load_jobs().await.unwrap();
            assert_eq!(again.len(), 1, "re-saving duplicated the job");
            assert_eq!(again[0].items.len(), 2);
            assert_eq!(again[0].items[0].files.len(), 2);

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    async fn temp_store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(new_id("rustydlp-test"));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(dir.join("library.db").to_str().unwrap())
            .await
            .unwrap();
        (store, dir)
    }

    #[test]
    fn presets_round_trip_and_only_one_stays_default() {
        pollster::block_on(async {
            let (store, dir) = temp_store().await;

            for p in Preset::seeds("D:/Videos") {
                store.save_preset(&p).await.unwrap();
            }

            let loaded = store.load_presets().await.unwrap();
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
            store.save_preset(&mp3).await.unwrap();

            let after = store.load_presets().await.unwrap();
            assert_eq!(after.iter().filter(|p| p.is_default).count(), 1);
            assert!(after.iter().find(|p| p.name == "MP3 audio").unwrap().is_default);
            assert!(!after.iter().find(|p| p.name == "Best (1080p mp4)").unwrap().is_default);

            store.delete_preset("M4A audio").await.unwrap();
            assert_eq!(store.load_presets().await.unwrap().len(), 3);

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    #[test]
    fn settings_round_trip_and_overwrite() {
        pollster::block_on(async {
            let (store, dir) = temp_store().await;

            assert_eq!(store.get_setting("download_dir").await.unwrap(), None);
            store.set_setting("download_dir", "D:/Videos").await.unwrap();
            assert_eq!(
                store.get_setting("download_dir").await.unwrap().as_deref(),
                Some("D:/Videos")
            );
            store.set_setting("download_dir", "E:/Other").await.unwrap();
            assert_eq!(
                store.get_setting("download_dir").await.unwrap().as_deref(),
                Some("E:/Other"),
                "second write must overwrite, not duplicate"
            );

            let _ = std::fs::remove_dir_all(&dir);
        });
    }

    /// A shrinking item list must not leave orphaned items/files behind.
    #[test]
    fn resaving_with_fewer_items_prunes_children() {
        pollster::block_on(async {
            let dir = std::env::temp_dir().join(new_id("rustydlp-test"));
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("library.db");

            let store = Store::open(path.to_str().unwrap()).await.unwrap();
            let mut job = sample_job();
            store.save_job(&job).await.unwrap();

            job.items.truncate(1);
            job.items[0].files.truncate(1);
            store.save_job(&job).await.unwrap();

            let loaded = store.load_jobs().await.unwrap();
            assert_eq!(loaded[0].items.len(), 1);
            assert_eq!(loaded[0].items[0].files.len(), 1);
            assert_eq!(loaded[0], job);

            let _ = std::fs::remove_dir_all(&dir);
        });
    }
}
