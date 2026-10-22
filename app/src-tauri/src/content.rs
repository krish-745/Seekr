//! The content-search folder picker: suggestions (none pre-checked), the user's own additions,
//! and a native folder chooser. The choice is saved through `common::content`.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime},
};

use content::{
    embed::{self, Device, Embedder, Weights},
    store::Store,
    vectors::{self, Vectors},
};
use fsindex::filter::Parsed;
use serde::Serialize;
use tantivy::IndexReader;
use tauri::{AppHandle, Manager, State};
use windows::Win32::{
    Foundation::HWND,
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
    },
};

use crate::{direct, fileops::Com, indexer::Indexer, query::Item};

/// Content results shown per query, and how many each of keyword and meaning search proposes.
const RESULTS: usize = 20;
const CANDIDATES: usize = 40;
/// Reciprocal rank fusion's usual constant: ranks matter, but the top few don't dominate.
const RRF_K: f32 = 60.0;
/// A meaning match below this centred cosine is unrelated text: on real folders, hub passages
/// reach about 0.35 and nonsense queries 0.25, while a passage that means the query scores 0.4+.
const MEANING_FLOOR: f32 = 0.36;
/// A partial word has no meaning to search for.
const MEANING_MIN_CHARS: usize = 4;
/// The query model holds about 170 MB, so it goes soon after the panel is put away; loading it
/// again takes well under a second, on a thread, while keyword results show.
const MODEL_IDLE: Duration = Duration::from_secs(30);
const SNIPPET_CHARS: usize = 180;
/// Shorter queries match too much text to be useful.
const MIN_QUERY_CHARS: usize = 3;

/// Code folders people commonly keep under their profile; suggested when they exist.
const CODE_FOLDERS: [&str; 7] = [
    r"source\repos",
    "Projects",
    "projects",
    "dev",
    "code",
    "repos",
    "GitHub",
];

#[derive(Serialize)]
pub struct Choice {
    path: String,
    checked: bool,
}

#[derive(Serialize)]
pub struct Setup {
    folders: Vec<Choice>,
    /// Always searched, whatever is chosen.
    notes: Option<String>,
}

/// Whether the first-run picker hasn't been answered yet.
#[tauri::command]
pub fn content_setup_needed() -> bool {
    common::content::chosen().is_none()
}

#[tauri::command]
pub fn content_setup() -> Setup {
    let chosen = common::content::chosen().unwrap_or_default();
    let profile = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let mut paths = common::content::suggestions();
    if let Some(profile) = &profile {
        paths.extend(
            CODE_FOLDERS
                .iter()
                .map(|f| profile.join(f))
                .filter(|p| p.is_dir()),
        );
    }
    // Folders chosen earlier that aren't suggestions stay listed, checked.
    paths.extend(chosen.iter().cloned());
    let mut folders: Vec<Choice> = Vec::new();
    for path in paths {
        let text = path.display().to_string();
        if folders.iter().any(|c| c.path.eq_ignore_ascii_case(&text)) {
            continue;
        }
        let checked = chosen.iter().any(|c| same(c, &path));
        folders.push(Choice {
            path: text,
            checked,
        });
    }
    Setup {
        folders,
        notes: common::content::notes_dir().map(|p| p.display().to_string()),
    }
}

fn same(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
    norm(a) == norm(b)
}

#[tauri::command]
pub fn save_content_folders(paths: Vec<String>, indexer: State<'_, Indexer>) -> Result<(), String> {
    let folders: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    tracing::info!("content search folders: {}", folders.len());
    common::content::set_chosen(&folders).map_err(|e| e.to_string())?;
    indexer.refresh();
    Ok(())
}

/// The Windows folder chooser. The panel hides while it is open and comes back afterwards.
#[tauri::command]
pub async fn choose_folder(app: AppHandle) -> Option<String> {
    let owner = crate::panel_hwnd(&app).map(|h| h.0 as isize);
    let chosen = tauri::async_runtime::spawn_blocking(move || pick(owner.map(|h| HWND(h as _))))
        .await
        .ok()
        .flatten();
    crate::show(&app);
    chosen
}

fn pick(owner: Option<HWND>) -> Option<String> {
    let _com = Com::init();
    let dialog: IFileOpenDialog =
        unsafe { CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()? };
    unsafe {
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM)
            .ok()?;
        // Cancelling returns an error, which is just "no folder".
        dialog.Show(owner).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok();
        windows::Win32::System::Com::CoTaskMemFree(Some(name.0 as _));
        path
    }
}

/// "Add to content search" on a folder result.
pub fn add(app: &AppHandle, path: &str) -> Result<(), String> {
    common::content::add(Path::new(path)).map_err(|e| e.to_string())?;
    app.state::<Indexer>().refresh();
    Ok(())
}

/// Keyword and meaning search over the indexer's output, opened lazily: the full-text index at
/// the first search after it exists, the query model when the panel opens.
#[derive(Default)]
pub struct Search {
    index: Mutex<Option<(Store, IndexReader)>>,
    meaning: Mutex<Meaning>,
}

