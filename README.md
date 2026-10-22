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

## Window switcher

Type `win ` to list open windows in Alt+Tab order, or `win code` for those whose title or program contains "code". Enter switches; Ctrl+Enter minimizes; Tab adds Close window.

## System controls

Type what you want: `lock`, `sleep`, `restart`, `shut down`, `sign out`, `mute`, `volume up`, `volume 40`, `brightness 70`, `wifi`, `bluetooth`, `dark`, `empty recycle bin`, `close frozen apps`, or any Settings page (`display settings`, `startup apps`). Restart, Shut down, Sign out, Empty Recycle Bin and Close frozen apps ask for a second Enter.

## Web

`g tauri plugins` searches Google, `yt lofi` YouTube, `gh seekr` GitHub (also `ddg`, `b`, `wiki`, `maps`, `amz`, `so`, `r`, `npm`, `crates`, `mdn`, `imdb`, `tr`). Add your own in `%APPDATA%\Seekr\config.toml`:

```toml
search = "ddg"   # the engine for "Search the web"

[shortcuts]
jira = "https://example.atlassian.net/browse/%s"
```

Bookmarks and history from Brave, Chrome and Edge show up as you type; the last row searches the web.

## Emoji and symbols

Type `:` and a word: `:fire`, `:laugh`, `:indian rupee`, `:double arrow`, `:greek small pi`, `:half`. Enter pastes, Ctrl+Enter copies, and Tab lists skin tones where an emoji has them.

## Notes and to-dos

`note pick up the parcel from the post office` saves a note in `Documents\Seekr Notes`; `note` lists recent notes, and Tab → Edit here edits one in place (Ctrl+S saves). `todo call the bank by friday` adds a to-do due on Friday; `todo` shows the list, and Enter marks one done.

## Ask AI

- `?` and a question asks about **your files**: `?when is the rent due`. The answer cites the passages it used; click a number to open that file. A model on your GPU answers, so nothing leaves your PC; it stops after ten idle minutes.
- `??` and a question asks **anything**: `??explain big-O notation`. A cloud model answers, and only what you type after `??` is sent, never your files.
- On a text, Markdown, code, Word, PowerPoint or Excel file, Tab → **Summarize with AI** summarizes it with the model on your GPU; `?` follow-ups then ask about that file. A long file is cut to its first 16,000 characters, and the model is told so.

Follow up with another `?` (or `??`) question; Esc stops an answer.

The model for `?` isn't part of the install. The first time, the `?` row says so: press Enter to download it (llama.cpp's server, its CUDA runtime and Qwen3.5-4B: 3.3 GB, each file checked against its SHA-256). It needs an NVIDIA GPU and takes 3.5 GB in `%LOCALAPPDATA%\Seekr\llm`. Esc stops the download; a stopped file starts over next time.

`??` needs a Gemini or Groq key in your own environment (never in a file you share); both have free tiers:

```powershell
[Environment]::SetEnvironmentVariable('GROQ_API_KEY', 'your-key', 'User')   # or GEMINI_API_KEY
```

With both set, `api = "groq"` in `%APPDATA%\Seekr\config.toml` picks Groq; `api_model` picks the model (defaults: `openai/gpt-oss-120b` on Groq, `gemini-3.5-flash` on Gemini).

## Develop

Requirements: Windows 11, Rust (stable, MSVC), Visual Studio Build Tools with the C++ workload, Node.js 20+.

```powershell
scripts\dev.ps1     # run the app with hot reload
scripts\check.ps1   # fmt, clippy, tests, cargo-deny, typecheck, vitest, release build
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
