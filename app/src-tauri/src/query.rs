//! Query engine: every keystroke is a query with a rising id, and a newer id cancels older work.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tauri::{AppHandle, Manager, State, ipc::Channel};

use crate::{apps, builtin, direct, files, rank::Ranker};

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
    pub actions: Vec<Action>,
}

/// Results from one provider for query `id`. Each provider sends one batch per query (none once
/// the query is superseded), so the first batch of a new id tells the UI to replace the old list.
#[derive(Serialize)]
pub struct Batch {
    pub id: u64,
    pub items: Vec<Item>,
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
    // Apps and Seekr's own commands are fast, so they are ranked together into the first batch;
    // slower providers will append later batches.
    let mut candidates = apps::all(&index);
    candidates.extend(builtin::all());
    // A typed path or URL is what the user meant, so it leads, unranked.
    let mut items = direct::items(&text);
    items.extend(ranker.rank(&text, candidates));
    if !engine.is_current(id) {
        return Ok(());
    }
    on_batch
        .send(Batch { id, items })
        .map_err(|e| e.to_string())?;
    // File names take milliseconds across all cores, so they follow as a second batch.
    let searcher = app.clone();
    let files = tauri::async_runtime::spawn_blocking(move || {
        let engine = searcher.state::<Engine>();
        files::search(&searcher, &text, &|| !engine.is_current(id))
    })
    .await
    .map_err(|e| e.to_string())?;
    if let Some(items) = files.filter(|items| !items.is_empty() && engine.is_current(id)) {
        on_batch
            .send(Batch { id, items })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn run_action(
    app: AppHandle,
    item: String,
    action: String,
    query: String,
    ranker: State<'_, Ranker>,
) -> Result<(), String> {
    tracing::info!("run {item} {action}");
    if let Err(e) = ranker.record(&query, &item) {
        tracing::error!("could not record use of {item}: {e}");
    }
    let owner = crate::panel_hwnd(&app).ok_or("panel window is gone")?;
    match item.split_once(':') {
        Some(("app", id)) => apps::run(&app, id, &action, owner)?,
        Some(("path" | "url", _)) => direct::run(&app, &item, &action, owner)?,
        _ => builtin::run(&app, &item, &action)?,
    }
    // Copying leaves nothing new on screen, so focus goes back to where the user was.
    crate::hide_panel(&app, action == "copy");
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
