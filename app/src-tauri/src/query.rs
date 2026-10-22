//! Query engine: every keystroke is a query with a rising id, and a newer id cancels older work.

use std::{
    collections::HashSet,
    sync::atomic::{AtomicU64, Ordering},
};

use fsindex::filter;
use serde::Serialize;
use tauri::{AppHandle, Manager, State, ipc::Channel};

use crate::{
    apps, builtin, calc, clipboard, clips, content, direct, emoji, files, rank::Ranker, switcher,
    system, web::Web,
};

#[derive(Clone, Serialize)]
pub struct Action {
    pub id: &'static str,
    pub label: &'static str,
}

/// One result row. `actions[0]` runs on Enter, `actions[1]` on Ctrl+Enter.
#[derive(Clone, Serialize)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub subtitle: String,
    pub preview: String,
    /// `http://icon.localhost/<file>` from the icon cache.
    pub icon: Option<String>,
    /// For a content match: the line its passage starts on, where the preview opens.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    pub actions: Vec<Action>,
}

/// Results from one provider for query `id`. Each provider sends one batch per query (none once
/// the query is superseded), so the first batch of a new id tells the UI to replace the old list.
#[derive(Serialize)]
pub struct Batch {
    pub id: u64,
    /// "apps" (with commands and typed paths), "files", "content" or "web" (history and a web
    /// search), for latency telemetry.
    pub source: &'static str,
    pub items: Vec<Item>,
    /// The file filters typed, such as `ext: pdf`; only the first batch of a query carries them.
    pub chips: Vec<Chip>,
}

#[derive(Serialize)]
pub struct Chip {
    pub label: String,
    /// False when the value didn't parse, so the filter is ignored.
    pub valid: bool,
}

#[derive(Default)]
pub struct Engine {
    latest: AtomicU64,
}

impl Engine {
    /// Ids can arrive out of order across IPC, so only ever move forward.
    fn begin(&self, id: u64) {
        self.latest.fetch_max(id, Ordering::SeqCst);
    }

    pub fn is_current(&self, id: u64) -> bool {
        self.latest.load(Ordering::SeqCst) == id
    }
}

