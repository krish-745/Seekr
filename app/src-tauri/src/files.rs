//! Whole-drive file-name search: one in-memory index per fixed drive, cached on disk. C: is fed
//! by the SeekrIndex service, so a restart only catches up from the last USN. Other drives, and
//! C: when the service isn't installed, use the no-admin fallback: a walk, then a watcher.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
    sync::{
        Mutex, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    time::{Duration, Instant},
};

use fsindex::{
    filter::{Filter, Parsed},
    index::{Hit, Index, can_narrow},
    meta,
    scan::{self, Change},
    usn::Record,
    wire::{self, Message, PIPE_NAME},
};
use tauri::{AppHandle, Manager};

use crate::{direct, indexer::Indexer, query::Item, rank::Ranker};

/// The only volume the service reads.
const SERVICE_DRIVE: char = 'C';
const USN_REASON_FILE_DELETE: u32 = 0x200;
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

/// The index generation they belong to, and (name pattern, filter, per drive: every entry it
/// matched and how many entries the index had), each extending the one before.
type Levels = (
    u64,
    Vec<(String, Filter, BTreeMap<char, (Vec<u32>, usize)>)>,
);

#[derive(Default)]
pub struct Files {
    drives: RwLock<BTreeMap<char, Index>>,
    /// Bumped when a drive's index is replaced or its sizes and dates are filled, which
    /// invalidates `levels`. Journal and watcher changes don't bump it: new entries are appended,
    /// and a level also searches every entry added after it was made.
    generation: AtomicU64,
    levels: Mutex<Levels>,
}

impl Files {
    /// Each drive's number of names, for diagnostics.
    pub fn counts(&self) -> Vec<(char, usize)> {
        let drives = self.drives.read().unwrap();
        drives.iter().map(|(d, index)| (*d, index.len())).collect()
    }

    fn set(&self, drive: char, index: Index) {
        self.drives.write().unwrap().insert(drive, index);
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) fn cache_path(drive: char) -> PathBuf {
    common::data_dir().join(format!("files-{drive}.idx"))
}

pub fn start(app: &AppHandle) {
    let service = service_installed();
    for drive in scan::fixed_drives() {
        let app = app.clone();
        let use_service = service && drive == SERVICE_DRIVE;
        let spawned = std::thread::Builder::new()
            .name(format!("file-index-{drive}"))
            .spawn(move || run(&app, drive, use_service));
        if let Err(e) = spawned {
            tracing::error!("no file search on {drive}: {e}");
        }
    }
}

/// shortcut: an installed service is assumed to be running, so a stopped one leaves C: on its
/// cached names until it's back; ask the service manager if that turns out to matter.
fn service_installed() -> bool {
    std::env::var_os("ProgramFiles")
        .is_some_and(|p| Path::new(&p).join(r"Seekr\seekr-svc.exe").exists())
}

fn run(app: &AppHandle, drive: char, use_service: bool) {
    let files = app.state::<Files>();
    let started = Instant::now();
    match fs::File::open(cache_path(drive)).and_then(|f| Index::load(BufReader::new(f))) {
        Ok(index) => {
            tracing::info!(
                "loaded {} names on {drive}: from the cache in {:?}",
                index.len(),
                started.elapsed()
            );
            let missing_stats = !index.has_stats();
            files.set(drive, index);
            if use_service && missing_stats {
                fill_stats_later(app, drive);
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!("{drive}: cache unusable ({e}); indexing it afresh"),
    }
    loop {
        let stopped = if use_service {
            follow(app, &files)
        } else {
            fallback(&files, &app.state::<Indexer>(), drive)
        };
        if let Err(e) = stopped {
            tracing::info!("{drive}: file index unavailable: {e}");
        }
        std::thread::sleep(RECONNECT_AFTER);
    }
}

/// shortcut: trusts whoever serves the pipe name; a same-user program could feed fake names, but
/// it could already open the files itself. Check GetNamedPipeServerProcessId if that changes.
fn follow(app: &AppHandle, files: &Files) -> io::Result<()> {
    let mut pipe = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(PIPE_NAME)?;
    let (journal_id, usn) = files
        .drives
        .read()
        .unwrap()
        .get(&SERVICE_DRIVE)
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
                files.set(SERVICE_DRIVE, index);
                save(files, SERVICE_DRIVE);
                (saved_at, dirty) = (Instant::now(), false);
                fill_stats_later(app, SERVICE_DRIVE);
            }
            Message::Changes { next_usn, records } => {
                let gone = deleted_paths(files, SERVICE_DRIVE, &records);
                if let Some(index) = files.drives.write().unwrap().get_mut(&SERVICE_DRIVE) {
                    records.iter().for_each(|r| index.apply(r));
                    index.usn = next_usn;
                    dirty = true;
                }
                let touched = restat(files, SERVICE_DRIVE, &records);
                app.state::<Indexer>()
                    .changed(gone.into_iter().chain(touched));
            }
            Message::Hello { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected Hello",
                ));
            }
        }
        if dirty && saved_at.elapsed() > SAVE_EVERY {
            save(files, SERVICE_DRIVE);
            (saved_at, dirty) = (Instant::now(), false);
        }
    }
}

