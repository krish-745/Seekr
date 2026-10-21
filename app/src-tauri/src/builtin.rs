//! Seekr's own commands: config, logs, quit.

use std::{fs, io::Write, path::PathBuf};

use tauri::AppHandle;
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
/// The panel opens its folder picker for this one; Rust never sees it.
const SETUP: Action = Action {
    id: "setup",
    label: "Choose folders",
};

const CONFIG_TEMPLATE: &str = "\
# Seekr settings. Every key is optional; changes apply after restarting Seekr.
# hotkey = \"Alt+Space\"
";

fn config_path() -> PathBuf {
    common::config_dir().join("config.toml")
}

fn logs_dir() -> PathBuf {
    common::data_dir().join("logs")
}

pub fn all() -> Vec<Item> {
    let config = config_path().display().to_string();
    let logs = logs_dir().display().to_string();
    vec![
        Item {
            id: "seekr:config".into(),
            title: "Open Seekr config".into(),
            subtitle: config.clone(),
            preview: format!("Edit {config} to change the hotkey.\n\n{CONFIG_TEMPLATE}"),
            icon: None,
            actions: vec![OPEN, REVEAL],
        },
        Item {
            id: "seekr:logs".into(),
            title: "Open Seekr logs".into(),
            subtitle: logs.clone(),
            preview: format!("Daily logs, latency summaries and panic reports live in {logs}."),
            icon: None,
            actions: vec![OPEN],
        },
        Item {
            id: "seekr:content".into(),
            title: "Choose content search folders".into(),
            subtitle: "Folders Seekr searches inside".into(),
            preview: "Pick the folders whose files Seekr reads for content and meaning search. Your notes folder is always included.".into(),
            icon: None,
            actions: vec![SETUP],
        },
        Item {
            id: "seekr:quit".into(),
            title: "Quit Seekr".into(),
            subtitle: "Stop Seekr until the next login or launch".into(),
            preview: "Closes the panel, tray icon and hotkey.".into(),
            icon: None,
            actions: vec![QUIT],
        },
    ]
}

pub fn run(app: &AppHandle, item: &str, action: &str) -> Result<(), String> {
    let opener = app.opener();
    match (item, action) {
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
