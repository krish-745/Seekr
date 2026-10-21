//! Whole-drive file-name search: an in-memory index fed by the SeekrIndex service and cached on
//! disk, so a restart only catches up from the last USN instead of re-reading the drive.

use std::{
    fs,
    io::{self, BufReader, BufWriter},
    path::PathBuf,
    sync::{
        Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use fsindex::{
    index::{Index, can_narrow},
    wire::{self, Message, PIPE_NAME},
};
use tauri::{AppHandle, Manager};

use crate::{direct, query::Item, rank::Ranker};

/// shortcut: C: only, which is the only volume the service reads; add drives with it.
const DRIVE: char = 'C';
/// Fuzzy matches re-ranked with location, frecency and pins, and how many of those are shown.
const CANDIDATES: usize = 300;
const MAX_RESULTS: usize = 50;
/// One letter matches about half the drive, which is a full scan and useless as results, so
/// file search starts at two.
const MIN_QUERY_CHARS: usize = 2;
const SAVE_EVERY: Duration = Duration::from_secs(300);
const RECONNECT_AFTER: Duration = Duration::from_secs(30);
/// Fuzzy score points; a good match on a short name scores around 100-150.
const SYSTEM_PENALTY: f64 = 40.0;
const PROFILE_BOOST: f64 = 15.0;
const WORK_FOLDER_BOOST: f64 = 15.0;
/// Lower-case path fragments of folders people rarely want results from.
const SYSTEM_PATHS: [&str; 10] = [
    r"c:\windows\",
    r"\$recycle.bin\",
    r"\appdata\",
    r"c:\programdata\",
    r"c:\program files",
    r"\node_modules\",
    r"\.git\",
    r"\__pycache__\",
    r"\.cache\",
    r"\target\",
];
const WORK_FOLDERS: [&str; 3] = [r"\desktop", r"\documents", r"\downloads"];

/// Narrowing levels kept while typing; a 16-character query rarely needs more.
const MAX_LEVELS: usize = 16;

/// The index generation they belong to, and (query, every entry it matched) pairs, each query
/// extending the one before.
type Levels = (u64, Vec<(String, Vec<u32>)>);

#[derive(Default)]
pub struct Files {
    index: RwLock<Option<Index>>,
    /// Bumped on every change to `index`, which invalidates `levels`.
    generation: AtomicU64,
    levels: Mutex<Levels>,
}

fn cache_path() -> PathBuf {
    common::data_dir().join("files-C.idx")
}

pub fn start(app: &AppHandle) {
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("file-index".into())
        .spawn(move || run(&app));
    if let Err(e) = spawned {
        tracing::error!("no file search: {e}");
    }
}

fn run(app: &AppHandle) {
    let files = app.state::<Files>();
    let started = Instant::now();
    match fs::File::open(cache_path()).and_then(|f| Index::load(BufReader::new(f))) {
        Ok(index) => {
            tracing::info!(
                "loaded {} files from the cache in {:?}",
                index.len(),
                started.elapsed()
            );
            *files.index.write().unwrap() = Some(index);
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("file cache unusable ({e}); waiting for a fresh snapshot"),
    }
    loop {
        if let Err(e) = follow(&files) {
            tracing::info!("file index service unavailable: {e}");
        }
        std::thread::sleep(RECONNECT_AFTER);
    }
}

/// shortcut: trusts whoever serves the pipe name; a same-user program could feed fake names, but
/// it could already open the files itself. Check GetNamedPipeServerProcessId if that changes.
fn follow(files: &Files) -> io::Result<()> {
    let mut pipe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(PIPE_NAME)?;
    let (journal_id, usn) = files
        .index
        .read()
        .unwrap()
        .as_ref()
        .map_or((0, 0), |i| (i.journal_id, i.usn));
    wire::write_message(&mut pipe, &Message::Hello { journal_id, usn })?;
    let mut saved_at = Instant::now();
    let mut dirty = false;
    loop {
        match wire::read_message(&mut pipe)? {
            Message::Snapshot { path, header } => {
                let started = Instant::now();
                let mut records = Vec::with_capacity(header.count as usize);
                wire::read_snapshot(BufReader::new(fs::File::open(&path)?), |r| records.push(r))?;
                let index = Index::from_records(header.journal_id, header.usn, records);
                tracing::info!(
                    "indexed {} files from the snapshot in {:?}",
                    index.len(),
                    started.elapsed()
                );
                *files.index.write().unwrap() = Some(index);
                files.generation.fetch_add(1, Ordering::SeqCst);
                save(files);
                (saved_at, dirty) = (Instant::now(), false);
            }
            Message::Changes { next_usn, records } => {
                if let Some(index) = files.index.write().unwrap().as_mut() {
                    records.iter().for_each(|r| index.apply(r));
                    index.usn = next_usn;
                    files.generation.fetch_add(1, Ordering::SeqCst);
                    dirty = true;
                }
            }
            Message::Hello { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected Hello",
                ));
            }
        }
        if dirty && saved_at.elapsed() > SAVE_EVERY {
            save(files);
            (saved_at, dirty) = (Instant::now(), false);
        }
    }
}

fn save(files: &Files) {
    let guard = files.index.read().unwrap();
    let Some(index) = guard.as_ref() else {
        return;
    };
    let started = Instant::now();
    let tmp = cache_path().with_extension("tmp");
    let saved = fs::File::create(&tmp)
        .and_then(|f| index.save(BufWriter::new(f)))
        .and_then(|()| fs::rename(&tmp, cache_path()));
    match saved {
        Ok(()) => tracing::info!(
            "saved {} files to the cache in {:?}",
            index.len(),
            started.elapsed()
        ),
        Err(e) => tracing::error!("could not save the file cache: {e}"),
    }
}

/// Best file and folder names for `query`; None once a newer keystroke cancels it.
pub fn search(
    app: &AppHandle,
    query: &str,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<Item>> {
    let query = query.trim();
    let files = app.state::<Files>();
    let guard = files.index.read().unwrap();
    let too_short = query.chars().count() < MIN_QUERY_CHARS;
    let (Some(index), false) = (guard.as_ref(), too_short) else {
        return Some(Vec::new());
    };
    let hits = narrowed_search(&files, index, query, cancelled)?;
    let boosts = app.state::<Ranker>().boosts(query);
    let profile = std::env::var("USERPROFILE")
        .unwrap_or_default()
        .to_lowercase();
    let mut scored: Vec<(f64, String, u32)> = hits
        .into_iter()
        .map(|hit| {
            let path = index.path(DRIVE, hit.entry);
            let score = f64::from(hit.score)
                + place(&path, &profile)
                + boosts.score(&format!("path:{path}"));
            (score, path, hit.entry)
        })
        .collect();
    scored.sort_by(|(a, x, _), (b, y, _)| b.total_cmp(a).then(x.len().cmp(&y.len())));
    let items = scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, path, entry)| {
            let folder = path.rsplit_once('\\').map_or("", |(f, _)| f).to_string();
            let name = index.name(entry).to_string();
            boosts.decorate(direct::path_item(path, name, folder))
        })
        .collect();
    Some(items)
}

