# Seekr

A fast keyboard launcher for Windows: apps, files, file contents and meaning, plus everyday tools, all on one hotkey.

**Status:** early development (Phase 3: content search). Alt+Space opens the search panel: installed apps (Start menu and Store), every file and folder name on your fixed drives, and the text inside files in the folders you choose (asked the first time the panel opens), matched by words and by meaning. Tools and AI come later.

## Install

Build and install for daily use. This copies the app to `%LOCALAPPDATA%\Programs\Seekr` (starts hidden at login, power-saving GPU), downloads the meaning-search model (about 750 MB, once) to `%LOCALAPPDATA%\Seekr\models`, and installs the SeekrIndex service to `C:\Program Files\Seekr`, which asks for one UAC approval:

```powershell
scripts\install.ps1             # build, install, start
scripts\install.ps1 -NoBuild    # install the existing target\release build
scripts\install.ps1 -SkipService  # app only, no UAC prompt; C: then uses the slower no-admin scan
scripts\install.ps1 -Uninstall  # remove the app; keeps your data in %LOCALAPPDATA%\Seekr and %APPDATA%\Seekr
```

Then press Alt+Space.

## Search syntax

File names match fuzzily from the second character. Add filters anywhere in the query; they show as chips under the search box, and a chip turns red when its value isn't understood. With only filters, the newest matches come first.

| Filter | Example | Matches |
|---|---|---|
| `ext:` | `ext:pdf,docx` | files with any of these extensions |
| `kind:` | `kind:image` | `folder`, `file`, `image`, `video`, `audio`, `doc`, `code`, `archive` |
| `folder:` | `folder:` | folders only (`folder:music` is the same as `in:music`) |
| `in:` | `in:downloads`, `in:C:\Work` | inside a folder whose name contains the text, or inside that exact path |
| `dm:` / `modified:` | `dm:today`, `dm:week`, `dm:3d`, `dm:2025-05`, `dm:>2025-01-01`, `dm:2025-01..2025-03` | last written in that period (local time) |
| `size:` | `size:>100mb`, `size:<1kb`, `size:1mb..10mb`, `size:empty` | file size; a bare `size:10mb` means at least 10 MB |
| `"…"` | `"read me"` | names containing exactly that text |
| `-` | `-draft` | names not containing that text |

## Calculator

Type a calculation and its result comes first; Enter copies it.

| Type | Examples |
|---|---|
| Math, units, bases | `125*48+7`, `sqrt(2)*10`, `5 km to miles`, `255 to hex`, `0xff to decimal` |
| Percentages | `150 + 18%`, `80 - 25%`, `20% of 150` |
| Currency (ECB daily rates, rupees by default) | `10 usd`, `$10`, `250 gbp to eur`, `₹500 to usd` |
| Time zones | `time in tokyo`, `london time`, `3pm ist to pst`, `9am in new york` |
| Dates | `today + 30 days`, `10 days ago`, `days until 25 dec`, `2026-12-25 - today`, `dec 25` |

## Clipboard history

Type `clip ` (with the space) to list what you copied, newest first, or `clip invoice` to find entries containing "invoice". Enter pastes into the window you were in, Ctrl+Enter copies; Tab lists Paste as plain text, Pin and Delete. Into an admin window, Seekr can only copy: press Ctrl+V yourself.

## Develop

Requirements: Windows 11, Rust (stable, MSVC), Visual Studio Build Tools with the C++ workload, Node.js 20+.

```powershell
scripts\dev.ps1     # run the app with hot reload
scripts\check.ps1   # fmt, clippy, tests, cargo-deny, typecheck, vitest, release build
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
