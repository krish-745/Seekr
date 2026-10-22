//! What the indexer last did with each file, so a run only reads files that changed and a file
//! that crashed or hung a run is skipped (quarantined) until it changes again.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

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
                 status TEXT NOT NULL, note TEXT);",
        )?;
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
        Ok(())
    }

    pub fn paths(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self.0.prepare("SELECT path FROM files")?;
        stmt.query_map([], |r| r.get(0))?.collect()
    }

    /// Marks every file a previous run died on as quarantined, and returns them.
    pub fn quarantine_unfinished(&self) -> rusqlite::Result<Vec<String>> {
        let mut stmt = self
            .0
            .prepare("SELECT path FROM files WHERE status = 'working'")?;
        let paths: Vec<String> = stmt
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        self.0.execute(
            "UPDATE files SET status = 'quarantined', note = 'a run stopped while reading it'
             WHERE status = 'working'",
            [],
        )?;
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(state.quarantine_unfinished().unwrap(), ["b"]);
        assert_eq!(state.get("b").unwrap().unwrap().status, Status::Quarantined);
        state.remove("a").unwrap();
        assert_eq!(state.paths().unwrap(), ["b"]);
        assert_eq!(state.get("missing").unwrap(), None);
    }
}