/// Reads the size and date of every entry the journal touched, since its records carry neither.
/// Returns the paths it read, for the content indexer.
fn restat(files: &Files, drive: char, records: &[Record]) -> Vec<String> {
    let paths: Vec<(u64, String)> = {
        let drives = files.drives.read().unwrap();
        let Some(index) = drives.get(&drive) else {
            return Vec::new();
        };
        let live: HashSet<u64> = records
            .iter()
            .filter(|r| r.reason & USN_REASON_FILE_DELETE == 0)
            .map(|r| r.frn)
            .collect();
        live.into_iter()
            .filter_map(|frn| Some((frn, index.path(drive, index.entry(frn)?))))
            .collect()
    };
    let stats: Vec<_> = paths
        .iter()
        .filter_map(|(frn, path)| Some((*frn, scan::stat(Path::new(path))?.1)))
        .collect();
    if let Some(index) = files.drives.write().unwrap().get_mut(&drive) {
        for (frn, (_, size, modified)) in stats {
            index.set_stat(frn, size, modified);
        }
    }
    paths.into_iter().map(|(_, path)| path).collect()
}

/// Paths of the entries these records delete, read before the records are applied.
fn deleted_paths(files: &Files, drive: char, records: &[Record]) -> Vec<String> {
    let drives = files.drives.read().unwrap();
    let Some(index) = drives.get(&drive) else {
        return Vec::new();
    };
    records
        .iter()
        .filter(|r| r.reason & USN_REASON_FILE_DELETE != 0)
        .filter_map(|r| Some(index.path(drive, index.entry(r.frn)?)))
        .collect()
}

/// The service's names come without sizes and dates; a low-priority walk (about 20 s for C:)
/// fills them in by path. Runs after each fresh snapshot, and at startup if the cache has none.
fn fill_stats_later(app: &AppHandle, drive: char) {
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name(format!("file-stats-{drive}"))
        .spawn(move || fill_stats(&app.state::<Files>(), drive));
    if let Err(e) = spawned {
        tracing::error!("{drive}: no sizes or dates: {e}");
    }
}

