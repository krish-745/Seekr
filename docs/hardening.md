# Hardening

Stress tests from Phase 7, what each found, and what changed. Each fix is its own commit.
Run on the development PC: Windows 11, Core i7, RTX 5050 Laptop GPU, NVMe C: with about
2.09 million names.

## 7.1 Stress tests

| Test | How | Result |
|---|---|---|
| OneDrive placeholders | The content indexer, in a scratch data folder, over the 8 OneDrive folders holding the 14 cloud-only files on this PC | Passed: none downloaded, none indexed; the 138 local files there were indexed |
| Junction loops | The content indexer over a folder with a junction to itself and one to its parent | Passed: finished in 0.2 s, nothing indexed through a junction |
| Long and Unicode paths | The same folder, with a 306-character path through Japanese, Cyrillic, Arabic, accented and emoji folder names | Passed: the file was indexed |
| File-name walk: junctions, long paths | `fsindex` unit tests (`reports_sizes_and_does_not_enter_junctions`, `long_path_prefixes`) | Passed |
| Malformed Office files | Garbage bytes and a truncated zip named .docx, a docx whose one part inflates to 1 GB, a docx nested a million elements deep, an xlsx with unclosed tags and invalid UTF-8 (`content` test `hostile_files`) | Passed: each refused or read in at most 35 ms |
| A deck of zip-bomb slides | A pptx of 300 slides, each inflating to 40 MB with no text | **Fixed**: each part was capped at 32 MB but the file wasn't, so a preview took 6.35 s of a core. All parts together now stop at 96 MB: 175 ms |
| Crash recovery: content indexer | 4,000 Markdown files; the indexer killed 0.4, 0.8 and 1.2 s into a run, then left to finish | **Fixed**: the files being read at each kill were quarantined for good, as if they had crashed it (2 of 4,000), which Seekr quitting or Windows shutting down mid-pass would also do. Now a file left mid-read gets one more try, and only a second stop on it quarantines it; a hang (a file taking over 30 s) is still quarantined at once. After the fix: all 4,000 indexed |
| Mass file churn: file names | A synthetic 2.02-million-name index, then 4 million temporary files created and deleted, as builds and installs do (`fsindex` test `churn`) | **Fixed**: deleted names kept their slots until Seekr restarted, so slots tripled to 6.02 million and a search for "rpt" went from 23.6 to 58.0 ms. The index now compacts when deleted slots pass a quarter of the live names, at its five-minute save: 6.02 to 2.02 million slots in about 1 s (searches wait for that second), and "rpt" back to 26.3 ms |
| Mass file churn: content | 3,000 indexed notes, then 5,000 created, 1,500 deleted, 1,000 moved between folders, 500 rewritten and a folder moved, reported as changed paths the way the app does | Passed: the index matched the disk exactly (6,500 files, none missing or stale). **Fixed** on the way: each deleted path reread every known path from the database, so deletions were quadratic: 3,000 of 12,000 files took 5.6 s, now 1.0 s. The state database also stops forcing a disk flush at every write (WAL with `synchronous = NORMAL`, still crash-safe) |
| Crash recovery: the app | The installed Seekr ended with `Stop-Process -Force`, then started again with `--hidden` | Passed: nothing of Seekr's was left running; it came back with the hotkey registered and 2.09 million names loaded from the cache in 0.43 s (the cache is written to a temporary file and renamed, so a kill mid-save can't damage it). The AI server ends with Seekr through a job object, tested in Phase 5 |
| A two-million-file volume | The real C: drive (2.09 million names, the Phase 2 gate) and the synthetic two-million-name `fsindex` benchmark | Covered there; no separate synthetic volume was made, since mounting one needs administrator rights |

Indexing files that were written a moment earlier is slow on this PC (16.9 s for 3,000 new
files, 2.4 s for the same files read again): the time goes to Windows Defender scanning each new
file as it is first opened, outside Seekr.

PDFs aren't read at all yet (no PDF text extraction is built), so malformed PDFs can't reach
the extractor.

## 7.3 Accessibility

Roles, names and live regions were added throughout the panel, high-contrast and reduced-motion
styles, and keyboard access to answer sources. `app/src/a11y.test.tsx` checks the results list,
the actions menu and the settings view with axe-core (no violations) and follows the selection
by keyboard through `aria-activedescendant`. Not yet tried with a running screen reader or a
high-contrast theme on screen; those need the panel driven on this PC.

## 7.2 Soak

`scripts/soak.ps1` samples Seekr's processes (CPU time, working set, private memory, handles,
threads), the GPU's power state and the battery every five minutes, to
`%LOCALAPPDATA%\Seekr\soak`; `scripts/soak.ps1 -Report` summarizes a run. A 72-hour run started
on 23 October 2026 at 01:57 with Seekr in normal use; its results go here when it ends.
