//! Runs `seekr-indexer.exe` (shipped next to Seekr) in the background: a full pass at launch and
//! whenever the content folders change, and the changed paths inside those folders as the journal
//! or watcher reports them. Only one run at a time; changes that arrive meanwhile wait for the
//! next one.

use std::{
    io::Write,
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Condvar, Mutex},
    time::Duration,
};

use tauri::{AppHandle, Manager};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Saving a file writes it several times; gather the burst into one run.
const SETTLE: Duration = Duration::from_millis(400);

#[derive(Default)]
pub struct Indexer {
    work: Mutex<Work>,
    wake: Condvar,
}

#[derive(Default)]
struct Work {
    full: bool,
    paths: Vec<PathBuf>,
    /// The content folders, lower case with no trailing backslash, for quick filtering.
    folders: Vec<String>,
}

impl Indexer {
    /// Queues a full pass, rereading which folders are chosen.
    pub fn refresh(&self) {
        let folders = common::content::folders()
            .iter()
            .map(|f| {
                f.display()
                    .to_string()
                    .trim_end_matches('\\')
                    .to_lowercase()
            })
            .collect();
        let mut work = self.work.lock().unwrap();
        work.folders = folders;
        work.full = true;
        self.wake.notify_one();
    }

    /// Queues the paths that lie inside a content folder.
    pub fn changed(&self, paths: impl IntoIterator<Item = String>) {
        let mut work = self.work.lock().unwrap();
        let before = work.paths.len();
        for path in paths {
            let lower = path.to_lowercase();
            let inside = work.folders.iter().any(|f| {
                lower.len() > f.len()
                    && lower.starts_with(f.as_str())
                    && lower.as_bytes()[f.len()] == b'\\'
            });
            if inside {
                work.paths.push(PathBuf::from(path));
            }
        }
        if work.paths.len() > before {
            self.wake.notify_one();
        }
    }
}

pub fn start(app: &AppHandle) {
    let Some(exe) = std::env::current_exe()
        .ok()
        .and_then(|e| Some(e.parent()?.join("seekr-indexer.exe")))
        .filter(|e| e.exists())
    else {
        tracing::warn!("seekr-indexer.exe isn't next to Seekr; content search stays empty");
        return;
    };
    let app = app.clone();
    app.state::<Indexer>().refresh();
    let spawned = std::thread::Builder::new()
        .name("indexer".into())
        .spawn(move || supervise(&app.state::<Indexer>(), &exe));
    if let Err(e) = spawned {
        tracing::error!("no content indexing: {e}");
    }
}

fn supervise(indexer: &Indexer, exe: &Path) {
    loop {
        {
            let mut work = indexer.work.lock().unwrap();
            while !work.full && work.paths.is_empty() {
                work = indexer.wake.wait(work).unwrap();
            }
        }
        std::thread::sleep(SETTLE);
        let (full, mut paths) = {
            let mut work = indexer.work.lock().unwrap();
            (
                std::mem::take(&mut work.full),
                std::mem::take(&mut work.paths),
            )
        };
        paths.sort();
        paths.dedup();
        let result = if full {
            run(exe, None)
        } else {
            run(exe, Some(&paths))
        };
        match result {
            Ok(status) if status.success() => {}
            // A file hung or a run crashed: it is quarantined, so a full pass picks up the rest.
            Ok(status) => {
                tracing::warn!("indexer exited with {status}; running a full pass");
                indexer.work.lock().unwrap().full = true;
            }
            Err(e) => {
                tracing::error!("could not run the indexer: {e}");
                std::thread::sleep(Duration::from_secs(60));
            }
        }
    }
}

fn run(exe: &Path, paths: Option<&[PathBuf]>) -> std::io::Result<std::process::ExitStatus> {
    let mut command = Command::new(exe);
    command.creation_flags(CREATE_NO_WINDOW);
    let Some(paths) = paths else {
        return command.status();
    };
    let mut child = command.arg("--stdin").stdin(Stdio::piped()).spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        for path in paths {
            writeln!(stdin, "{}", path.display())?;
        }
    }
    child.wait()
}
