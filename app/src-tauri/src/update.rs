//! Updates from GitHub Releases, signed with the maintainer's key (the updater `pubkey` in
//! tauri.conf.json): looked for once a day unless `check_updates` is off, and installed only when
//! asked, by the installer in passive mode, which closes Seekr and starts it again.

use std::{sync::RwLock, time::Duration};

use tauri::AppHandle;
use tauri_plugin_updater::UpdaterExt;

use crate::query::{Action, Item};

/// Not right at login, when the network may not be up yet.
const FIRST_AFTER: Duration = Duration::from_secs(10 * 60);
const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// A newer version found by the last check.
static AVAILABLE: RwLock<Option<String>> = RwLock::new(None);

pub fn start(app: &AppHandle) {
    let app = app.clone();
    let spawned = std::thread::Builder::new()
        .name("updates".into())
        .spawn(move || {
            std::thread::sleep(FIRST_AFTER);
            loop {
                if crate::settings::get().check_updates
                    && let Err(e) = tauri::async_runtime::block_on(check(&app))
                {
                    tracing::info!("no update check: {e}");
                }
                std::thread::sleep(EVERY);
            }
        });
    if let Err(e) = spawned {
        tracing::error!("no update checks: {e}");
    }
}

async fn check(app: &AppHandle) -> Result<Option<String>, String> {
    let update = app
        .updater()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())?;
    let version = update.map(|u| u.version);
    if let Some(v) = &version {
        tracing::info!("Seekr {v} is available");
    }
    AVAILABLE.write().unwrap().clone_from(&version);
    Ok(version)
}

/// The tray's "Check for updates", answered with a toast.
pub fn check_now(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        crate::paste::toast(&match check(&app).await {
            Ok(Some(v)) => format!("Seekr {v} is available: type update"),
            Ok(None) => "Seekr is up to date".into(),
            Err(e) => {
                tracing::warn!("update check failed: {e}");
                "Couldn't check for updates".into()
            }
        });
    });
}

/// A row for the newer version, once one has been found.
pub fn item() -> Option<Item> {
    let version = AVAILABLE.read().unwrap().clone()?;
    Some(Item {
        id: "seekr:update".into(),
        title: format!("Update Seekr to {version}"),
        subtitle: "Downloads the signed installer from GitHub; Seekr restarts".into(),
        preview: format!(
            "Seekr {version} is available. Enter downloads it from github.com/krish-745/Seekr, \
             checks its signature, and installs it; Seekr closes and starts again. Your settings \
             and indexes are kept."
        ),
        icon: None,
        line: None,
        actions: vec![Action {
            id: "install",
            label: "Install",
        }],
    })
}

pub fn install(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        crate::paste::toast("Downloading the update…");
        let result = async {
            let update = app
                .updater()
                .map_err(|e| e.to_string())?
                .check()
                .await
                .map_err(|e| e.to_string())?
                .ok_or("no update any more")?;
            tracing::info!("installing Seekr {}", update.version);
            // On Windows the installer takes over from here and ends this process.
            update
                .download_and_install(|_, _| {}, || {})
                .await
                .map_err(|e| e.to_string())
        }
        .await;
        if let Err(e) = result {
            tracing::warn!("update failed: {e}");
            crate::paste::toast("The update didn't install");
        }
    });
}