#[derive(Default)]
struct Meaning {
    model: Option<Embedder>,
    /// The vector file and when it was written, reloaded when the indexer saves a new one.
    vectors: Option<(SystemTime, Vectors)>,
    used: Option<Instant>,
}

/// Loads the query model in the background when the panel opens, so the first keystroke doesn't
/// wait for it, and drops it once it has gone unused for a while.
pub fn warm(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        let state = app.state::<Search>();
        let mut meaning = state.meaning.lock().unwrap();
        meaning.used = Some(Instant::now());
        meaning.load();
    });
}

/// Drops the query model and vectors after `MODEL_IDLE` without a search.
pub fn start_reaper(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(10));
            let state = app.state::<Search>();
            let mut meaning = state.meaning.lock().unwrap();
            if meaning.model.is_some() && meaning.used.is_none_or(|u| u.elapsed() > MODEL_IDLE) {
                *meaning = Meaning::default();
                tracing::info!("query model unloaded after {MODEL_IDLE:?} unused");
            }
        }
    });
}

/// Files whose text matches the query, by words (BM25) and by meaning (embeddings), fused by
/// reciprocal rank, as path results whose subtitle is the matching line. Files in `shown` (the
/// file-name results already on screen) are left out.
pub fn search(app: &AppHandle, parsed: &Parsed, shown: &HashSet<String>) -> Vec<Item> {
    let query = tantivy_query(&parsed.text);
    // shortcut: file filters (ext:, in:, dm:) don't narrow content results yet; until they do,
    // a filtered query shows file names only.
    if query.chars().count() < MIN_QUERY_CHARS || !parsed.filter.is_empty() {
        return Vec::new();
    }
    let state = app.state::<Search>();
    let mut open = state.index.lock().unwrap();
    let Some((store, reader)) = open_index(&mut open) else {
        return Vec::new();
    };
    let keyword = store
        .search(reader, &query, CANDIDATES)
        .unwrap_or_else(|e| {
            tracing::warn!("content search failed: {e}");
            Vec::new()
        });
    let meaning = if query.chars().count() >= MEANING_MIN_CHARS {
        state.meaning.lock().unwrap().search(&plain_words(&query))
    } else {
        Vec::new()
    };

    // Reciprocal rank fusion: each list adds 1/(k + rank) for the files it found.
    let mut fused: Vec<(f32, String, u32, Option<String>)> = Vec::new();
    let mut add = |path: &str, rank: usize, line: u32, snippet: Option<String>| {
        let gain = 1.0 / (RRF_K + rank as f32 + 1.0);
        match fused.iter_mut().find(|f| f.1 == path) {
            Some(f) => {
                f.0 += gain;
                if f.3.is_none() {
                    (f.2, f.3) = (line, snippet);
                }
            }
            None => fused.push((gain, path.to_string(), line, snippet)),
        }
    };
    for (rank, hit) in keyword.iter().enumerate() {
        add(&hit.path, rank, hit.line, Some(hit.snippet.clone()));
    }
    for (rank, hit) in meaning
        .iter()
        .filter(|h| h.score >= MEANING_FLOOR)
        .enumerate()
    {
        add(&hit.path, rank, hit.line, None);
    }
    fused.sort_by(|a, b| b.0.total_cmp(&a.0));
    // Left out at once, before the indexer's next pass drops them.
    let config = crate::settings::get();
    fused
        .into_iter()
        .filter(|f| !shown.contains(&f.1.to_lowercase()) && !config.excludes(&f.1))
        .take(RESULTS)
        .map(|(_, path, line, snippet)| {
            let name = Path::new(&path)
                .file_name()
                .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
            // A meaning-only match shows the start of the passage it matched.
            let text = snippet
                .or_else(|| store.passage(reader, &path, line))
                .unwrap_or_default();
            let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let text: String = text.chars().take(SNIPPET_CHARS).collect();
            let subtitle = format!("Line {line} · {text}");
            let mut item = direct::path_item(path, name, subtitle, false);
            item.line = Some(line);
            item
        })
        .collect()
}

/// The full-text index, opened the first time it is needed once the indexer has made one.
fn open_index(open: &mut Option<(Store, IndexReader)>) -> Option<&(Store, IndexReader)> {
    if open.is_none() {
        let dir = content::index_dir();
        if !dir.join("meta.json").exists() {
            return None;
        }
        match Store::open(&dir).and_then(|s| s.reader().map(|r| (s, r))) {
            Ok(opened) => *open = Some(opened),
            Err(e) => {
                tracing::warn!("content index unavailable: {e}");
                return None;
            }
        }
    }
    open.as_ref()
}

/// A passage of a file, given to the AI to answer from.
#[derive(Clone, Debug, PartialEq)]
pub struct Passage {
    pub path: String,
    /// The line it starts on.
    pub line: u32,
    pub text: String,
}

