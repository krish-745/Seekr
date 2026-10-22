//! What the indexer last did with each file, so a run only reads files that changed and a file
//! that crashed or hung a run is skipped (quarantined) until it changes again.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::vectors::Vectors;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Being read now; still set at the next start means that run died on this file.
    Working,
    Indexed,
    /// Nothing to index: binary, empty or unreadable.
    Skipped,
    Quarantined,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::Indexed => "indexed",
            Status::Skipped => "skipped",
            Status::Quarantined => "quarantined",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "working" => Status::Working,
            "indexed" => Status::Indexed,
            "quarantined" => Status::Quarantined,
            _ => Status::Skipped,
        }
    }
}

/// The note on a file being read a second time after a run stopped while reading it.
pub const RETRIED: &str = "read again after a run stopped";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seen {
    pub size: u64,
    /// FILETIME of the last write.
    pub modified: i64,
    pub status: Status,
}

pub struct State(Connection);

impl State {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::with(Connection::open(path)?)
    }

    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS files (
                 path TEXT PRIMARY KEY, size INTEGER NOT NULL, modified INTEGER NOT NULL,
                 status TEXT NOT NULL, note TEXT);
             CREATE TABLE IF NOT EXISTS vectors (
                 path TEXT NOT NULL, line INTEGER NOT NULL, scale REAL NOT NULL,
                 data BLOB NOT NULL);
             CREATE INDEX IF NOT EXISTS vectors_path ON vectors (path);",
        )?;
        // Added with meaning search: whether a file's passages have vectors yet.
        let has_column = conn.prepare("SELECT embedded FROM files LIMIT 0").is_ok();
        if !has_column {
            conn.execute_batch("ALTER TABLE files ADD COLUMN embedded INTEGER NOT NULL DEFAULT 0")?;
        }
        Ok(Self(conn))
    }

    pub fn get(&self, path: &str) -> rusqlite::Result<Option<Seen>> {
        self.0
            .query_row(
                "SELECT size, modified, status FROM files WHERE path = ?1",
                [path],
                |r| {
                    Ok(Seen {
                        size: r.get::<_, i64>(0)? as u64,
                        modified: r.get(1)?,
                        status: Status::parse(&r.get::<_, String>(2)?),
                    })
                },
            )
            .optional()
    }

    pub fn set(&self, path: &str, seen: Seen, note: Option<&str>) -> rusqlite::Result<()> {
        self.0.execute(
            "INSERT INTO files (path, size, modified, status, note) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(path) DO UPDATE SET size = ?2, modified = ?3, status = ?4, note = ?5",
            params![
                path,
                seen.size as i64,
                seen.modified,
                seen.status.as_str(),
                note
            ],
        )?;
        Ok(())
    }

    pub fn remove(&self, path: &str) -> rusqlite::Result<()> {
        self.0
            .execute("DELETE FROM files WHERE path = ?1", [path])?;
        self.0
            .execute("DELETE FROM vectors WHERE path = ?1", [path])?;
        Ok(())
    }

    /// Whether `path` has vectors for its current text.
    pub fn embedded(&self, path: &str) -> rusqlite::Result<bool> {
        Ok(self
            .0
            .query_row("SELECT embedded FROM files WHERE path = ?1", [path], |r| {
                r.get::<_, i64>(0)
            })
            .optional()?
            == Some(1))
    }

    /// Replaces the vectors of `path` (start line, scale, int8 data) and marks it embedded;
    /// an empty list just clears them.
    pub fn set_vectors(&self, path: &str, rows: &[(u32, f32, Vec<i8>)]) -> rusqlite::Result<()> {
        let tx = self.0.unchecked_transaction()?;
        tx.execute("DELETE FROM vectors WHERE path = ?1", [path])?;
        for (line, scale, data) in rows {
            let bytes: Vec<u8> = data.iter().map(|&b| b as u8).collect();
            tx.execute(
                "INSERT INTO vectors (path, line, scale, data) VALUES (?1, ?2, ?3, ?4)",
                params![path, line, f64::from(*scale), bytes],
            )?;
        }
        tx.execute(
            "UPDATE files SET embedded = ?2 WHERE path = ?1",
            params![path, i64::from(!rows.is_empty())],
        )?;
        tx.commit()
    }

    /// Every stored vector, grouped by file, for the app's meaning search.
    pub fn vectors(&self) -> rusqlite::Result<Vectors> {
        let mut stmt = self
            .0
            .prepare("SELECT path, line, scale, data FROM vectors ORDER BY path, rowid")?;
        let mut out = Vectors::default();
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let data: Vec<u8> = r.get(3)?;
            let data: Vec<i8> = data.into_iter().map(|b| b as i8).collect();
            out.push(
                &r.get::<_, String>(0)?,
                r.get(1)?,
                r.get::<_, f64>(2)? as f32,
                &data,
            );
        }
        Ok(out)
    }

    pub fn paths(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.0.prepare("SELECT path FROM files")?;
        stmt.query_map([], |r| r.get(0))?.collect()
    }

    /// Forgets every file and vector, for a rebuild.
    pub fn clear(&self) -> rusqlite::Result<()> {
        self.0
            .execute_batch("DELETE FROM files; DELETE FROM vectors;")
    }

    /// How many files have each status, and how many have meaning vectors.
    pub fn counts(&self) -> rusqlite::Result<(Vec<(Status, u64)>, u64)> {
        let mut stmt = self
            .0
            .prepare("SELECT status, COUNT(*) FROM files GROUP BY status ORDER BY status")?;
        let by_status = stmt
            .query_map([], |r| {
                Ok((
                    Status::parse(&r.get::<_, String>(0)?),
                    r.get::<_, i64>(1)? as u64,
                ))
            })?
            .collect::<Result<_, _>>()?;
        let embedded: i64 =
            self.0
                .query_row("SELECT COUNT(*) FROM files WHERE embedded = 1", [], |r| {
                    r.get(0)
                })?;
        Ok((by_status, embedded as u64))
    }

    /// Quarantined files and why.
    pub fn quarantined(&self) -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = self.0.prepare(
            "SELECT path, COALESCE(note, '') FROM files WHERE status = 'quarantined' ORDER BY path",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect()
    }

    /// Quarantines the files a run died on twice, and returns them. A run also dies when it is
    /// killed (Seekr quitting, Windows shutting down), so a file left mid-read once is read
    /// again first: it stays `Working`, and the indexer marks the retry with [`RETRIED`].
    pub fn quarantine_unfinished(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .0
            .prepare("SELECT path FROM files WHERE status = 'working' AND note = ?1")?;
        let paths: Vec<String> = stmt
            .query_map([RETRIED], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        self.0.execute(
            "UPDATE files SET status = 'quarantined', note = 'a run stopped while reading it twice'
             WHERE status = 'working' AND note = ?1",
            [RETRIED],
        )?;
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_lists_quarantine_and_clears() {
        let state = State::in_memory().unwrap();
        let seen = |status| Seen {
            size: 1,
            modified: 1,
            status,
        };
        state.set("a", seen(Status::Indexed), None).unwrap();
        state.set("b", seen(Status::Indexed), None).unwrap();
        state
            .set("c", seen(Status::Quarantined), Some("hung"))
            .unwrap();
        state.set_vectors("a", &[(1, 0.5, vec![1, 2])]).unwrap();
        let (by_status, embedded) = state.counts().unwrap();
        assert_eq!(by_status, [(Status::Indexed, 2), (Status::Quarantined, 1)]);
        assert_eq!(embedded, 1);
        assert_eq!(
            state.quarantined().unwrap(),
            [("c".to_string(), "hung".to_string())]
        );
        state.clear().unwrap();
        assert!(state.paths().unwrap().is_empty());
        assert!(state.vectors().unwrap().is_empty());
    }

    #[test]
    fn remembers_files_and_quarantines_unfinished_ones() {
        let state = State::in_memory().unwrap();
        let seen = |status| Seen {
            size: 10,
            modified: 5,
            status,
        };
        state.set("a", seen(Status::Indexed), None).unwrap();
        state.set("b", seen(Status::Working), None).unwrap();
        assert_eq!(state.get("a").unwrap(), Some(seen(Status::Indexed)));
        // Left mid-read once: read again, not quarantined.
        assert!(state.quarantine_unfinished().unwrap().is_empty());
        assert_eq!(state.get("b").unwrap().unwrap().status, Status::Working);
        // Left mid-read on its retry too: quarantined.
        state
            .set("b", seen(Status::Working), Some(RETRIED))
            .unwrap();
        assert_eq!(state.quarantine_unfinished().unwrap(), ["b"]);
        assert_eq!(state.get("b").unwrap().unwrap().status, Status::Quarantined);
        assert!(!state.embedded("a").unwrap());
        let v = vec![1i8; crate::embed::DIMS];
        state
            .set_vectors("a", &[(1, 0.5, v.clone()), (9, 0.5, v)])
            .unwrap();
        assert!(state.embedded("a").unwrap());
        assert_eq!(state.vectors().unwrap().len(), 2);
        state
            .set(
                "a",
                Seen {
                    size: 11,
                    ..seen(Status::Indexed)
                },
                None,
            )
            .unwrap();
        state.remove("a").unwrap();
        assert!(
            state.vectors().unwrap().is_empty(),
            "vectors go with the file"
        );
        assert_eq!(state.paths().unwrap(), ["b"]);
        assert_eq!(state.get("missing").unwrap(), None);
    }
}
