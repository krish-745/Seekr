//! `seekr-indexer`: reads the files in the content-search folders into the full-text index, at
//! background priority, then exits. The app runs it at launch (a full pass that also drops files
//! that are gone or no longer chosen) and again with the changed paths on stdin (`--stdin`).
//!
//! A file being read is marked in the state database first. If the process dies or hangs on it
//! (`EXTRACT_TIMEOUT`), the mark survives and the next run quarantines the file until it changes.

use std::{
    collections::HashSet,
    io::{self, BufRead},
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::mpsc,
    time::{Duration, Instant},
};

use content::{
    chunk::chunks,
    extract::{self, Kind},
    state::{Seen, State, Status},
    store::Store,
};
use tantivy::IndexWriter;
use windows::Win32::System::Threading::{
    GetCurrentProcess, PROCESS_MODE_BACKGROUND_BEGIN, SetPriorityClass,
};

const EXTRACT_TIMEOUT: Duration = Duration::from_secs(30);
/// Commit at least this often during a long pass, so search sees progress.
const COMMIT_EVERY: Duration = Duration::from_secs(5);
/// Exit code for a file that hung; the app just starts another run later.
const HUNG: u8 = 3;
const RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
const OFFLINE: u32 = 0x1000;
/// Folders of generated or installed files that nobody searches by content.
const JUNK: [&str; 9] = [
    "node_modules",
    "__pycache__",
    "venv",
    "site-packages",
    "target",
    "build",
    "dist",
    "obj",
    "$recycle.bin",
];

fn main() -> ExitCode {
    let _log = common::init_logging("seekr-indexer").ok();
    // Lowest CPU and I/O priority for the whole process, so typing elsewhere never waits on it.
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), PROCESS_MODE_BACKGROUND_BEGIN) };
    match run() {
        Ok(code) => code,
        Err(e) => {
            tracing::error!("indexer failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let store = Store::open(&content::index_dir())?;
    let mut writer = match store.writer() {
        Ok(w) => w,
        Err(tantivy::TantivyError::LockFailure(..)) => {
            tracing::info!("another indexer is running");
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => return Err(e.into()),
    };
    let state = State::open(&content::state_path())?;
    for path in state.quarantine_unfinished()? {
        tracing::warn!("quarantined {path}: a run stopped while reading it");
    }
    let folders = common::content::folders();
    let mut run = Run {
        store: &store,
        writer: &mut writer,
        state: &state,
        indexed: 0,
        removed: 0,
        committed: Instant::now(),
    };
    let started = Instant::now();
    let outcome = if std::env::args().any(|a| a == "--stdin") {
        let paths: Vec<PathBuf> = io::stdin()
            .lock()
            .lines()
            .map_while(Result::ok)
            .filter(|l| !l.trim().is_empty())
            .map(PathBuf::from)
            .collect();
        run.changed(&folders, &paths)
    } else {
        run.full(&folders)
    };
    run.writer.commit()?;
    tracing::info!(
        "indexed {} files, removed {} in {:?}",
        run.indexed,
        run.removed,
        started.elapsed()
    );
    Ok(match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(Hung(path)) => {
            tracing::warn!("{} took over {EXTRACT_TIMEOUT:?}; exiting", path.display());
            ExitCode::from(HUNG)
        }
    })
}

struct Hung(PathBuf);

struct Run<'a> {
    store: &'a Store,
    writer: &'a mut IndexWriter,
    state: &'a State,
    indexed: usize,
    removed: usize,
    committed: Instant,
}

impl Run<'_> {
    /// Every content folder, then drops what's gone or no longer chosen.
    fn full(&mut self, folders: &[PathBuf]) -> Result<(), Hung> {
        let mut seen = HashSet::new();
        for folder in folders {
            self.walk(folder, &mut seen)?;
        }
        for path in self.state.paths().unwrap_or_default() {
            if !seen.contains(&path) {
                self.remove(&path);
            }
        }
        Ok(())
    }

    /// Paths the journal or watcher reported: files are (re)read or dropped; folders (moved or
    /// renamed in) are walked, and what used to be under them but no longer is gets dropped.
    fn changed(&mut self, folders: &[PathBuf], paths: &[PathBuf]) -> Result<(), Hung> {
        for path in paths {
            if !inside(folders, path) {
                continue;
            }
            if path.exists() && !eligible(path) {
                continue;
            }
            if path.is_dir() {
                let mut seen = HashSet::new();
                self.walk(path, &mut seen)?;
                let prefix = format!("{}\\", path.display()).to_lowercase();
                for known in self.state.paths().unwrap_or_default() {
                    if known.to_lowercase().starts_with(&prefix) && !seen.contains(&known) {
                        self.remove(&known);
                    }
                }
            } else if path.exists() {
                self.file(path)?;
            } else {
                // Gone: the file itself, or a folder that held indexed files.
                let text = path.display().to_string();
                let prefix = format!("{text}\\").to_lowercase();
                for known in self.state.paths().unwrap_or_default() {
                    if known.eq_ignore_ascii_case(&text)
                        || known.to_lowercase().starts_with(&prefix)
                    {
                        self.remove(&known);
                    }
                }
            }
        }
        Ok(())
    }

    /// Indexes every eligible file under `root`, honouring .gitignore and skipping hidden and
    /// junk folders; links and junctions are never followed.
    fn walk(&mut self, root: &Path, seen: &mut HashSet<String>) -> Result<(), Hung> {
        for entry in walker(root).build().flatten() {
            if entry.file_type().is_some_and(|t| t.is_file()) {
                seen.insert(entry.path().display().to_string());
                self.file(entry.path())?;
            }
        }
        Ok(())
    }

    fn file(&mut self, path: &Path) -> Result<(), Hung> {
        let Some(kind) = Kind::of(path) else {
            return Ok(());
        };
        let Ok(meta) = path.symlink_metadata() else {
            return Ok(());
        };
        // Reading an online-only OneDrive file would download it.
        if meta.file_attributes() & (RECALL_ON_DATA_ACCESS | OFFLINE) != 0
            || meta.len() > kind.max_bytes()
        {
            return Ok(());
        }
        let key = path.display().to_string();
        let now = Seen {
            size: meta.len(),
            modified: meta.last_write_time() as i64,
            status: Status::Working,
        };
        if let Ok(Some(before)) = self.state.get(&key)
            && before.size == now.size
            && before.modified == now.modified
        {
            // Unchanged, including files quarantined in this version.
            return Ok(());
        }
        if let Err(e) = self.state.set(&key, now, None) {
            tracing::warn!("state for {key}: {e}");
            return Ok(());
        }
        let text = extract_in_time(path, kind)?;
        let (status, note) = match text {
            Ok(Some(text)) if !text.trim().is_empty() => {
                let passages = chunks(&text, kind == Kind::Markdown);
                match self.store.replace(self.writer, &key, &passages) {
                    Ok(()) => {
                        self.indexed += 1;
                        (Status::Indexed, None)
                    }
                    Err(e) => (Status::Skipped, Some(e.to_string())),
                }
            }
            Ok(_) => (Status::Skipped, Some("no text".to_string())),
            Err(e) => (Status::Skipped, Some(e.to_string())),
        };
        if status != Status::Indexed {
            self.store.remove(self.writer, &key);
        }
        let _ = self
            .state
            .set(&key, Seen { status, ..now }, note.as_deref());
        if self.committed.elapsed() > COMMIT_EVERY {
            if let Err(e) = self.writer.commit() {
                tracing::warn!("commit: {e}");
            }
            self.committed = Instant::now();
        }
        Ok(())
    }

    fn remove(&mut self, path: &str) {
        self.store.remove(self.writer, path);
        let _ = self.state.remove(path);
        self.removed += 1;
    }
}

