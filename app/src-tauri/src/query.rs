//! Query engine: every keystroke is a query with a rising id, and a newer id cancels older work.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tauri::{AppHandle, State, ipc::Channel};

use crate::builtin;

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
    pub actions: Vec<Action>,
}

/// Results from one provider for query `id`. Each provider sends exactly one batch per query,
/// so the first batch of a new id tells the UI to replace the old list.
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
    id: u64,
    text: String,
    on_batch: Channel<Batch>,
    engine: State<'_, Engine>,
) -> Result<(), String> {
    engine.begin(id);
    let items = builtin::search(&text);
    if engine.is_current(id) {
        on_batch
            .send(Batch { id, items })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub fn run_action(app: AppHandle, item: String, action: String) -> Result<(), String> {
    tracing::info!("run {item} {action}");
    builtin::run(&app, &item, &action)?;
    crate::hide_panel(&app);
    Ok(())
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
