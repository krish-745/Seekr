# How Seekr was tested

The stress tests, accessibility checks, automated tests and installer tests behind 0.1.0, what
each found, and what was changed because of it. All of them ran on one PC: Windows 11, AMD Ryzen
7 250 with Radeon 780M, RTX 5050 Laptop GPU, and an NVMe C: drive with about 2.09 million names.

## Stress tests

| Test | How | Result |
|---|---|---|
| OneDrive placeholders | The content indexer, in a scratch data folder, over the 8 OneDrive folders holding the 14 cloud-only files on the test PC | Passed: none downloaded, none indexed; the 138 local files there were indexed |
| Junction loops | The content indexer over a folder with a junction to itself and one to its parent | Passed: finished in 0.2 s, nothing indexed through a junction |
| Long and Unicode paths | The same folder, with a 306-character path through Japanese, Cyrillic, Arabic, accented and emoji folder names | Passed: the file was indexed |
| File-name walk: junctions, long paths | `fsindex` unit tests (`reports_sizes_and_does_not_enter_junctions`, `long_path_prefixes`) | Passed |
| Malformed Office files | Garbage bytes and a truncated zip named .docx, a docx whose one part inflates to 1 GB, a docx nested a million elements deep, an xlsx with unclosed tags and invalid UTF-8 (`content` test `hostile_files`) | Passed: each refused or read in at most 35 ms |
| A deck of zip-bomb slides | A pptx of 300 slides, each inflating to 40 MB with no text | **Fixed**: each part was capped at 32 MB but the file wasn't, so a preview took 6.35 s of a core. All parts together now stop at 96 MB: 175 ms |
| Crash recovery: content indexer | 4,000 Markdown files; the indexer killed 0.4, 0.8 and 1.2 s into a run, then left to finish | **Fixed**: the files being read at each kill were quarantined for good, as if they had crashed it (2 of 4,000), which Seekr quitting or Windows shutting down mid-pass would also do. Now a file left mid-read gets one more try, and only a second stop on it quarantines it; a hang (a file taking over 30 s) is still quarantined at once. After the fix: all 4,000 indexed |
| Mass file churn: file names | A synthetic 2.02-million-name index, then 4 million temporary files created and deleted, as builds and installs do (`fsindex` test `churn`) | **Fixed**: deleted names kept their slots until Seekr restarted, so slots tripled to 6.02 million and a search for "rpt" went from 23.6 to 58.0 ms. The index now compacts when deleted slots pass a quarter of the live names, at its five-minute save: 6.02 to 2.02 million slots in about 1 s (searches wait for that second), and "rpt" back to 26.3 ms |
| Mass file churn: content | 3,000 indexed notes, then 5,000 created, 1,500 deleted, 1,000 moved between folders, 500 rewritten and a folder moved, reported as changed paths the way the app does | Passed: the index matched the disk exactly (6,500 files, none missing or stale). **Fixed** on the way: each deleted path reread every known path from the database, so deletions were quadratic: 3,000 of 12,000 files took 5.6 s, now 1.0 s. The state database also stops forcing a disk flush at every write (WAL with `synchronous = NORMAL`, still crash-safe) |
| Crash recovery: the app | The installed Seekr ended with `Stop-Process -Force`, then started again with `--hidden` | Passed: nothing of Seekr's was left running; it came back with the hotkey registered and 2.09 million names loaded from the cache in 0.43 s (the cache is written to a temporary file and renamed, so a kill mid-save can't damage it). The AI server ends with Seekr through a job object, as measured in [the AI benchmarks](benchmarks/llm.md) |
| A two-million-file volume | The real C: drive (2.09 million names) and the synthetic two-million-name `fsindex` benchmark | Covered there; no separate synthetic volume was made, since mounting one needs administrator rights |
| Service restart | `Restart-Service SeekrIndex` from an elevated PowerShell while Seekr ran, then a new file created and searched for | Passed: Seekr logged the dropped pipe ("C: file index unavailable") and reconnected by itself within its retry interval; the new file was found |
| Sleep and resume | The laptop asleep for about 2 h 20 min, then the panel opened and a new file searched for | Passed: the panel opened at once, file and app search worked, the new file was found; keystroke latency right after waking p50 14.8 ms, p95 39.8 ms |
| Elevated foreground window | Over an administrator PowerShell: the hotkey, an emoji (`:fire`, Enter), then launching Notepad | Passed: the panel opened over it; Windows doesn't let Seekr type into an elevated window, so the emoji was left on the clipboard with a "Copied — press Ctrl+V" toast, and Ctrl+V pasted it; Notepad opened |
| DPI change | Display scale changed and changed back, the panel opened at each | Passed: centered in the upper part of the screen, sized for the scale, sharp text. Not tried with a second monitor (none here); `panel_is_centered_in_upper_part` covers the placement math |

