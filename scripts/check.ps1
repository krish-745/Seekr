# Every gate a commit must pass. Stops at the first failure with a non-zero exit code.
# Failures are detected by $LASTEXITCODE, not ErrorActionPreference: PS 5.1 treats cargo's stderr as errors.

function Step([string]$name, [scriptblock]$cmd) {
    Write-Host "== $name" -ForegroundColor Cyan
    & $cmd
    if ($LASTEXITCODE -ne 0) { Write-Host "FAILED: $name (exit $LASTEXITCODE)" -ForegroundColor Red; exit $LASTEXITCODE }
}

Push-Location (Split-Path $PSScriptRoot -Parent)
try {
    Step 'cargo fmt' { cargo fmt --all --check }
    Step 'cargo clippy' { cargo clippy --workspace --all-targets -- -D warnings }
    Step 'cargo test' { cargo test --workspace }
    Step 'cargo deny' { cargo deny check }
    Push-Location app
    try {
        if (-not (Test-Path node_modules)) { Step 'npm install' { npm install } }
        Step 'tsc' { npm run typecheck }
        Step 'vitest' { npm test }
        Step 'release build' { npm run tauri build -- --no-bundle }
        Step 'service and indexer release build' { cargo build --release -p service -p indexer }
    } finally { Pop-Location }
    Write-Host 'All checks passed.' -ForegroundColor Green
} finally { Pop-Location }