/// Searches only the previous query's matches while the user keeps typing the same word, which
/// makes every keystroke after the first a fraction of a full scan.
fn narrowed_search(
    files: &Files,
    index: &Index,
    query: &str,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<fsindex::index::Hit>> {
    let mut levels = files.levels.lock().unwrap();
    let generation = files.generation.load(Ordering::SeqCst);
    if levels.0 != generation {
        *levels = (generation, Vec::new());
    }
    // Drop levels the new query doesn't extend, then search the narrowest one that is valid.
    while levels
        .1
        .last()
        .is_some_and(|(q, _)| !query.starts_with(q.as_str()))
    {
        levels.1.pop();
    }
    let within = levels
        .1
        .iter()
        .rev()
        .find(|(q, _)| can_narrow(q, query))
        .map(|(_, all)| all.as_slice());
    let matches = index.search(query, within, CANDIDATES, cancelled)?;
    let repeated = levels.1.last().is_some_and(|(q, _)| q == query);
    if !repeated && levels.1.len() < MAX_LEVELS {
        levels.1.push((query.to_string(), matches.all));
    }
    Some(matches.top)
}

/// Where a result lives: system and build folders sink, the user's own work folders rise.
fn place(path: &str, profile: &str) -> f64 {
    let lower = path.to_lowercase();
    if SYSTEM_PATHS.iter().any(|p| lower.contains(p)) {
        return -SYSTEM_PENALTY;
    }
    let under_profile = lower
        .strip_prefix(profile)
        .filter(|rest| !profile.is_empty() && (rest.is_empty() || rest.starts_with('\\')));
    let Some(rest) = under_profile else {
        return 0.0;
    };
    let in_work_folder = WORK_FOLDERS.iter().any(|w| {
        rest.strip_prefix(w)
            .is_some_and(|r| r.is_empty() || r.starts_with('\\'))
    });
    PROFILE_BOOST
        + if in_work_folder {
            WORK_FOLDER_BOOST
        } else {
            0.0
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_work_folders_above_system_ones() {
        let me = r"c:\users\me";
        let docs = place(r"C:\Users\me\Documents\report.pdf", me);
        let home = place(r"C:\Users\me\report.pdf", me);
        let other = place(r"C:\Data\report.pdf", me);
        let system = place(r"C:\Windows\System32\report.pdf", me);
        let cache = place(r"C:\Users\me\AppData\Local\report.pdf", me);
        assert!(docs > home && home > other && other > system);
        assert_eq!(cache, -SYSTEM_PENALTY);
        assert_eq!(
            place(r"C:\Users\me\Documents", me),
            PROFILE_BOOST + WORK_FOLDER_BOOST
        );
        assert_eq!(place(r"C:\Users\me\DocumentsOld\x", me), PROFILE_BOOST);
        assert_eq!(
            place(r"C:\Users\meow\x", me),
            0.0,
            "a different profile sharing a prefix"
        );
    }
}
