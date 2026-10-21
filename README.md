# Seekr

A fast keyboard launcher for Windows: apps, files, file contents and meaning, plus everyday tools, all on one hotkey.

**Status:** early development (Phase 1: launcher core). Alt+Space opens the search panel and launches installed apps (Start menu and Store); files, tools and AI come later.

## Install

Build and install for daily use. This copies the app to `%LOCALAPPDATA%\Programs\Seekr` (starts hidden at login, power-saving GPU) and installs the SeekrIndex service to `C:\Program Files\Seekr`, which asks for one UAC approval:

```powershell
scripts\install.ps1             # build, install, start
scripts\install.ps1 -NoBuild    # install the existing target\release build
scripts\install.ps1 -Uninstall  # remove the app; keeps your data in %LOCALAPPDATA%\Seekr and %APPDATA%\Seekr
```

Then press Alt+Space.

## Develop

Requirements: Windows 11, Rust (stable, MSVC), Visual Studio Build Tools with the C++ workload, Node.js 20+.

```powershell
scripts\dev.ps1     # run the app with hot reload
scripts\check.ps1   # fmt, clippy, tests, cargo-deny, typecheck, vitest, release build
```

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
