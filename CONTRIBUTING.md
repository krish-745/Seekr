# Contributing

Bug reports, ideas and pull requests are welcome. Seekr is Windows-only (Windows 11, x64).

## Reporting a bug

Open an issue with the bug template. Settings → Diagnostics shows the numbers that help most:
index counts, recent latencies, quarantined files and the end of the log. Copy what is relevant
and leave out anything private; the log names a file only when it fails to index.

## Building

Requirements: Windows 11, Rust (stable, MSVC), Visual Studio Build Tools with the C++ workload,
Node.js 20+.

```powershell
scripts\dev.ps1       # run the app with hot reload
scripts\check.ps1     # every check a pull request must pass
scripts\install.ps1   # build and install for daily use
```

`check.ps1` runs, in order: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test`,
`cargo deny`, the third-party notices check, `tsc`, `vitest`, the release builds and the
WebDriver smoke test. The smoke test runs the release build beside your installed Seekr with its
own data and settings, and its last check briefly shows the panel by pressing a test hotkey.

## Pull requests

- One change per pull request, with a test when it changes behavior.
- Commit messages: an imperative, sentence-case subject that says what changed and, where it
  isn't obvious, why ("Compact the file-name index when deleted names pile up").
- Add a line to the Unreleased section of `CHANGELOG.md` for anything a user would notice.
- Keep the README honest: examples must work as written, and a removed feature leaves the docs
  too.
- A new dependency needs a license `deny.toml` allows; then run `scripts\notices.ps1` and commit
  the updated `THIRD-PARTY-NOTICES.md`.

## Privacy rules for code

Seekr reads a lot of personal data, so:

- Never log what the user types, searches for, opens or copies; log counts, timings and kinds.
- Nothing leaves the PC except what [PRIVACY.md](PRIVACY.md) lists. A change that sends
  something new over the network updates that page in the same pull request.
- API keys come from the user's environment and are never written to files, logs or exports.

## License

By contributing, you agree that your contributions are licensed under MIT or Apache-2.0, at the
user's option, the same as Seekr.
