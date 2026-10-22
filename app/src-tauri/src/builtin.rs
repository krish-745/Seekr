//! Seekr's own commands: config, logs, quit.

use std::{fs, io::Write, path::PathBuf};

use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;

use crate::query::{Action, Item};

const OPEN: Action = Action {
    id: "open",
    label: "Open",
};
const REVEAL: Action = Action {
    id: "reveal",
    label: "Show in folder",
};
const QUIT: Action = Action {
    id: "quit",
    label: "Quit",
};
const TOGGLE: Action = Action {
    id: "toggle",
    label: "Pause or resume",
};
/// The panel types `clip ` for this one; Rust never sees it.
const HISTORY: Action = Action {
    id: "cliphistory",
    label: "Show",
};
/// The panel types `?` for this one; Rust never sees it.
const ASK_MODE: Action = Action {
    id: "askmode",
    label: "Ask",
};
/// The panel types `note ` or `todo ` for these; Rust never sees them.
const NOTES: Action = Action {
    id: "notesmode",
    label: "Show",
};
const TODOS: Action = Action {
    id: "todosmode",
    label: "Show",
};
/// The panel types `emoji ` for this one; Rust never sees it.
const EMOJI: Action = Action {
    id: "emojimode",
    label: "Show",
};
/// The panel types `win ` for this one; Rust never sees it.
const WINDOWS: Action = Action {
    id: "switchwindows",
    label: "Show",
};
/// The panel opens its folder picker for this one; Rust never sees it.
const SETUP: Action = Action {
    id: "setup",
    label: "Choose folders",
};

/// The panel opens its settings view for this one; Rust never sees it.
const SETTINGS: Action = Action {
    id: "settings",
    label: "Open",
};

pub(crate) const CONFIG_TEMPLATE: &str = "\
# Seekr settings. Every key is optional; saved changes apply within two seconds.
# The Settings command in Seekr changes most of these for you.
# hotkey = \"Alt+Space\"
#
# The web shortcut for \"Search the web\": g, ddg, b, or one of yours.
# search = \"ddg\"
#
# AI: ? asks about your files with the model on this PC (ai_model picks it); ?? asks a cloud
# model anything, sending only the question. ?? needs GEMINI_API_KEY or GROQ_API_KEY in your
# environment; api picks one when both are set, api_model its model.
# ai_model = \"Qwen3.5-9B-IQ4_XS\"
# api = \"gemini\"
# api_model = \"gemini-3.5-flash\"
# The local model unloads after this many minutes without a question.
# ai_idle_minutes = 10
#
# Clipboard history keeps entries this many days, and at most this many (pinned ones stay).
# clipboard_days = 30
# clipboard_entries = 1000
#
# Left out of file and content results: folder paths, or names such as node_modules.
# exclude = [\"node_modules\", 'C:\\Users\\you\\Downloads\\Big']
#
# Index meaning on battery too; by default a big pass waits for mains power.
# embed_on_battery = true
#
# Web shortcuts: type the key, a space and your words. %s is where the words go.
# [shortcuts]
# jira = \"https://example.atlassian.net/browse/%s\"
#
# Tools to turn off: calculator, clipboard, windows, system, web, emoji, notes, ai.
# [tools]
# emoji = false
#
# The words that open a list: clipboard, windows, emoji, note, todo.
# [prefixes]
# clipboard = \"cb\"
";

fn config_path() -> PathBuf {
    common::config_dir().join("config.toml")
}

fn logs_dir() -> PathBuf {
    common::data_dir().join("logs")
}

/// Seekr's commands, without those that open a tool turned off in config.toml.
pub fn all() -> Vec<Item> {
    let tools = crate::settings::get().tools.clone();
    let mut items = commands();
    items.retain(|item| match item.id.as_str() {
        "seekr:ask" => tools.ai,
        "seekr:notes" | "seekr:todos" => tools.notes,
        "seekr:emoji" => tools.emoji,
        "seekr:windows" => tools.windows,
        "seekr:clipboard" => tools.clipboard,
        _ => true,
    });
    items
}