/// Passages for a question, up to `budget` characters, fused from keyword and meaning search
/// the way content results are, leaving out those in `skip` (already given).
pub fn retrieve(app: &AppHandle, question: &str, budget: usize, skip: &[Passage]) -> Vec<Passage> {
    let state = app.state::<Search>();
    let words: String = question
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let meaning = {
        let mut meaning = state.meaning.lock().unwrap();
        meaning.load();
        meaning.search(question)
    };
    let mut open = state.index.lock().unwrap();
    let Some((store, reader)) = open_index(&mut open) else {
        return Vec::new();
    };
    let keyword = store
        .passages(reader, &words, CANDIDATES)
        .unwrap_or_else(|e| {
            tracing::warn!("passage search failed: {e}");
            Vec::new()
        });
    let mut fused: Vec<(f32, String, u32, Option<String>)> = Vec::new();
    let mut add = |path: &str, line: u32, rank: usize, text: Option<String>| {
        let gain = 1.0 / (RRF_K + rank as f32 + 1.0);
        match fused.iter_mut().find(|f| f.1 == path && f.2 == line) {
            Some(f) => f.0 += gain,
            None => fused.push((gain, path.to_string(), line, text)),
        }
    };
    for (rank, (path, line, text)) in keyword.into_iter().enumerate() {
        add(&path, line, rank, Some(text));
    }
    for (rank, hit) in meaning
        .iter()
        .filter(|h| h.score >= MEANING_FLOOR)
        .enumerate()
    {
        add(&hit.path, hit.line, rank, None);
    }
    fused.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut chosen = Vec::new();
    let mut size = 0;
    let config = crate::settings::get();
    for (_, path, line, text) in fused {
        if config.excludes(&path) || skip.iter().any(|p| p.path == path && p.line == line) {
            continue;
        }
        let Some(text) = text.or_else(|| store.passage(reader, &path, line)) else {
            continue;
        };
        if size + text.len() > budget {
            continue;
        }
        size += text.len();
        chosen.push(Passage { path, line, text });
    }
    chosen
}

impl Meaning {
    /// Loads the query model if it isn't loaded and is installed.
    fn load(&mut self) {
        if self.model.is_some() || !embed::available(Weights::Int8) {
            return;
        }
        let start = Instant::now();
        match Embedder::load(Weights::Int8, Device::Cpu) {
            Ok(model) => {
                tracing::info!("query model loaded in {:?}", start.elapsed());
                self.model = Some(model);
            }
            Err(e) => tracing::warn!("no meaning search: {e}"),
        }
    }

    /// The files closest in meaning to `text`; empty without a model or vectors.
    fn search(&mut self, text: &str) -> Vec<vectors::Hit> {
        self.used = Some(Instant::now());
        let path = content::vectors_path();
        let written = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let stale = self.vectors.as_ref().map(|(t, _)| Some(*t)) != Some(written);
        if stale && let Some(written) = written {
            match Vectors::load(&path) {
                Ok(v) => self.vectors = Some((written, v)),
                Err(e) => tracing::warn!("vectors unreadable: {e}"),
            }
        }
        let (Some(model), Some((_, vectors))) = (self.model.as_mut(), self.vectors.as_ref()) else {
            return Vec::new();
        };
        match model.embed(&[text]) {
            Ok(q) => vectors.search(&q[0], CANDIDATES),
            Err(e) => {
                tracing::warn!("query embedding failed: {e}");
                Vec::new()
            }
        }
    }
}

/// The words of a Tantivy query, without quotes, exclusions or field syntax, for embedding.
fn plain_words(query: &str) -> String {
    query
        .split_whitespace()
        .filter(|w| !w.starts_with('-'))
        .map(|w| w.trim_matches('"'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The name pattern in Tantivy's syntax: nucleo's `'exact\ words` becomes a quoted phrase,
/// `!word` becomes `-word`, and the anchors `^` and `$` are dropped.
fn tantivy_query(pattern: &str) -> String {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&' ') => {
                word.push(' ');
                chars.next();
            }
            ' ' => words.push(std::mem::take(&mut word)),
            c => word.push(c),
        }
    }
    words.push(word);
    words
        .into_iter()
        .filter(|w| !w.is_empty())
        .map(|w| {
            if let Some(phrase) = w.strip_prefix('\'') {
                format!("\"{}\"", phrase.replace('"', ""))
            } else if let Some(not) = w.strip_prefix('!') {
                format!("-{not}")
            } else {
                w.trim_matches(['^', '$']).to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_name_patterns_to_tantivy_syntax() {
        let parsed = fsindex::filter::parse(r#"invoice "due date" -draft"#);
        assert_eq!(tantivy_query(&parsed.text), r#"invoice "due date" -draft"#);
        assert_eq!(
            plain_words(&tantivy_query(&parsed.text)),
            "invoice due date"
        );
        assert_eq!(tantivy_query("^readme$"), "readme");
    }
}