fn fill_stats(files: &Files, drive: char) {
    let started = Instant::now();
    let ids: HashMap<u64, u64> = {
        let drives = files.drives.read().unwrap();
        let Some(index) = drives.get(&drive) else {
            return;
        };
        index
            .entries()
            .map(|(entry, frn)| (scan::path_id(Path::new(&index.path(drive, entry))), frn))
            .collect()
    };
    let stats = Mutex::new(Vec::new());
    let root = PathBuf::from(format!(r"{drive}:\"));
    let walked = meta::walk(&root, &AtomicBool::new(false), |listing| {
        let found: Vec<(u64, u64, i64)> = listing
            .iter()
            .filter_map(|m| Some((*ids.get(&scan::path_id(&m.path))?, m.size, m.modified)))
            .collect();
        stats.lock().unwrap().extend(found);
    });
    if let Err(e) = walked {
        tracing::warn!("{drive}: sizes and dates walk failed: {e}");
        return;
    }
    let stats = stats.into_inner().unwrap();
    if let Some(index) = files.drives.write().unwrap().get_mut(&drive) {
        for &(frn, size, modified) in &stats {
            index.set_stat(frn, size, modified);
        }
    }
    files.generation.fetch_add(1, Ordering::SeqCst);
    tracing::info!(
        "filled {} sizes and dates on {drive}: in {:?}",
        stats.len(),
        started.elapsed()
    );
    save(files, drive);
}

/// Walks `drive`, then follows its watcher, walking again whenever the watcher drops changes.
fn fallback(files: &Files, indexer: &Indexer, drive: char) -> io::Result<()> {
    let root = PathBuf::from(format!(r"{drive}:\"));
    let (tx, rx) = mpsc::channel();
    let watched = root.clone();
    // Started first, so changes made during the walk queue up and are applied after it.
    std::thread::Builder::new()
        .name(format!("file-watch-{drive}"))
        .spawn(move || {
            if let Err(e) = scan::watch(&watched, |changes| tx.send(changes).is_ok()) {
                tracing::warn!("{drive}: watcher stopped: {e}");
            }
        })?;
    loop {
        let started = Instant::now();
        let index = scan::scan(&root, &AtomicBool::new(false))?;
        tracing::info!(
            "scanned {} names on {drive}: in {:?}",
            index.len(),
            started.elapsed()
        );
        files.set(drive, index);
        save(files, drive);
        let mut saved_at = Instant::now();
        let mut dirty = false;
        loop {
            let changes = match rx.recv_timeout(SAVE_EVERY) {
                Ok(changes) => changes,
                Err(RecvTimeoutError::Timeout) => Vec::new(),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("the watcher stopped"));
                }
            };
            if changes.contains(&Change::Overflow) {
                tracing::info!("{drive}: too many changes at once; scanning again");
                break;
            }
            if !changes.is_empty() {
                indexer.changed(changes.iter().filter_map(|c| match c {
                    Change::Added(p) | Change::Removed(p) | Change::Modified(p) => {
                        Some(p.display().to_string())
                    }
                    Change::Overflow => None,
                }));
                // Walking a folder that moved in happens under the read lock, so search goes on.
                let records = files
                    .drives
                    .read()
                    .unwrap()
                    .get(&drive)
                    .map(|index| scan::records(index, &root, &changes))
                    .unwrap_or_default();
                if let Some(index) = files.drives.write().unwrap().get_mut(&drive) {
                    index.apply_watched(&records.0);
                    for &(frn, size, modified) in &records.1 {
                        index.set_stat(frn, size, modified);
                    }
                }
                dirty = true;
            }
            if dirty && saved_at.elapsed() > SAVE_EVERY {
                save(files, drive);
                (saved_at, dirty) = (Instant::now(), false);
            }
        }
    }
}

fn save(files: &Files, drive: char) {
    let guard = files.drives.read().unwrap();
    let Some(index) = guard.get(&drive) else {
        return;
    };
    let started = Instant::now();
    let path = cache_path(drive);
    let tmp = path.with_extension("tmp");
    let saved = fs::File::create(&tmp)
        .and_then(|f| index.save(BufWriter::new(f)))
        .and_then(|()| fs::rename(&tmp, &path));
    match saved {
        Ok(()) => tracing::info!(
            "saved {} names on {drive}: to the cache in {:?}",
            index.len(),
            started.elapsed()
        ),
        Err(e) => tracing::error!("could not save the {drive}: cache: {e}"),
    }
}

/// Best file and folder names for `parsed` (from the typed `query`); None once a newer keystroke
/// cancels it.
pub fn search(
    app: &AppHandle,
    query: &str,
    parsed: &Parsed,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<Item>> {
    let text = parsed.text.trim();
    let files = app.state::<Files>();
    let drives = files.drives.read().unwrap();
    let too_short = parsed.filter.is_empty() && text.chars().count() < MIN_QUERY_CHARS;
    if drives.is_empty() || too_short {
        return Some(Vec::new());
    }
    let hits = narrowed_search(&files, &drives, text, &parsed.filter, cancelled)?;
    let boosts = app.state::<Ranker>().boosts(query.trim());
    let profile = std::env::var("USERPROFILE")
        .unwrap_or_default()
        .to_lowercase();
    let config = crate::settings::get();
    let mut scored: Vec<(f64, String, &Index, u32)> = hits
        .into_iter()
        .map(|(drive, hit)| {
            let index = &drives[&drive];
            let path = index.path(drive, hit.entry);
            let score = f64::from(hit.score)
                + place(&path, &profile)
                + boosts.score(&format!("path:{path}"));
            (score, path, index, hit.entry)
        })
        .filter(|(_, path, ..)| !config.excludes(path))
        .collect();
    scored.sort_by(|(a, x, ..), (b, y, ..)| b.total_cmp(a).then(x.len().cmp(&y.len())));
    let items = scored
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(_, path, index, entry)| {
            let folder = path.rsplit_once('\\').map_or("", |(f, _)| f).to_string();
            let name = index.name(entry).to_string();
            let is_dir = index.is_dir(entry);
            boosts.decorate(direct::path_item(path, name, folder, is_dir))
        })
        .collect();
    Some(items)
}

/// Searches only the previous query's matches while the user keeps typing the same word, which
/// makes every keystroke after the first a fraction of a full scan.
fn narrowed_search(
    files: &Files,
    drives: &BTreeMap<char, Index>,
    query: &str,
    filter: &Filter,
    cancelled: &(dyn Fn() -> bool + Sync),
) -> Option<Vec<(char, Hit)>> {
    let mut levels = files.levels.lock().unwrap();
    let generation = files.generation.load(Ordering::SeqCst);
    if levels.0 != generation {
        *levels = (generation, Vec::new());
    }
    // Drop levels the new query doesn't extend, then search the narrowest one that is valid.
    while levels
        .1
        .last()
        .is_some_and(|(q, f, _)| f != filter || !query.starts_with(q.as_str()))
    {
        levels.1.pop();
    }
    let within = levels
        .1
        .iter()
        .rev()
        .find(|(q, _, _)| can_narrow(q, query))
        .map(|(_, _, all)| all);
    let mut top = Vec::new();
    let mut all = BTreeMap::new();
    for (&drive, index) in drives {
        // An `in:` path on another drive rules this one out.
        let Some(on_drive) = filter.on_drive(drive) else {
            continue;
        };
        // shortcut: an entry renamed into a match, or whose new size or date now passes the
        // filter, shows up at the next query that doesn't narrow; track touched entries if that
        // ever matters.
        let narrowed: Option<Vec<u32>> = within.and_then(|w| w.get(&drive)).map(|(all, len)| {
            let added = *len as u32..index.slots() as u32;
            all.iter().copied().chain(added).collect()
        });
        let matches =
            index.search_filtered(query, narrowed.as_deref(), CANDIDATES, &on_drive, cancelled)?;
        top.extend(matches.top.into_iter().map(|hit| (drive, hit)));
        all.insert(drive, (matches.all, index.slots()));
    }
    let repeated = levels
        .1
        .last()
        .is_some_and(|(q, f, _)| q == query && f == filter);
    if !repeated && levels.1.len() < MAX_LEVELS {
        levels.1.push((query.to_string(), filter.clone(), all));
    }
    Some(top)
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
