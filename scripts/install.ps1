# Installs the release build for daily use: copies it to %LOCALAPPDATA%\Programs\Seekr, starts it
# hidden at login, and asks Windows to run it on the power-saving GPU. -Uninstall undoes all three
# and keeps your data (%LOCALAPPDATA%\Seekr, %APPDATA%\Seekr). -NoBuild reuses target\release.
param([switch]$Uninstall, [switch]$NoBuild)
$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot -Parent
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Seekr'
$exe = Join-Path $dest 'seekr.exe'
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$approved = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
$gpu = 'HKCU:\Software\Microsoft\DirectX\UserGpuPreferences'

# Every running copy, dev or installed, so the exe can be replaced or removed.
Get-Process seekr -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

if ($Uninstall) {
    Remove-ItemProperty $run -Name Seekr -ErrorAction SilentlyContinue
    Remove-ItemProperty $approved -Name Seekr -ErrorAction SilentlyContinue
    Remove-ItemProperty $gpu -Name $exe -ErrorAction SilentlyContinue
    Remove-Item $dest -Recurse -Force -ErrorAction SilentlyContinue
    Write-Host "Seekr uninstalled. Data kept in $env:LOCALAPPDATA\Seekr and $env:APPDATA\Seekr."
    exit 0
}

if (-not $NoBuild) {
    Push-Location (Join-Path $root 'app')
    try {
        npm run tauri build -- --no-bundle
        if ($LASTEXITCODE -ne 0) { throw "release build failed (exit $LASTEXITCODE)" }
    } finally { Pop-Location }
}

New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item (Join-Path $root 'target\release\seekr.exe') $exe -Force
# Native DLLs (onnxruntime, pdfium) get pinned and copied next to the exe once Seekr ships them.

Set-ItemProperty $run -Name Seekr -Value "`"$exe`" --hidden"
# Task Manager's Startup tab can veto a Run entry; mark ours enabled (first byte 2).
if (Test-Path $approved) {
    Set-ItemProperty $approved -Name Seekr -Type Binary -Value ([byte[]](2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
}
# Not New-Item -Force: on an existing key that would wipe other apps' GPU choices.
if (-not (Test-Path $gpu)) { New-Item $gpu | Out-Null }
Set-ItemProperty $gpu -Name $exe -Value 'GpuPreference=1;'

Start-Process $exe -ArgumentList '--hidden'
Write-Host "Seekr installed to $exe, starts at login, power-saving GPU. Press Alt+Space."
