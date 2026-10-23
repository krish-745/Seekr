# WebDriver smoke test of the release build (target\release\seekr.exe), through tauri-driver
# and the msedgedriver matching the installed WebView2. The test instance gets its own data and
# config folders, so it runs beside an installed Seekr without touching its index or settings.
# -Keep leaves the test instance's data folder, with its log, in %TEMP%.
param([int]$Port = 4444, [switch]$Keep)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$app = Join-Path $root 'target\release\seekr.exe'
if (-not (Test-Path $app)) { throw "build first: $app is missing" }

# msedgedriver has to match the WebView2 runtime exactly; Edge Update records its version.
$webview = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
$version = foreach ($key in "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\$webview",
    "HKCU:\Software\Microsoft\EdgeUpdate\Clients\$webview") {
    try { (Get-ItemProperty $key -ErrorAction Stop).pv; break } catch {}
}
if (-not $version) { throw 'no WebView2 runtime found' }
$drivers = Join-Path $root "target\webdriver\$version"
$edge = Join-Path $drivers 'msedgedriver.exe'
if (-not (Test-Path $edge)) {
    Write-Host "Fetching msedgedriver $version"
    New-Item -ItemType Directory -Force $drivers | Out-Null
    $zip = Join-Path $drivers 'edgedriver_win64.zip'
    Invoke-WebRequest "https://msedgedriver.microsoft.com/$version/edgedriver_win64.zip" -OutFile $zip -UseBasicParsing
    Expand-Archive $zip $drivers -Force
    Remove-Item $zip
}

$tauriDriver = (Get-Command tauri-driver -ErrorAction SilentlyContinue).Source
if (-not $tauriDriver) {
    cargo install tauri-driver --version 2.1.0 --locked
    if ($LASTEXITCODE -ne 0) { throw 'cargo install tauri-driver failed' }
    $tauriDriver = Join-Path $env:USERPROFILE '.cargo\bin\tauri-driver.exe'
}

$scratch = Join-Path $env:TEMP "seekr-smoke-$PID"
New-Item -ItemType Directory -Force "$scratch\data", "$scratch\config" | Out-Null
# A hotkey nobody uses, and no clipboard history: the test instance shouldn't record the user's copies.
Set-Content "$scratch\config\config.toml" -Encoding utf8 -Value @'
hotkey = "Ctrl+Alt+Shift+F12"

[tools]
clipboard = false
'@
# No content folders, already answered: otherwise the first focus asks for them over the results.
Set-Content "$scratch\data\content-folders.txt" -Encoding utf8 -Value '# none, for the smoke test'
$saved = $env:SEEKR_DATA_DIR, $env:SEEKR_CONFIG_DIR
$env:SEEKR_DATA_DIR = "$scratch\data"
$env:SEEKR_CONFIG_DIR = "$scratch\config"
$driver = $null
try {
    $driver = Start-Process $tauriDriver -ArgumentList '--port', $Port, '--native-port', ($Port + 1), '--native-driver', "`"$edge`"" -PassThru -WindowStyle Hidden
    $deadline = (Get-Date).AddSeconds(15)
    while (-not (Test-NetConnection 127.0.0.1 -Port $Port -InformationLevel Quiet -WarningAction SilentlyContinue)) {
        if ((Get-Date) -gt $deadline) { throw 'tauri-driver did not start' }
        Start-Sleep -Milliseconds 200
    }
    node (Join-Path $root 'app\e2e\smoke.mjs') "http://127.0.0.1:$Port" $app
    $code = $LASTEXITCODE
} finally {
    $env:SEEKR_DATA_DIR, $env:SEEKR_CONFIG_DIR = $saved
    # Only tauri-driver's own tree: msedgedriver and the test instance, never the user's Seekr.
    if ($driver) { taskkill /T /F /PID $driver.Id 2>&1 | Out-Null }
    Start-Sleep -Milliseconds 500
    if ($Keep) { Write-Host "Kept $scratch" } else { Remove-Item -Recurse -Force $scratch -ErrorAction SilentlyContinue }
}
exit $code
