//! Settings import and export, index rebuilds, and the diagnostics view.

use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};
use windows::{
    Win32::{
        Foundation::HWND,
        System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree},
        UI::Shell::{
            FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FileOpenDialog, FileSaveDialog, IFileDialog,
            IFileOpenDialog, IFileSaveDialog, SIGDN_FILESYSPATH,
        },
    },
    core::{HSTRING, Interface},
};

use crate::{
    ai::Ai, apps::AppIndex, clips::Clips, fileops::Com, files::Files, indexer::Indexer, settings,
    telemetry::Telemetry,
};

/// Everything an export holds: config.toml as written (comments kept), the content folders and
/// theme.css. API keys live in the environment, never here.
#[derive(Serialize, Deserialize)]
struct Bundle {
    seekr_settings: u32,
    config: String,
    folders: Vec<String>,
    theme_css: Option<String>,
}

/// Runs a maintenance row of the settings view, and says what happened.
#[tauri::command]
pub async fn maintenance(app: AppHandle, key: String) -> Result<String, String> {
    let owner = crate::panel_hwnd(&app).map(|h| h.0 as isize);
    let worker = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let owner = owner.map(|h| HWND(h as _));
        match key.as_str() {
            "export" => export(owner),
            "import" => import(&worker, owner),
            "rebuild_content" => {
                ::content::request_rebuild().map_err(|e| e.to_string())?;
                worker.state::<Indexer>().refresh();
                tracing::info!("content index rebuild asked for");
                Ok("The content index is being rebuilt: every file is read again.".into())
            }
            "rebuild_files" => {
                for drive in worker.state::<Files>().counts().iter().map(|(d, _)| *d) {
                    let _ = fs::remove_file(crate::files::cache_path(drive));
                }
                tracing::info!("file-name caches dropped; restarting");
                worker.restart();
            }
            _ => Err(format!("{key} isn't a maintenance action")),
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    // The panel hid while a file dialog was open.
    crate::show(&app);
    result
}

fn export(owner: Option<HWND>) -> Result<String, String> {
    let Some(path) = dialog(owner, true) else {
        return Ok(String::new());
    };
    let bundle = Bundle {
        seekr_settings: 1,
        config: fs::read_to_string(settings::path()).unwrap_or_default(),
        folders: common::content::chosen()
            .unwrap_or_default()
            .iter()
            .map(|f| f.display().to_string())
            .collect(),
        theme_css: fs::read_to_string(settings::css_path()).ok(),
    };
    let text = serde_json::to_string_pretty(&bundle).map_err(|e| e.to_string())?;
    fs::write(&path, text).map_err(|e| e.to_string())?;
    tracing::info!("settings exported");
    Ok(format!("Saved to {}", path.display()))
}

fn import(app: &AppHandle, owner: Option<HWND>) -> Result<String, String> {
    let Some(path) = dialog(owner, false) else {
        return Ok(String::new());
    };
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let bundle = parse_bundle(&text)?;
    write_atomic(&settings::path(), &bundle.config)?;
    match &bundle.theme_css {
        Some(css) => write_atomic(&settings::css_path(), css)?,
        None => {
            let _ = fs::remove_file(settings::css_path());
        }
    }
    let folders: Vec<PathBuf> = bundle.folders.iter().map(PathBuf::from).collect();
    common::content::set_chosen(&folders).map_err(|e| e.to_string())?;
    settings::reload(app);
    app.state::<Indexer>().refresh();
    tracing::info!("settings imported");
    Ok(format!("Imported from {}", path.display()))
}

/// An export's contents, if its config is one Seekr would accept.
fn parse_bundle(text: &str) -> Result<Bundle, String> {
    let bundle: Bundle =
        serde_json::from_str(text).map_err(|_| "that isn't a Seekr settings export".to_string())?;
    if bundle.seekr_settings != 1 {
        return Err("that export is from a newer Seekr".into());
    }
    let config: common::Config =
        toml::from_str(&bundle.config).map_err(|e| format!("its config.toml has an error: {e}"))?;
    config.check()?;
    Ok(bundle)
}

fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    fs::create_dir_all(common::config_dir()).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, text).map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Windows' save or open dialog for a .json file; None when cancelled.
fn dialog(owner: Option<HWND>, save: bool) -> Option<PathBuf> {
    let _com = Com::init();
    unsafe {
        let dialog: IFileDialog = if save {
            let d: IFileSaveDialog =
                CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            d.cast().ok()?
        } else {
            let d: IFileOpenDialog =
                CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            d.cast().ok()?
        };
        let options = dialog.GetOptions().ok()?;
        dialog
            .SetOptions(options | FOS_FORCEFILESYSTEM | FOS_OVERWRITEPROMPT)
            .ok()?;
        dialog.SetDefaultExtension(&HSTRING::from("json")).ok()?;
        if save {
            dialog
                .SetFileName(&HSTRING::from("Seekr settings.json"))
                .ok()?;
        }
        dialog.Show(owner).ok()?;
        let item = dialog.GetResult().ok()?;
        let name = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let path = name.to_string().ok().map(PathBuf::from);
        CoTaskMemFree(Some(name.0 as _));
        path
    }
}

