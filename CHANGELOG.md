# Changelog

## Unreleased

- Desktop apps get Open file location (Ctrl+Enter), Run as administrator and Copy path; Store apps only open.
- Typing a path (`C:\...`, `~\...`, `%WINDIR%\...`, `\server\share`) or a URL (`https://...`, `www....`) offers it as the first result, with Open, Show in folder and Copy.
- Esc, the hotkey and copy actions give focus back to the window you were in, and check that Windows did it.
- Ranking: fuzzy matching that favours word starts and acronyms ("vsc" finds Visual Studio Code), boosted by how often and how recently you chose a result for what you typed (14-day half-life). Results can be pinned to the top from the actions list. An empty search shows pins, then recently used results. Usage lives in `%LOCALAPPDATA%\Seekr\seekr.db`.
- Fixed: the first time the panel opened it was a title bar's height too tall.
- `SEEKR_DATA_DIR` overrides the data folder (logs, icons, database), for test runs.
- App search: every Start menu shortcut and Store app (shell:AppsFolder) is searchable by name and launches with Enter. Icons are rendered once into `%LOCALAPPDATA%\Seekr\icons`. The list is rescanned in the background at most once a minute when the panel opens, so new installs appear on the next open.
- If Windows can't report the cursor position, the panel opens on the primary monitor instead of failing.
- Panel layout: results list, preview pane and hint bar. Keys: Up/Down select, Enter runs the main action, Ctrl+Enter the second one, Tab or Ctrl+K lists all actions, Alt+1-9 runs a visible row, Esc closes the actions list or the panel.
- Seekr's own commands are searchable: Open Seekr config (creates a commented `config.toml` if missing), Open Seekr logs, Quit Seekr.
- Every keystroke starts a new query that supersedes older ones; results reach the screen at most once per frame.
- Latency telemetry: hotkey-to-ready and keystroke-to-painted times are logged as p50/p95/max every 50 samples.
- Strict content security policy in release builds.
- Turning Start at login on or off is logged.
- Search panel: a pre-created hidden window with a Mica backdrop that opens on Alt+Space (or the `hotkey` in `config.toml`) on the monitor under the cursor, sized for its DPI. Esc or clicking away hides it.
- Hotkey falls back to a low-level keyboard hook when another app already owns the combination. In that mode the hotkey opens the panel, but pressing it again while the panel is focused reaches the other app instead; use Esc.
- Tray icon with Open, Start at login and Quit. A second launch opens the running panel instead of starting another copy.
- Panics are written to `logs\<name>.panic.log`.
- Project setup: Cargo workspace, Tauri 2 + SolidJS app, `common` crate (paths, config, logs), check and dev scripts.
