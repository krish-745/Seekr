mod apps;
mod builtin;
mod clipboard;
mod content;
mod direct;
mod fileops;
mod files;
mod focus;
mod hotkey;
mod indexer;
mod preview;
mod query;
mod rank;
mod telemetry;

use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, WindowEvent,
    menu::{CheckMenuItem, Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt as _};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use telemetry::Telemetry;

/// Panel size in logical pixels, scaled to the target monitor's DPI on every show.
const PANEL: (f64, f64) = (760.0, 480.0);
/// Passed by the login entry so the panel stays hidden until the hotkey.
const HIDDEN_ARG: &str = "--hidden";

#[tauri::command]
fn hide(app: AppHandle) {
    hide_panel(&app, true);
}

/// `restore` hands focus back to the window used before the panel opened: right after Esc or
/// a copy, but not after launching something, which should keep the focus it takes.
pub(crate) fn hide_panel(app: &AppHandle, restore: bool) {
    if restore && !app.state::<focus::Focus>().restore() {
        tracing::warn!("focus didn't return to the previous window");
    }
    if let Some(w) = app.get_webview_window("main") {
        log_err("hide", w.hide());
    }
}

pub(crate) fn toggle(app: &AppHandle) {
    match app.get_webview_window("main") {
        Some(w) if w.is_visible().unwrap_or(false) => hide_panel(app, true),
        _ => show(app),
    }
}

/// WebView2 trims caches and memory when the target is low; the panel only needs that while hidden.
fn set_webview_memory(w: &tauri::WebviewWindow, low: bool) {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
        ICoreWebView2_19,
    };
    use windows::core::Interface;
    let level = if low {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
    } else {
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
    };
    let queued = w.with_webview(move |webview| {
        let set = unsafe {
            webview
                .controller()
                .CoreWebView2()
                .and_then(|core| core.cast::<ICoreWebView2_19>())
                .and_then(|core| core.SetMemoryUsageTargetLevel(level))
        };
        if let Err(e) = set {
            tracing::warn!("WebView2 memory target: {e}");
        }
    });
    log_err("WebView2 memory target", queued);
}

/// The panel's window handle, for Win32 calls that need an owner window.
pub(crate) fn panel_hwnd(app: &AppHandle) -> Option<windows::Win32::Foundation::HWND> {
    let hwnd = app.get_webview_window("main")?.hwnd().ok()?;
    Some(windows::Win32::Foundation::HWND(hwnd.0))
}

pub(crate) fn show(app: &AppHandle) {
    log_err("show", try_show(app));
}

fn try_show(app: &AppHandle) -> tauri::Result<()> {
    let Some(w) = app.get_webview_window("main") else {
        return Ok(());
    };
    // GetCursorPos fails while the desktop is unavailable (e.g. a UAC prompt); use the primary.
    let under_cursor = match app.cursor_position() {
        Ok(c) => app.monitor_from_point(c.x, c.y)?,
        Err(e) => {
            tracing::warn!("no cursor position ({e}); using the primary monitor");
            None
        }
    };
    let monitor = match under_cursor {
        Some(m) => Some(m),
        None => app.primary_monitor()?,
    };
    let mut size = None;
    if let Some(m) = monitor {
        let area = m.work_area();
        let (x, y, width, height) = place(
            (area.position.x, area.position.y),
            (area.size.width, area.size.height),
            m.scale_factor(),
        );
        // Move first so any DPI change between monitors lands before the final size is set.
        w.set_position(PhysicalPosition::new(x, y))?;
        w.set_size(PhysicalSize::new(width, height))?;
        w.set_position(PhysicalPosition::new(x, y))?;
        size = Some(PhysicalSize::new(width, height));
    }
    if let Some(panel) = panel_hwnd(app) {
        app.state::<focus::Focus>().remember(panel);
    }
    app.state::<Telemetry>().mark_shown();
    apps::refresh(app);
    set_webview_memory(&w, false);
    w.show()?;
    // The first show of a never-shown undecorated window comes out a caption-height too tall.
    if let Some(size) = size
        && w.inner_size()? != size
    {
        w.set_size(size)?;
    }
    w.set_focus()
}

/// Centered horizontally, a quarter of the free space above it, clamped to the work area.
fn place(pos: (i32, i32), size: (u32, u32), scale: f64) -> (i32, i32, u32, u32) {
    let width = ((PANEL.0 * scale).round() as u32).min(size.0);
    let height = ((PANEL.1 * scale).round() as u32).min(size.1);
    let x = pos.0 + ((size.0 - width) / 2) as i32;
    let y = pos.1 + ((size.1 - height) / 4) as i32;
    (x, y, width, height)
}

fn log_err<T>(what: &str, result: tauri::Result<T>) {
    if let Err(e) = result {
        tracing::error!("{what} failed: {e}");
    }
}

