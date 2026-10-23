# Builds the Windows installer, target\release\bundle\nsis\Seekr_<version>_x64-setup.exe: the app,
# the content indexer, the SeekrIndex service, DirectML.dll and pdfium.dll, with the hooks in
# app\src-tauri\windows\hooks.nsh. tauri.nsis.json is merged in only here, so other builds don't
# need the indexer and service built first.
#
# The installer is signed for the updater with %USERPROFILE%\.tauri\seekr-updater.key (or
# TAURI_SIGNING_PRIVATE_KEY), asking for its password, and latest.json is written beside it for
# the GitHub release. -Unsigned skips both, for a test build.
param([switch]$Unsigned)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$nsis = Join-Path $root 'target\release\bundle\nsis'

$extra = @()
if ($Unsigned) {
    # A file, not inline JSON: PowerShell 5.1 strips the quotes from arguments to npm.
    $unsignedConfig = Join-Path $env:TEMP "seekr-unsigned-$PID.json"
    [IO.File]::WriteAllText($unsignedConfig, '{"bundle":{"createUpdaterArtifacts":false}}')
    $extra = '--config', $unsignedConfig
} else {
    if (-not $env:TAURI_SIGNING_PRIVATE_KEY) {
        $key = Join-Path $env:USERPROFILE '.tauri\seekr-updater.key'
        if (-not (Test-Path $key)) { throw "no updater signing key at $key; pass -Unsigned to build without one" }
        $env:TAURI_SIGNING_PRIVATE_KEY = $key
    }
    if (-not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD) {
        $secure = Read-Host 'Password for the updater signing key' -AsSecureString
        $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = [Runtime.InteropServices.Marshal]::PtrToStringBSTR(
            [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure))
    }
}

Push-Location (Join-Path $root 'app')
try {
    cargo build --release -p indexer -p service
    if ($LASTEXITCODE -ne 0) { throw "indexer and service build failed (exit $LASTEXITCODE)" }
    npm run tauri build -- --config src-tauri/tauri.nsis.json @extra
    if ($LASTEXITCODE -ne 0) { throw "installer build failed (exit $LASTEXITCODE)" }
} finally {
    Pop-Location
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
    if ($unsignedConfig) { Remove-Item $unsignedConfig -ErrorAction SilentlyContinue }
}

$version = (Get-Content (Join-Path $root 'app\src-tauri\tauri.conf.json') -Raw | ConvertFrom-Json).version
$setup = Join-Path $nsis "Seekr_${version}_x64-setup.exe"
if (-not $Unsigned) {
    # What the updater reads from the release's latest/download/latest.json.
    $latest = [ordered]@{
        version   = $version
        notes     = "See https://github.com/krish-745/Seekr/blob/v$version/CHANGELOG.md"
        pub_date  = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
        platforms = [ordered]@{
            'windows-x86_64' = [ordered]@{
                signature = (Get-Content "$setup.sig" -Raw).Trim()
                url       = "https://github.com/krish-745/Seekr/releases/download/v$version/Seekr_${version}_x64-setup.exe"
            }
        }
    }
    [IO.File]::WriteAllText((Join-Path $nsis 'latest.json'), ($latest | ConvertTo-Json -Depth 4), [Text.UTF8Encoding]::new($false))
}
Get-ChildItem $nsis -File | Where-Object Name -match "_${version}_|^latest\.json$" | ForEach-Object {
    "{0} ({1:N1} MB)" -f $_.FullName, ($_.Length / 1MB)
}
