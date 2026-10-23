# Privacy

Seekr indexes your PC and keeps everything it learns on your PC. It has no account, no
analytics and no telemetry server. This page lists everything it stores and every time it uses
the network.

## What stays on your PC

In `%LOCALAPPDATA%\Seekr` (data) and `%APPDATA%\Seekr` (settings):

- **File names** on your fixed drives, with sizes and dates: the SeekrIndex service reads them
  from the NTFS change journal and gives your account only the names you could open yourself;
  other people's profiles on the PC stay hidden. A copy is cached in `files-C.idx` (one per drive).
- **The text inside files** in the folders you choose (asked the first time the panel opens),
  with meaning vectors, in `content`. Nothing outside those folders is read for its text.
- **What you launch**, as counts and times in `seekr.db`, so your usual picks rank first.
- **Clipboard history** in `clipboard.db` (images in `clips`): text, images and files you copy, kept for 30 days and at
  most 1,000 entries (both in Settings), and off entirely if you turn the clipboard tool off.
  Copies a password manager marks as private are never recorded, the way Windows' own clipboard
  history skips them.
- **Browser bookmarks and history** from Brave, Chrome and Edge are read to search them and
  kept in memory. History is read from a temporary copy in `%TEMP%`, deleted right after.
- **Notes and to-dos** you write, in `Documents\Seekr Notes`.
- **Logs** in `logs`: startup times, index counts, errors, and latency figures. They don't
  record what you type, search for or open (only the kind, such as "run path open"); a file is
  named only when it fails to index.
- **AI models**, once downloaded: the meaning-search model in `models` and the `?` model in
  `llm`.

Uninstalling with `scripts\install.ps1 -Uninstall` keeps these folders and your notes; delete
them to remove everything.

## When Seekr uses the network

| When | Where | What is sent |
|---|---|---|
| You ask a general question with `??` | Groq (`api.groq.com`) or Google's Gemini API, whichever key you set | Your question and the earlier `??` questions and answers of that conversation. Never your files, file names or `?` answers |
| Once a day, unless "Check for updates" is off | `github.com` (this project's releases) | A request for the latest version's description; an update downloads only when you choose to install it, and its signature is checked first |
| You convert a currency, at most once a day | `api.frankfurter.dev` (European Central Bank rates) | A request for the day's rates; nothing about you or the amount |
| You press Enter to download the `?` model | GitHub (llama.cpp releases) and Hugging Face | Ordinary download requests |
| The installer fetches the meaning-search model | Hugging Face | Ordinary download requests |
| You pick "Search the web" or a web shortcut | Your browser opens the search page | What you typed, to that site, by your browser |

`?` questions about your files are answered by the model on your GPU, so neither the
question nor your files leave the PC. API keys are read from your Windows environment and are
never written to Seekr's files, logs or exported settings.
