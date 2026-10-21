//! The content-search folder picker: suggestions (none pre-checked), the user's own additions,
//! and a native folder chooser. The choice is saved through `common::content`.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::AppHandle;
use windows::Win32::{
    Foundation::HWND,
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
    UI::Shell::{
        FOS_FORCEFILESYSTEM, FOS_PICKFOLDERS, FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH,
    },
};

use crate::fileops::Com;

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
pub fn save_content_folders(paths: Vec<String>) -> Result<(), String> {
    let folders: Vec<PathBuf> = paths.iter().map(PathBuf::from).collect();
    tracing::info!("content search folders: {}", folders.len());
    common::content::set_chosen(&folders).map_err(|e| e.to_string())
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
pub fn add(path: &str) -> Result<(), String> {
    common::content::add(Path::new(path)).map_err(|e| e.to_string())
}
