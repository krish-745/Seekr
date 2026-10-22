//! The content-search folder picker: suggestions (none pre-checked), the user's own additions,
//! and a native folder chooser. The choice is saved through `common::content`.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

use content::store::Store;
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

/// Content results shown per query.
const RESULTS: usize = 20;
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

/// The content index, opened at the first search after the indexer has created it.
#[derive(Default)]
pub struct Search(Mutex<Option<(Store, IndexReader)>>);

/// Files whose text matches the query's words, best passage first, as path results whose
/// subtitle is the matching line.
pub fn search(app: &AppHandle, parsed: &Parsed) -> Vec<Item> {
    let query = tantivy_query(&parsed.text);
    // shortcut: file filters (ext:, in:, dm:) don't narrow content results yet; until they do,
    // a filtered query shows file names only.
    if query.chars().count() < MIN_QUERY_CHARS || !parsed.filter.is_empty() {
        return Vec::new();
    }
    let state = app.state::<Search>();
    let mut open = state.0.lock().unwrap();
    if open.is_none() {
        let dir = content::index_dir();
        if !dir.join("meta.json").exists() {
            return Vec::new();
        }
        match Store::open(&dir).and_then(|s| s.reader().map(|r| (s, r))) {
            Ok(opened) => *open = Some(opened),
            Err(e) => {
                tracing::warn!("content index unavailable: {e}");
                return Vec::new();
            }
        }
    }
    let Some((store, reader)) = open.as_ref() else {
        return Vec::new();
    };
    let hits = match store.search(reader, &query, RESULTS) {
        Ok(hits) => hits,
        Err(e) => {
            tracing::warn!("content search failed: {e}");
            return Vec::new();
        }
    };
    hits.into_iter()
        .map(|hit| {
            let name = Path::new(&hit.path)
                .file_name()
                .map_or_else(|| hit.path.clone(), |n| n.to_string_lossy().into_owned());
            let snippet = hit.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
            let subtitle = format!("Line {} · {snippet}", hit.line);
            direct::path_item(hit.path, name, subtitle, false)
        })
        .collect()
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
        assert_eq!(tantivy_query("^readme$"), "readme");
    }
}