fn commands() -> Vec<Item> {
    let config = config_path().display().to_string();
    let logs = logs_dir().display().to_string();
    vec![
        Item {
            id: "seekr:settings".into(),
            title: "Seekr settings".into(),
            subtitle: "Hotkey, tools, prefixes, exclusions, clipboard, AI".into(),
            preview: "Change Seekr's settings here; they are saved to config.toml and apply at once.".into(),
            icon: None,
            line: None,
            actions: vec![SETTINGS],
        },
        Item {
            id: "seekr:config".into(),
            title: "Open Seekr config".into(),
            subtitle: config.clone(),
            preview: format!("Edit {config}; saved changes apply within two seconds.\n\n{CONFIG_TEMPLATE}"),
            icon: None,
            line: None,
            actions: vec![OPEN, REVEAL],
        },
        Item {
            id: "seekr:logs".into(),
            title: "Open Seekr logs".into(),
            subtitle: logs.clone(),
            preview: format!("Daily logs, latency summaries and panic reports live in {logs}."),
            icon: None,
            line: None,
            actions: vec![OPEN],
        },
        Item {
            id: "seekr:content".into(),
            title: "Choose content search folders".into(),
            subtitle: "Folders Seekr searches inside".into(),
            preview: "Pick the folders whose files Seekr reads for content and meaning search. Your notes folder is always included.".into(),
            icon: None,
            line: None,
            actions: vec![SETUP],
        },
        indexing(),
        Item {
            id: "seekr:ask".into(),
            title: "Ask AI".into(),
            subtitle: "? asks about your files, ?? asks anything".into(),
            preview: "? answers from the files content search reads, citing the passages it used, with a model on this PC that starts on the first question and stops after ten idle minutes. ?? answers anything with a cloud model (Gemini or Groq, once its key is in your environment), sending only your question.".into(),
            icon: None,
            line: None,
            actions: vec![ASK_MODE],
        },
        Item {
            id: "seekr:notes".into(),
            title: "Notes".into(),
            subtitle: "Type note and some text to save it".into(),
            preview: "Notes are Markdown files in Documents\\Seekr Notes, searchable by their words and meaning like any file there. note lists recent ones; Tab offers Edit here, which edits a note in this pane (Ctrl+S saves, Esc cancels).".into(),
            icon: None,
            line: None,
            actions: vec![NOTES],
        },
        Item {
            id: "seekr:todos".into(),
            title: "To-dos".into(),
            subtitle: "Type todo, a task and a due date".into(),
            preview: "todo call the bank by friday adds a to-do due on Friday; todo alone shows the list, soonest due first. Enter marks one done. They live in Documents\\Seekr Notes\\To-dos.md as a Markdown checklist.".into(),
            icon: None,
            line: None,
            actions: vec![TODOS],
        },
        Item {
            id: "seekr:emoji".into(),
            title: "Search emoji & symbols".into(),
            subtitle: "Type : and a word, like :fire or :rupee".into(),
            preview: "Emoji (Unicode 16) by name, keyword and shortcode, and symbols: currencies, arrows, math, Greek letters, fractions and more. Enter pastes into the window you were in, Ctrl+Enter copies; emoji with skin tones list them under Tab.".into(),
            icon: None,
            line: None,
            actions: vec![EMOJI],
        },
        Item {
            id: "seekr:windows".into(),
            title: "Switch windows".into(),
            subtitle: "Type win and part of a title or program".into(),
            preview: "Lists the windows Alt+Tab would show, most recently used first. Enter switches to one; Tab offers Minimize and Close window.".into(),
            icon: None,
            line: None,
            actions: vec![WINDOWS],
        },
        Item {
            id: "seekr:clipboard".into(),
            title: "Clipboard history".into(),
            subtitle: "Type clip and a few words to search it".into(),
            preview: "Seekr keeps what you copy (text, images and files) for 30 days or 1,000 entries; pinned entries stay until you unpin them. Copies a password manager marks as private are skipped. Enter pastes into the window you were in, Ctrl+Enter copies.".into(),
            icon: None,
            line: None,
            actions: vec![HISTORY],
        },
        Item {
            id: "seekr:quit".into(),
            title: "Quit Seekr".into(),
            subtitle: "Stop Seekr until the next login or launch".into(),
            preview: "Closes the panel, tray icon and hotkey.".into(),
            icon: None,
            line: None,
            actions: vec![QUIT],
        },
    ]
}

/// Pause or resume content indexing, with what the indexer is doing now.
fn indexing() -> Item {
    let paused = content::is_paused();
    let status = match content::read_progress() {
        Some(p) if p.phase == "embedding" => {
            format!(
                "Embedding passages: {} of {}",
                p.done,
                p.total.unwrap_or(p.done)
            )
        }
        Some(p) => format!("Reading files: {} checked", p.done),
        None if paused => "Paused; files that change wait until you resume".into(),
        None => "Up to date".into(),
    };
    Item {
        id: "seekr:indexing".into(),
        title: if paused {
            "Resume content indexing".into()
        } else {
            "Pause content indexing".into()
        },
        subtitle: status,
        preview: "Content indexing reads the files in your content-search folders in the background. Pausing stops it at the next file; resuming catches up on everything that changed. Bulk embedding also waits while the laptop is on battery.".into(),
        icon: None,
        line: None,
        actions: vec![TOGGLE],
    }
}

pub fn run(app: &AppHandle, item: &str, action: &str) -> Result<(), String> {
    let opener = app.opener();
    match (item, action) {
        ("seekr:indexing", "toggle") => {
            let resume = content::is_paused();
            content::set_paused(!resume).map_err(|e| e.to_string())?;
            tracing::info!(
                "content indexing {}",
                if resume { "resumed" } else { "paused" }
            );
            if resume {
                app.state::<crate::indexer::Indexer>().refresh();
            }
            return Ok(());
        }
        ("seekr:config", "open") => {
            ensure_config()?;
            opener.open_path(config_path().display().to_string(), None::<&str>)
        }
        ("seekr:config", "reveal") => {
            ensure_config()?;
            opener.reveal_item_in_dir(config_path())
        }
        ("seekr:logs", "open") => opener.open_path(logs_dir().display().to_string(), None::<&str>),
        ("seekr:quit", "quit") => {
            app.exit(0);
            Ok(())
        }
        _ => return Err(format!("unknown action {action} on {item}")),
    }
    .map_err(|e| e.to_string())
}

/// Creates the commented template once; never overwrites a config the user wrote.
fn ensure_config() -> Result<(), String> {
    let path = config_path();
    fs::create_dir_all(common::config_dir()).map_err(|e| e.to_string())?;
    match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => f
            .write_all(CONFIG_TEMPLATE.as_bytes())
            .map_err(|e| e.to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_handled_action() {
        for item in all() {
            assert!(!item.actions.is_empty(), "{}", item.id);
        }
    }

    #[test]
    fn template_is_a_valid_empty_config() {
        let path = std::env::temp_dir().join(format!("seekr-template-{}.toml", std::process::id()));
        fs::write(&path, CONFIG_TEMPLATE).unwrap();
        let cfg = common::Config::load_from(&path);
        fs::remove_file(path).unwrap();
        assert_eq!(cfg.unwrap(), common::Config::default());
    }
}
