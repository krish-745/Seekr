# Changelog

## Unreleased

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
