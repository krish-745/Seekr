# Changelog

## Unreleased

- File actions: Open with…, Copy file (pastes into Explorer as a copy), Copy path, Open in Terminal (Windows Terminal, or PowerShell), Properties, and Move to Recycle Bin, which asks for a second Enter first.
- Alt+F4 or a close message on the panel hides it instead of quitting Seekr; Quit is in the tray and in Seekr's commands.
- File filters: `ext:` `kind:` `folder:` `in:` `dm:`/`modified:` `size:` `"exact"` and `-exclude` (see Search syntax in the README), shown as chips under the search box. Filters alone list the newest matches first, and queries with filters skip app results. Sizes and dates for C: come from a background walk after the service's snapshot (about 24 s); changes are re-read as they happen. The index cache format changed, so the first start after updating rebuilds it.
- File search covers every fixed drive. Drives the service doesn't read (other letters, FAT/exFAT), and C: when the service isn't installed, use a no-admin fallback: a low-priority walk that skips junctions and symlinks (about 21 s for 2.2 million names on C:), then a directory watcher for changes. Each drive is cached in `%LOCALAPPDATA%\Seekr\files-<letter>.idx` and walked again at startup.
- Whole-drive file search: every file and folder name on C: (from the SeekrIndex service) is searchable from the second character typed. One or two characters match as a substring; longer queries are fuzzy. Results favour your Desktop, Documents and Downloads, sink system and build folders, and learn from what you open. New files show up within milliseconds. The index is cached in `%LOCALAPPDATA%\Seekr\files-C.idx`, so restarts take about 0.3 s.
- SeekrIndex service (`seekr-svc.exe`, LocalSystem, installed to `C:\Program Files\Seekr` by `scripts\install.ps1` with one UAC prompt). It reads every file name on C: from the NTFS master file table, writes a snapshot only the signed-in user can read, then streams changes from the USN journal over a pipe only SYSTEM and that user can open. Other accounts' profiles and NTFS metadata files are left out.
- `scripts\install.ps1`: installs the release build to `%LOCALAPPDATA%\Programs\Seekr`, starts it hidden at login and sets the power-saving GPU preference; `-Uninstall` removes all of that and keeps your data.
- WebView2 is asked to use less memory while the panel is hidden.
- Desktop apps get Open file location (Ctrl+Enter), Run as administrator and Copy path; Store apps only open.
- Typing a path (`C:\...`, `~\...`, `%WINDIR%\...`, `\\server\share`) or a URL (`https://...`, `www....`) offers it as the first result, with Open, Show in folder and Copy.
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
