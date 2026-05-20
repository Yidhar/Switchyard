Write-Host "=========================================" -ForegroundColor Cyan
Write-Host "   Starting Switchyard Desktop GUI Client  " -ForegroundColor Cyan
Write-Host "=========================================" -ForegroundColor Cyan

# Define directories
$FrontendDir = Join-Path $PSScriptRoot "crates/switchyard-gui/frontend"
$GuiDir = Join-Path $PSScriptRoot "crates/switchyard-gui"

# 1. Ensure packages are installed
if (-not (Test-Path (Join-Path $FrontendDir "node_modules"))) {
    Write-Host "[1/3] installing dependencies in frontend..." -ForegroundColor Yellow
    Push-Location $FrontendDir
    npm install
    Pop-Location
}

# 2. Start Vite Dev Server in the background
Write-Host "[2/3] Starting Vite Dev Server in background..." -ForegroundColor Yellow
$ViteJob = Start-Job -ScriptBlock {
    param($path)
    cd $path
    npm run dev
} -ArgumentList $FrontendDir

# Wait for Vite to initialize
Start-Sleep -Seconds 3

# 3. Start Tauri App in the foreground
Write-Host "[3/3] Launching Tauri App wrapper..." -ForegroundColor Yellow
Push-Location $GuiDir
npx --package @tauri-apps/cli tauri dev --no-watch
Pop-Location

# 4. Clean up background Vite server when Tauri exits
Write-Host "Tauri wrapper closed. Cleaning up background jobs..." -ForegroundColor Yellow
Stop-Job $ViteJob
Remove-Job $ViteJob

Write-Host "Done! Switchyard GUI stopped cleanly." -ForegroundColor Green