Indexing files that were written a moment earlier is slow on the test PC (16.9 s for 3,000 new
files, 2.4 s for the same files read again): the time goes to Windows Defender scanning each new
file as it is first opened, outside Seekr.

PDFs aren't read at all yet (no PDF text extraction is built), so malformed PDFs can't reach
the extractor.

## Accessibility

Roles, names and live regions were added throughout the panel, high-contrast and reduced-motion
styles, and keyboard access to answer sources. `app/src/a11y.test.tsx` checks the results list,
the actions menu and the settings view with axe-core (no violations) and follows the selection
by keyboard through `aria-activedescendant`, and the WebDriver smoke test (below) follows it in
the real WebView2. Not tried with a running screen reader (Narrator or NVDA) or a high-contrast
theme on screen.

## Automated tests

| Test | How | Result |
|---|---|---|
| WebDriver smoke test | `scripts/smoke.ps1` (also the last step of `check.ps1`): the release build under tauri-driver 2.1.0 and msedgedriver 155.0.4283.62, matching the installed WebView2, with its own data and config folders beside the installed Seekr | Passed 3 runs of 3, about 3 s of checks each: the calculator, arrow keys moving `aria-activedescendant`, Tab and Esc on the actions menu, the settings view and Diagnostics with the panel hidden; then the real hotkey, which showed and focused the panel, and Esc, which hid it and gave focus back to the window used before. Writing it found nothing in Seekr; it did show that a first focus asks for content folders over the results, so the test answers that in advance |
| Hotkey | The smoke test's last check, with Ctrl+Alt+Shift+F12 pressed through `keybd_event` once input has been idle for 2 s; key mapping for the hook fallback in `hotkey::tests` | Passed. The keyboard-hook fallback (used when another program owns the combination) isn't driven: it needs a second program holding the hotkey |
| Service and pipe | `cargo test -p service --test live -- --ignored` against the installed SeekrIndex, with Seekr quit (the pipe takes one client); the pipe's ACL and single-server rule in `security::tests`; the wire format in `fsindex` | Passed: 2,097,583 names in 1.8 s, a new file reported 0.6 ms after it was created, other profiles and the service's own logs hidden |

## Installer

| Test | How | Result |
|---|---|---|
| Install | `Seekr_0.1.0_x64-setup.exe` (26.3 MB) over an `install.ps1` install, run by hand | Passed: offered `%LOCALAPPDATA%\Programs\Seekr`, closed the running Seekr, one UAC prompt for the changed service; the Run entry, GPU preference, Apps entry and Start menu shortcut were written; app and file search worked; `seekr-svc current` then reported the service up to date |
| Reinstall, same version | The installer again, "Add/Reinstall components" | Passed: no UAC prompt, as the service was unchanged |
| Uninstall | Settings → Apps → Uninstall, keeping app data | Passed: one UAC prompt; the install folder and `C:\Program Files\Seekr` were removed (both found created anew by the following install), and `seekr.db`, the settings and the 750 MB model were kept |

## Long runs

`scripts/soak.ps1` samples Seekr's processes (CPU time, working set, private memory, handles,
threads), the GPU's power state and the battery every five minutes, to
`%LOCALAPPDATA%\Seekr\soak`, for as many hours as `-Hours` says; `scripts/soak.ps1 -Report`
summarizes a run. It shows memory or handles that grow over days, which shorter tests can't. A
run needs the PC on and awake throughout. No long run has been completed for 0.1.0 yet.