#[tauri::command]
pub async fn query(
    app: AppHandle,
    id: u64,
    text: String,
    on_batch: Channel<Batch>,
    engine: State<'_, Engine>,
    index: State<'_, apps::AppIndex>,
    ranker: State<'_, Ranker>,
) -> Result<(), String> {
    engine.begin(id);
    // `clip …` lists clipboard history, `win …` open windows and `:…` emoji, and nothing else.
    let windows = switcher::mode(&text).map(str::to_string);
    let glyphs = emoji::mode(&text);
    if clips::mode(&text).is_some() || windows.is_some() || glyphs.is_some() {
        let items = match windows {
            Some(words) => {
                let panel = crate::panel_hwnd(&app).map(|h| h.0 as isize);
                tauri::async_runtime::spawn_blocking(move || {
                    switcher::items(
                        &words,
                        panel.map(|h| windows::Win32::Foundation::HWND(h as _)),
                    )
                })
                .await
                .map_err(|e| e.to_string())?
            }
            None => match glyphs {
                Some(words) => emoji::items(words),
                None => clips::items(&app, clips::mode(&text).unwrap_or("")),
            },
        };
        if engine.is_current(id) {
            let chips = Vec::new();
            on_batch
                .send(Batch {
                    id,
                    source: "apps",
                    items,
                    chips,
                })
                .map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    let parsed = filter::parse(&text);
    // File names take milliseconds across all cores, so they start first, run while apps are
    // ranked, and follow as a second batch.
    let searcher = app.clone();
    let (file_text, file_query) = (text.clone(), parsed.clone());
    let files = tauri::async_runtime::spawn_blocking(move || {
        let engine = searcher.state::<Engine>();
        files::search(&searcher, &file_text, &file_query, &|| {
            !engine.is_current(id)
        })
    });
    // A calculation, typed path or URL is what the user meant, so it leads, unranked.
    let mut items = Vec::new();
    if parsed.filter.is_empty() {
        items.extend(calc::items(&text));
        items.extend(system::direct(&text));
        if let Some(web) = app.try_state::<Web>() {
            items.extend(web.shortcut_items(&text));
        }
    }
    items.extend(direct::items(&text));
    // Apps and Seekr's own commands are fast, so they are ranked together into the first batch;
    // slower providers append later batches. File filters mean the user wants files.
    if parsed.filter.is_empty() {
        let mut candidates = apps::all(&index);
        candidates.extend(builtin::all());
        candidates.extend(system::all());
        if let Some(web) = app.try_state::<Web>() {
            candidates.extend(web.bookmarks());
        }
        items.extend(ranker.rank(&text, candidates));
    }
    if !engine.is_current(id) {
        return Ok(());
    }
    let chips = parsed
        .chips
        .iter()
        .map(|c| Chip {
            label: c.label.clone(),
            valid: c.valid,
        })
        .collect();
    on_batch
        .send(Batch {
            id,
            source: "apps",
            items,
            chips,
        })
        .map_err(|e| e.to_string())?;
    let files = files.await.map_err(|e| e.to_string())?;
    // File-name results already on screen aren't repeated as content results.
    let shown: HashSet<String> = files
        .iter()
        .flatten()
        .filter_map(|i| i.id.strip_prefix("path:"))
        .map(str::to_lowercase)
        .collect();
    if let Some(items) = files.filter(|items| !items.is_empty() && engine.is_current(id)) {
        let chips = Vec::new();
        on_batch
            .send(Batch {
                id,
                source: "files",
                items,
                chips,
            })
            .map_err(|e| e.to_string())?;
    }
    // Words inside files come last: searched alongside file names they took the cores those
    // need, and took file results past their 30 ms budget.
    if !engine.is_current(id) {
        return Ok(());
    }
    let content_app = app.clone();
    let items = tauri::async_runtime::spawn_blocking(move || {
        content::search(&content_app, &parsed, &shown)
    })
    .await
    .map_err(|e| e.to_string())?;
    if !items.is_empty() && engine.is_current(id) {
        let chips = Vec::new();
        on_batch
            .send(Batch {
                id,
                source: "content",
                items,
                chips,
            })
            .map_err(|e| e.to_string())?;
    }
    // Browser history, then the words on the web, close the list.
    let Some(web) = app.try_state::<Web>() else {
        return Ok(());
    };
    if !text_filter_free(&text) || !engine.is_current(id) {
        return Ok(());
    }
    let mut items = web.history(&text);
    items.extend(web.fallback(&text));
    if !items.is_empty() {
        let chips = Vec::new();
        on_batch
            .send(Batch {
                id,
                source: "web",
                items,
                chips,
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// File filters mean the user wants files, not the web.
fn text_filter_free(text: &str) -> bool {
    filter::parse(text).filter.is_empty()
}

#[tauri::command]
pub fn run_action(
    app: AppHandle,
    item: String,
    action: String,
    query: String,
    ranker: State<'_, Ranker>,
) -> Result<(), String> {
    let owner = crate::panel_hwnd(&app).ok_or("panel window is gone")?;
    // Clipboard entries and windows act on themselves, and aren't recent results either.
    if let Some(clip) = item.strip_prefix("clip:") {
        tracing::info!("run {item} {action}");
        return clips::run(&app, clip, &action);
    }
    if let Some(glyph) = item.strip_prefix("emoji:") {
        tracing::info!("run emoji {action}");
        return emoji::run(&app, glyph, &action);
    }
    if let Some(window) = item.strip_prefix("win:") {
        tracing::info!("run window {action}");
        return switcher::run(&app, window, &action);
    }
    // A result is the user's own numbers: neither logged nor kept as a recent result.
    if let Some(result) = item.strip_prefix("calc:") {
        tracing::info!("run calc {action}");
        clipboard::set_text(owner, result)?;
        crate::hide_panel(&app, true);
        return Ok(());
    }
    // System commands and Settings pages learn from use, but "volume 40" is a one-off.
    if let Some(command) = item.strip_prefix("sys:") {
        if !command.contains(':') {
            let _ = ranker.record(&query, &item);
        }
        return system::run(&app, command, &action);
    }
    if let Some(page) = item.strip_prefix("setting:") {
        let _ = ranker.record(&query, &item);
        tracing::info!("open settings {page}");
        return system::open_setting(&app, page);
    }
    tracing::info!("run {item} {action}");
    if let Err(e) = ranker.record(&query, &item) {
        tracing::error!("could not record use of {item}: {e}");
    }
    match item.split_once(':') {
        Some(("app", id)) => apps::run(&app, id, &action, owner)?,
        Some(("path" | "url", _)) => direct::run(&app, &item, &action, owner)?,
        _ => builtin::run(&app, &item, &action)?,
    }
    // Copying or recycling leaves nothing new on screen, so focus goes back to where it was.
    let nothing_opened = matches!(
        action.as_str(),
        "copy" | "copyfile" | "recycle" | "addcontent"
    );
    crate::hide_panel(&app, nothing_opened);
    Ok(())
}

#[tauri::command]
pub fn toggle_pin(item: String, ranker: State<'_, Ranker>) -> Result<bool, String> {
    ranker.toggle_pin(&item).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_query_cancels_older() {
        let engine = Engine::default();
        engine.begin(2);
        engine.begin(1);
        assert!(engine.is_current(2));
        assert!(!engine.is_current(1));
    }
}
