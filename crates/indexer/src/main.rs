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
    embed::{self, Device, Embedder, Weights},
    extract::{self, Kind},
    state::{Seen, State, Status},
    store::Store,
    vectors,
};
use tantivy::IndexWriter;
use windows::Win32::{
    Graphics::Dxgi::{CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, IDXGIFactory1},
    System::Threading::{
        GetCurrentProcess, IDLE_PRIORITY_CLASS, PROCESS_MODE_BACKGROUND_BEGIN,
        PROCESS_MODE_BACKGROUND_END, SetPriorityClass,
    },
};

const EXTRACT_TIMEOUT: Duration = Duration::from_secs(30);
/// Commit at least this often during a long pass, so search sees progress.
const COMMIT_EVERY: Duration = Duration::from_secs(5);
/// At least this many passages to embed make a bulk run, which may use the GPU.
const BULK: usize = 64;
/// Passages embedded per model call: the GPU's fixed batch.
const BATCH: usize = embed::GPU_BATCH;
const SAVE_VECTORS_EVERY: Duration = Duration::from_secs(30);
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
    if content::is_paused() {
        tracing::info!("content indexing is paused");
        return Ok(ExitCode::SUCCESS);
    }
    let state = State::open(&content::state_path())?;
    let rebuild = content::take_rebuild();
    if rebuild {
        tracing::info!("rebuilding the content index from scratch");
        writer.delete_all_documents()?;
        writer.commit()?;
        state.clear()?;
    }
    for path in state.quarantine_unfinished()? {
        tracing::warn!("quarantined {path}: a run stopped while reading it");
    }
    let folders = common::content::folders();
    // Read at each run, so a changed exclusion or battery setting applies to the next pass.
    let config = common::Config::load().unwrap_or_else(|e| {
        tracing::warn!("config.toml not applied: {e}");
        common::Config::default()
    });
    let embed = embed::available(Weights::Full);
    if !embed {
        tracing::info!("no embedding model installed; meaning search stays off");
    }
    let mut run = Run {
        store: &store,
        writer: &mut writer,
        state: &state,
        indexed: 0,
        removed: 0,
        committed: Instant::now(),
        embed,
        pending: Vec::new(),
        model: None,
        embedded: 0,
        // A rebuild leaves no vectors, which the app must see too.
        vectors_changed: rebuild,
        files_seen: 0,
        paused: false,
        reported: Instant::now(),
        config,
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
    if outcome.is_ok() {
        run.embed_pending();
    }
    run.save_vectors();
    content::write_progress(None);
    tracing::info!(
        "indexed {} files, embedded {} passages, removed {} in {:?}",
        run.indexed,
        run.embedded,
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
    /// Whether the embedding model is installed.
    embed: bool,
    /// Passages (file, start line, text) still to embed, in file order.
    pending: Vec<(String, u32, String)>,
    /// Loaded at the first passage to embed, on the GPU for a bulk run.
    model: Option<Embedder>,
    embedded: usize,
    vectors_changed: bool,
    files_seen: u64,
    /// Set once the user pauses indexing: no more files are read and nothing is dropped.
    paused: bool,
    reported: Instant,
    /// For `exclude` and `embed_on_battery`.
    config: common::Config,
}

impl Run<'_> {
    /// Every content folder, then drops what's gone or no longer chosen.
    fn full(&mut self, folders: &[PathBuf]) -> Result<(), Hung> {
        let mut seen = HashSet::new();
        for folder in folders {
            self.walk(folder, &mut seen)?;
        }
        // A paused walk didn't see everything, so nothing unseen is gone.
        if self.paused {
            return Ok(());
        }
        for path in self.state.paths().unwrap_or_default() {
            if !seen.contains(&path) {
                self.remove(&path);
            }
        }
        Ok(())
    }

    /// Writes progress about once a second, and notices a pause.
    fn report(&mut self, phase: &str, done: u64, total: Option<u64>) {
        if self.reported.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.reported = Instant::now();
        self.paused |= content::is_paused();
        let phase = phase.to_string();
        content::write_progress(Some(&content::Progress { phase, done, total }));
    }

    /// Paths the journal or watcher reported: files are (re)read or dropped; folders (moved or
    /// renamed in) are walked, and what used to be under them but no longer is gets dropped.
    fn changed(&mut self, folders: &[PathBuf], paths: &[PathBuf]) -> Result<(), Hung> {
        for path in paths {
            if !inside(folders, path) {
                continue;
            }
            if path.exists()
                && (!eligible(path) || self.config.excludes(&path.display().to_string()))
            {
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
        if self.config.excludes(&root.display().to_string()) {
            return Ok(());
        }
        for entry in walker(root, &self.config.exclude).build().flatten() {
            if entry.file_type().is_some_and(|t| t.is_file()) {
                seen.insert(entry.path().display().to_string());
                self.file(entry.path())?;
            }
        }
        Ok(())
    }

    fn file(&mut self, path: &Path) -> Result<(), Hung> {
        self.files_seen += 1;
        self.report("reading", self.files_seen, None);
        if self.paused {
            return Ok(());
        }
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
        let needs_vectors = |state: &State, before: &Seen| {
            self.embed && before.status == Status::Indexed && !state.embedded(&key).unwrap_or(true)
        };
        if let Ok(Some(before)) = self.state.get(&key)
            && before.size == now.size
            && before.modified == now.modified
            && !needs_vectors(self.state, &before)
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
                        // Old vectors describe old text.
                        let _ = self.state.set_vectors(&key, &[]);
                        self.vectors_changed = true;
                        if self.embed {
                            self.pending.extend(
                                passages.into_iter().map(|c| (key.clone(), c.line, c.text)),
                            );
                        }
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
        self.vectors_changed = true;
    }

    /// Embeds the queued passages: a bulk run on the discrete GPU if there is one, otherwise
    /// on the CPU. On battery a bulk run waits for mains power; its files stay without vectors
    /// and the next run picks them up.
    fn embed_pending(&mut self) {
        if self.pending.is_empty() || self.paused {
            return;
        }
        let bulk = self.pending.len() >= BULK;
        if bulk && !self.config.embed_on_battery && common::on_battery() {
            tracing::info!(
                "on battery: {} passages wait for mains power",
                self.pending.len()
            );
            self.pending.clear();
            return;
        }
        if self.model.is_none() {
            let device = bulk
                .then(discrete_adapter)
                .flatten()
                .map_or(Device::Cpu, Device::DirectMl);
            let loaded = Embedder::load(Weights::Full, device).or_else(|e| {
                tracing::warn!("{device:?} unavailable ({e}); embedding on the CPU");
                Embedder::load(Weights::Full, Device::Cpu)
            });
            match loaded {
                Ok(model) => {
                    tracing::info!("embedding {} passages on {device:?}", self.pending.len());
                    self.model = Some(model);
                }
                Err(e) => {
                    tracing::error!("no embedding model: {e}");
                    self.pending.clear();
                    return;
                }
            }
        }
        // Background mode starves the model of CPU (on the GPU 7 passages a second instead of
        // 18, on the CPU 4 passages took 11 s); idle priority still yields to everything else.
        let process = unsafe { GetCurrentProcess() };
        let _ = unsafe { SetPriorityClass(process, PROCESS_MODE_BACKGROUND_END) };
        let _ = unsafe { SetPriorityClass(process, IDLE_PRIORITY_CLASS) };
        let pending = std::mem::take(&mut self.pending);
        let mut saved = Instant::now();
        // Batches span files, so small files don't send the GPU padding; a file's vectors are
        // stored once all of its passages are done, and not at all if any failed.
        let mut file: Option<(&str, Vec<_>, bool)> = None;
        let mut done = 0;
        for batch in pending.chunks(BATCH) {
            let texts: Vec<&str> = batch.iter().map(|p| p.2.as_str()).collect();
            let model = self.model.as_mut().expect("loaded above");
            let vectors = model
                .embed(&texts)
                .inspect_err(|e| tracing::warn!("embedding {}: {e}", batch[0].0))
                .ok();
            for (i, (path, line, _)) in batch.iter().enumerate() {
                if file.as_ref().is_none_or(|f| f.0 != path) {
                    if let Some((path, rows, ok)) = file.take() {
                        self.store_vectors(path, &rows, ok);
                    }
                    file = Some((path, Vec::new(), true));
                }
                let current = file.as_mut().expect("set above");
                match &vectors {
                    Some(v) => {
                        let (scale, data) = vectors::quantize(&v[i]);
                        current.1.push((*line, scale, data));
                    }
                    None => current.2 = false,
                }
            }
            done += batch.len();
            if saved.elapsed() > SAVE_VECTORS_EVERY {
                self.save_vectors();
                saved = Instant::now();
            }
            self.report("embedding", done as u64, Some(pending.len() as u64));
            // The file in progress and those not reached keep no vectors; the next run embeds them.
            if self.paused {
                file = None;
                break;
            }
        }
        if let Some((path, rows, ok)) = file {
            self.store_vectors(path, &rows, ok);
        }
        // Frees the GPU before the process exits.
        self.model = None;
        let _ = unsafe { SetPriorityClass(process, PROCESS_MODE_BACKGROUND_BEGIN) };
    }

    fn store_vectors(&mut self, path: &str, rows: &[(u32, f32, Vec<i8>)], ok: bool) {
        if ok && !rows.is_empty() && self.state.set_vectors(path, rows).is_ok() {
            self.embedded += rows.len();
            self.vectors_changed = true;
        }
    }

    /// Writes every vector to the file the app searches, if any changed.
    fn save_vectors(&mut self) {
        if !self.vectors_changed {
            return;
        }
        let saved = self
            .state
            .vectors()
            .map_err(|e| e.to_string())
            .and_then(|v| v.save(&content::vectors_path()).map_err(|e| e.to_string()));
        match saved {
            Ok(()) => self.vectors_changed = false,
            Err(e) => tracing::error!("saving vectors: {e}"),
        }
    }
}

/// The DXGI index (DirectML's device id) of the adapter with the most dedicated memory, if it
/// has at least 2 GB: a discrete GPU, not the integrated one or a software adapter.
fn discrete_adapter() -> Option<i32> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1().ok()? };
    let mut best: Option<(i32, usize)> = None;
    for i in 0.. {
        let Ok(adapter) = (unsafe { factory.EnumAdapters1(i) }) else {
            break;
        };
        let Ok(desc) = (unsafe { adapter.GetDesc1() }) else {
            continue;
        };
        let software = desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0;
        let memory = desc.DedicatedVideoMemory;
        if !software && memory >= 2 << 30 && best.is_none_or(|(_, m)| memory > m) {
            best = Some((i as i32, memory));
        }
    }
    best.map(|(i, _)| i)
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
/// not), hidden entries, junk folders and what config.toml leaves out are skipped, and links
/// are never followed.
fn walker(root: &Path, exclude: &[String]) -> ignore::WalkBuilder {
    let config = common::Config {
        exclude: exclude.to_vec(),
        ..Default::default()
    };
    let mut builder = ignore::WalkBuilder::new(root);
    builder
        .follow_links(false)
        .require_git(false)
        .git_global(false)
        .filter_entry(move |e| {
            let name = e.file_name().to_string_lossy().to_lowercase();
            let junk = e.file_type().is_some_and(|t| t.is_dir()) && JUNK.contains(&name.as_str());
            !junk && !config.excludes(&e.path().display().to_string())
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
    #[test]
    fn the_walk_leaves_out_excluded_names_and_folders() {
        let root = std::env::temp_dir().join(format!("seekr-exclude-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for dir in ["keep", "private", r"keep\drafts"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("a.txt"), "x").unwrap();
        }
        let exclude = [
            "private".to_string(),
            root.join(r"keep\drafts").display().to_string(),
        ];
        let found: Vec<String> = walker(&root, &exclude)
            .build()
            .flatten()
            .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
            .map(|e| e.path().strip_prefix(&root).unwrap().display().to_string())
            .collect();
        assert_eq!(found, [r"keep\a.txt"]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
