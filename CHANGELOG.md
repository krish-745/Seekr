# Changelog

## Unreleased

- Search panel: a pre-created hidden window with a Mica backdrop that opens on Alt+Space (or the `hotkey` in `config.toml`) on the monitor under the cursor, sized for its DPI. Esc or clicking away hides it.
- Hotkey falls back to a low-level keyboard hook when another app already owns the combination. In that mode the hotkey opens the panel, but pressing it again while the panel is focused reaches the other app instead; use Esc.
- Tray icon with Open, Start at login and Quit. A second launch opens the running panel instead of starting another copy.
- Panics are written to `logs\<name>.panic.log`.
- Project setup: Cargo workspace, Tauri 2 + SolidJS app, `common` crate (paths, config, logs), check and dev scripts.
