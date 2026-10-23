# Installs the release build for daily use: copies it to %LOCALAPPDATA%\Programs\Seekr, starts it
# hidden at login, and asks Windows to run it on the power-saving GPU. It also installs the
# SeekrIndex service (C:\Program Files\Seekr, one UAC prompt) that feeds whole-drive file search.
# -Uninstall undoes all of it and keeps your data (%LOCALAPPDATA%\Seekr, %APPDATA%\Seekr).
# -NoBuild reuses target\release. -SkipService updates only the app, with no UAC prompt.
param([switch]$Uninstall, [switch]$NoBuild, [switch]$SkipService)
$ErrorActionPreference = 'Stop'

$root = Split-Path $PSScriptRoot -Parent
$dest = Join-Path $env:LOCALAPPDATA 'Programs\Seekr'
$exe = Join-Path $dest 'seekr.exe'
$run = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
$approved = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run'
$gpu = 'HKCU:\Software\Microsoft\DirectX\UserGpuPreferences'

# Every running copy, dev or installed, so the exes can be replaced or removed.
Get-Process seekr, seekr-indexer -ErrorAction SilentlyContinue | Stop-Process -Force
Start-Sleep -Milliseconds 500

# Runs seekr-svc elevated (the UAC prompt) and fails if it does.
function Invoke-Service($svcExe, $verb) {
    $p = Start-Process $svcExe -ArgumentList $verb -Verb RunAs -Wait -PassThru -WindowStyle Hidden
    if ($p.ExitCode -ne 0) { throw "seekr-svc $verb failed (exit $($p.ExitCode))" }
}

if ($Uninstall) {
    $installedSvc = Join-Path $env:ProgramFiles 'Seekr\seekr-svc.exe'
    if (Get-Service SeekrIndex -ErrorAction SilentlyContinue) { Invoke-Service $installedSvc 'uninstall' }
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
        cargo build --release -p indexer
        if ($LASTEXITCODE -ne 0) { throw "indexer build failed (exit $LASTEXITCODE)" }
        if (-not $SkipService) {
            cargo build --release -p service
            if ($LASTEXITCODE -ne 0) { throw "service build failed (exit $LASTEXITCODE)" }
        }
    } finally { Pop-Location }
}

# seekr-svc copies itself to C:\Program Files\Seekr and (re)starts the service.
if (-not $SkipService) { Invoke-Service (Join-Path $root 'target\release\seekr-svc.exe') 'install' }

New-Item -ItemType Directory -Force $dest | Out-Null
Copy-Item (Join-Path $root 'target\release\seekr.exe') $exe -Force
# The content indexer runs from next to the app; both load DirectML for the embedding model and
# pdfium for PDF text.
foreach ($file in 'seekr-indexer.exe', 'DirectML.dll', 'pdfium.dll') {
    Copy-Item (Join-Path $root "target\release\$file") (Join-Path $dest $file) -Force
}

# The meaning-search model (gte-modernbert-base at a pinned revision; see
# docs\benchmarks\embeddings.md): about 750 MB, fetched once and checked against its SHA-256.
$models = Join-Path $env:LOCALAPPDATA 'Seekr\models\gte-modernbert-base'
$revision = 'e7f32e3c00f91d699e8c43b53106206bcc72bb22'
$modelFiles = [ordered]@{
    'tokenizer.json'       = '6c8aaa9a542084f2457eab775d4eeb51f92a70c0fd9de28d5edb0ddec3c08d30'
    'onnx/model_int8.onnx' = 'bae96b276d342bf86eeee07c1bdbc0c75bb82bf4033941aab7fabc1e33ee3b44'
    'onnx/model.onnx'      = '947f31df7effaeec4edb57c50e4ed7e0f2034d9336063f92615b92e3e0d24d78'
}
foreach ($name in $modelFiles.Keys) {
    $target = Join-Path $models ($name -replace '/', '\')
    $want = $modelFiles[$name]
    if ((Test-Path $target) -and (Get-FileHash $target -Algorithm SHA256).Hash -eq $want) { continue }
    New-Item -ItemType Directory -Force (Split-Path $target) | Out-Null
    Write-Host "Downloading $name..."
    $part = "$target.part"
    # The progress bar slows Invoke-WebRequest down several times in Windows PowerShell.
    $ProgressPreference = 'SilentlyContinue'
    Invoke-WebRequest "https://huggingface.co/Alibaba-NLP/gte-modernbert-base/resolve/$revision/$name" -OutFile $part
    if ((Get-FileHash $part -Algorithm SHA256).Hash -ne $want) {
        Remove-Item $part
        throw "$name doesn't match its checksum; not installed"
    }
    Move-Item $part $target -Force
}

Set-ItemProperty $run -Name Seekr -Value "`"$exe`" --hidden"
# Task Manager's Startup tab can veto a Run entry; mark ours enabled (first byte 2).
if (Test-Path $approved) {
    Set-ItemProperty $approved -Name Seekr -Type Binary -Value ([byte[]](2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0))
}
# Not New-Item -Force: on an existing key that would wipe other apps' GPU choices.
if (-not (Test-Path $gpu)) { New-Item $gpu | Out-Null }
Set-ItemProperty $gpu -Name $exe -Value 'GpuPreference=1;'

Start-Process $exe -ArgumentList '--hidden'
$with = if ($SkipService) { '' } else { ' with the SeekrIndex service' }
Write-Host "Seekr installed to $exe$with; starts at login. Press Alt+Space."