fn register_hotkey(app: &AppHandle) {
    let default = common::Config::default().hotkey;
    let hotkey = match common::Config::load() {
        Ok(cfg) => cfg.hotkey,
        Err(e) => {
            tracing::error!("config.toml ignored: {e}");
            default.clone()
        }
    };
    let shortcut: Shortcut = hotkey.parse().unwrap_or_else(|e| {
        tracing::error!("hotkey {hotkey:?} is invalid ({e}); using {default}");
        default.parse().expect("default hotkey parses")
    });
    match app.global_shortcut().register(shortcut) {
        Ok(()) => tracing::info!("hotkey {shortcut} registered"),
        Err(e) => {
            tracing::warn!("RegisterHotKey failed for {shortcut} ({e}); using a keyboard hook");
            match hotkey::install(shortcut, app.clone()) {
                Ok(()) => tracing::info!("hotkey {shortcut} hooked"),
                Err(e) => tracing::error!("no hotkey: {e}"),
            }
        }
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let at_login = app.autolaunch().is_enabled().unwrap_or(false);
    let open = MenuItem::with_id(app, "open", "Open Seekr", true, None::<&str>)?;
    let login =
        CheckMenuItem::with_id(app, "login", "Start at login", true, at_login, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &login, &quit])?;
    let icon = app
        .default_window_icon()
        .cloned()
        .expect("bundled app icon");
    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("Seekr")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(move |app, event| match event.id.as_ref() {
            "open" => show(app),
            "login" => set_autostart(app, &login),
            "quit" => app.exit(0),
            _ => {}
        })
        // Show, not toggle: clicking the tray takes focus from the panel, which already hides it.
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// The menu item has already flipped its check mark; make the Run key match it.
fn set_autostart(app: &AppHandle, item: &CheckMenuItem<tauri::Wry>) {
    let want = item.is_checked().unwrap_or(false);
    let autolaunch = app.autolaunch();
    let result = if want {
        autolaunch.enable()
    } else {
        autolaunch.disable()
    };
    match result {
        Ok(()) => tracing::info!("start at login {}", if want { "on" } else { "off" }),
        Err(e) => {
            tracing::error!("start at login: {e}");
            log_err("revert check", item.set_checked(!want));
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let _log_guard = common::init_logging("seekr").expect("failed to start logging");
    tauri::Builder::default()
        // Must be first: a second launch shows the running panel and exits.
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec![HIDDEN_ARG]),
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle(app);
                    }
                })
                .build(),
        )
        .manage(query::Engine::default())
        .manage(apps::AppIndex::default())
        .manage(focus::Focus::default())
        .manage(files::Files::default())
        .register_uri_scheme_protocol("icon", |_ctx, request| {
            let name = request.uri().path().trim_start_matches('/');
            let bytes = apps::is_icon_file(name)
                .then(|| std::fs::read(apps::icon_dir().join(name)).ok())
                .flatten();
            let response = tauri::http::Response::builder();
            match bytes {
                Some(png) => response.header("Content-Type", "image/png").body(png),
                None => response.status(404).body(Vec::new()),
            }
            .expect("static response parts are valid")
        })
        // Thumbnails take tens of milliseconds, so they are drawn off the webview's thread.
        .register_asynchronous_uri_scheme_protocol("thumb", |_ctx, request, responder| {
            let query = request.uri().query().unwrap_or("").to_string();
            std::thread::spawn(move || {
                let response = tauri::http::Response::builder();
                let response = match preview::thumbnail(&query) {
                    Some(png) => response.header("Content-Type", "image/png").body(png),
                    None => response.status(404).body(Vec::new()),
                };
                responder.respond(response.expect("static response parts are valid"));
            });
        })
        .manage(Telemetry::default())
        .manage(indexer::Indexer::default())
        .manage(content::Search::default())
        .setup(|app| {
            let db = common::data_dir().join("seekr.db");
            let ranker = rank::Ranker::open(&db).unwrap_or_else(|e| {
                tracing::error!("{} unusable ({e}); usage won't be remembered", db.display());
                rank::Ranker::in_memory().expect("in-memory SQLite")
            });
            app.manage(ranker);
            files::start(app.handle());
            indexer::start(app.handle());
            content::start_reaper(app.handle());
            register_hotkey(app.handle());
            apps::refresh(app.handle());
            build_tray(app)?;
            if !std::env::args().any(|a| a == HIDDEN_ARG) {
                show(app.handle());
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Alt+F4 or a WM_CLOSE would destroy the only window and quit; Quit is in the tray.
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                log_err("hide", window.hide());
            }
            if let WindowEvent::Focused(false) = event {
                log_err("hide", window.hide());
                window.state::<Telemetry>().flush();
                if let Some(w) = window.app_handle().get_webview_window("main") {
                    set_webview_memory(&w, true);
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            hide,
            query::query,
            query::run_action,
            query::toggle_pin,
            preview::preview,
            content::content_setup_needed,
            content::content_setup,
            content::save_content_folders,
            content::choose_folder,
            telemetry::panel_ready,
            telemetry::record_latency,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_is_centered_in_upper_part() {
        // 1920x1080 at 125% with a 60 px taskbar: the panel is 950x600.
        assert_eq!(place((0, 0), (1920, 1020), 1.25), (485, 105, 950, 600));
    }

    #[test]
    fn panel_follows_offset_monitor_and_clamps() {
        assert_eq!(
            place((-1280, 200), (1280, 400), 1.0),
            (-1020, 200, 760, 400)
        );
    }
}