/// Extraction on a thread of its own, so a file that hangs the parser can be given up on.
fn extract_in_time(path: &Path, kind: Kind) -> Result<io::Result<Option<String>>, Hung> {
    let (tx, rx) = mpsc::channel();
    let owned = path.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(extract::extract(&owned, kind));
    });
    rx.recv_timeout(EXTRACT_TIMEOUT)
        .map_err(|_| Hung(path.to_path_buf()))
}

fn inside(folders: &[PathBuf], path: &Path) -> bool {
    let p = path.display().to_string().to_lowercase();
    folders.iter().any(|f| {
        let f = f
            .display()
            .to_string()
            .trim_end_matches('\\')
            .to_lowercase();
        p == f || p.starts_with(&format!("{f}\\"))
    })
}

/// The walk every pass uses: .gitignore files in the folder and above it apply (git repo or
/// not), hidden entries and junk folders are skipped, and links are never followed.
fn walker(root: &Path) -> ignore::WalkBuilder {
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .follow_links(false)
        .require_git(false)
        .git_global(false)
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            !(e.file_type().is_some_and(|t| t.is_dir()) && JUNK.contains(&name.as_str()))
        });
    builder
}

/// Whether a full walk would reach `path`: no hidden or junk folder on the way, and no
/// .gitignore in a folder above it ignores it or one of its parents.
fn eligible(path: &Path) -> bool {
    if has_junk(path) {
        return false;
    }
    let is_dir = path.is_dir();
    !path.ancestors().skip(1).any(|dir| {
        let file = dir.join(".gitignore");
        file.is_file()
            && ignore::gitignore::Gitignore::new(&file)
                .0
                .matched_path_or_any_parents(path, is_dir)
                .is_ignore()
    })
}

/// A hidden or junk name anywhere in `path`, the last part included.
fn has_junk(path: &Path) -> bool {
    path.components().any(|c| {
        let name = c.as_os_str().to_string_lossy().to_lowercase();
        (name.starts_with('.') && name != "." && name != "..") || JUNK.contains(&name.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_contain_their_paths_only() {
        let folders = [PathBuf::from(r"C:\Users\me\Documents")];
        assert!(inside(&folders, Path::new(r"c:\users\me\documents\a.txt")));
        assert!(!inside(
            &folders,
            Path::new(r"C:\Users\me\DocumentsOld\a.txt")
        ));
        assert!(has_junk(Path::new(r"C:\w\node_modules\x\a.js")));
        assert!(has_junk(Path::new(r"C:\w\.git\config.txt")));
        assert!(
            has_junk(Path::new(r"C:\w\target")),
            "the folder itself counts"
        );
        assert!(!has_junk(Path::new(r"C:\w\src\main.rs")));
    }

    #[test]
    fn gitignore_above_a_changed_path_applies() {
        let root = std::env::temp_dir().join(format!("seekr-eligible-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(r"gen\deep")).unwrap();
        std::fs::write(root.join(".gitignore"), "gen/\n*.log\n").unwrap();
        std::fs::write(root.join(r"gen\deep\a.txt"), "x").unwrap();
        std::fs::write(root.join("b.log"), "x").unwrap();
        std::fs::write(root.join("c.txt"), "x").unwrap();
        assert!(!eligible(&root.join(r"gen\deep")), "ignored folder");
        assert!(
            !eligible(&root.join(r"gen\deep\a.txt")),
            "inside an ignored folder"
        );
        assert!(!eligible(&root.join("b.log")));
        assert!(eligible(&root.join("c.txt")));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
