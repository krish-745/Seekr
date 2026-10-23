# Builds the Windows installer, target\release\bundle\nsis\Seekr_<version>_x64-setup.exe: the app,
# the content indexer, the SeekrIndex service and DirectML.dll, with the hooks in
# app\src-tauri\windows\hooks.nsh. tauri.nsis.json is merged in only here, so other builds don't
# need the indexer and service built first.
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent

Push-Location (Join-Path $root 'app')
try {
    cargo build --release -p indexer -p service
    if ($LASTEXITCODE -ne 0) { throw "indexer and service build failed (exit $LASTEXITCODE)" }
    npm run tauri build -- --config src-tauri/tauri.nsis.json
    if ($LASTEXITCODE -ne 0) { throw "installer build failed (exit $LASTEXITCODE)" }
} finally { Pop-Location }
Get-ChildItem (Join-Path $root 'target\release\bundle\nsis\*.exe') | ForEach-Object {
    "{0} ({1:N1} MB)" -f $_.FullName, ($_.Length / 1MB)
}
