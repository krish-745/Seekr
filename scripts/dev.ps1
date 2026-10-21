# Runs the app with hot reload for the UI and a rebuild on Rust changes.
Push-Location (Join-Path (Split-Path $PSScriptRoot -Parent) 'app')
try { npm run tauri dev } finally { Pop-Location }