#[derive(Serialize)]
pub struct Row {
    label: String,
    value: String,
}

#[derive(Serialize)]
pub struct Section {
    title: &'static str,
    rows: Vec<Row>,
}

#[derive(Serialize)]
pub struct Diagnostics {
    sections: Vec<Section>,
    latency: Vec<crate::telemetry::Latency>,
    /// Quarantined files and why.
    quarantined: Vec<(String, String)>,
    /// The end of today's log.
    log: Vec<String>,
    logs: String,
}

fn row(label: impl Into<String>, value: impl Into<String>) -> Row {
    Row {
        label: label.into(),
        value: value.into(),
    }
}

#[tauri::command]
pub fn diagnostics(app: AppHandle) -> Diagnostics {
    let mut index = vec![row(
        "Apps",
        crate::apps::all(&app.state::<AppIndex>()).len().to_string(),
    )];
    for (drive, names) in app.state::<Files>().counts() {
        index.push(row(format!("File names on {drive}:"), names.to_string()));
    }
    let state = ::content::state::State::open(&::content::state_path());
    let (by_status, embedded) = state
        .as_ref()
        .ok()
        .and_then(|s| s.counts().ok())
        .unwrap_or_default();
    let folders = common::content::folders().len();
    let mut content = vec![row("Folders searched", folders.to_string())];
    for (status, count) in by_status {
        content.push(row(
            format!("Files {status:?}").to_lowercase(),
            count.to_string(),
        ));
    }
    content.push(row("Files with meaning vectors", embedded.to_string()));
    content.push(row(
        "Index size",
        format!("{:.1} MB", dir_size(&::content::dir()) as f64 / 1e6),
    ));
    content.push(row(
        "Indexing",
        match ::content::read_progress() {
            _ if ::content::is_paused() => "Paused".to_string(),
            Some(p) => format!("{} {}", p.phase, p.done),
            None => "Idle".to_string(),
        },
    ));
    let other = vec![
        row(
            "Clipboard entries",
            app.try_state::<Clips>()
                .map_or(0, |c| c.count())
                .to_string(),
        ),
        row(
            "Local AI model",
            app.try_state::<Ai>()
                .map_or("Not set up".to_string(), |ai| ai.status()),
        ),
        row(
            "config.toml",
            settings::problem().unwrap_or_else(|| "Applied".into()),
        ),
    ];
    let logs = common::data_dir().join("logs");
    Diagnostics {
        sections: vec![
            Section {
                title: "Indexes",
                rows: index,
            },
            Section {
                title: "Content search",
                rows: content,
            },
            Section {
                title: "Other",
                rows: other,
            },
        ],
        latency: app.state::<Telemetry>().summary(),
        quarantined: state
            .ok()
            .and_then(|s| s.quarantined().ok())
            .unwrap_or_default(),
        log: tail(&newest_log(&logs), 40),
        logs: logs.display().to_string(),
    }
}

fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir).map_or(0, |entries| {
        entries
            .flatten()
            .map(|e| match e.file_type() {
                Ok(t) if t.is_dir() => dir_size(&e.path()),
                _ => e.metadata().map_or(0, |m| m.len()),
            })
            .sum()
    })
}

/// The app's newest log file.
fn newest_log(dir: &Path) -> PathBuf {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("seekr.") && name.ends_with(".log") && !name.contains("panic")
        })
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
        .unwrap_or_default()
}

/// The last `n` lines of a file.
fn tail(path: &Path, n: usize) -> Vec<String> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..]
        .iter()
        .map(|l| l.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_only_seekr_exports_with_a_valid_config() {
        let good = r##"{"seekr_settings": 1, "config": "# mine\nclipboard_days = 7\n",
            "folders": ["C:\\Notes"], "theme_css": null}"##;
        let bundle = parse_bundle(good).unwrap();
        assert_eq!(
            (bundle.folders.len(), bundle.config.starts_with("# mine")),
            (1, true)
        );
        assert!(parse_bundle("{}").is_err());
        assert!(parse_bundle(&good.replace("clipboard_days = 7", "clipboard_days = 0")).is_err());
        assert!(
            parse_bundle(&good.replace("\"seekr_settings\": 1", "\"seekr_settings\": 2")).is_err()
        );
    }

    #[test]
    fn tails_the_end_of_a_file() {
        let path = std::env::temp_dir().join(format!("seekr-tail-{}.log", std::process::id()));
        fs::write(&path, "a\nb\nc\n").unwrap();
        assert_eq!(tail(&path, 2), ["b", "c"]);
        assert_eq!(tail(&path, 9), ["a", "b", "c"]);
        assert!(tail(Path::new("Z:/none.log"), 3).is_empty());
        fs::remove_file(path).unwrap();
    }
}
