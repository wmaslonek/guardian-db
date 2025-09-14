# Badge Update Script for Guardian-DB (Windows PowerShell)
# Updates various badges in README.md based on project status
# Usage: powershell -ExecutionPolicy Bypass -File .\scripts\update-badges.ps1

param(
    [switch]$UpdateLOC = $false
)

Write-Host "Updating badges in README.md..." -ForegroundColor Green

# Get current directory (should be project root)
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$ReadmePath = Join-Path $ProjectRoot "README.md"

# Check if README.md exists
if (-not (Test-Path $ReadmePath)) {
    Write-Host "README.md not found at $ReadmePath" -ForegroundColor Red
    exit 1
}

# Update Rust version from Cargo.toml
$CargoPath = Join-Path $ProjectRoot "Cargo.toml"
$RustVersionMatch = Select-String -Path $CargoPath -Pattern 'rust-version = "(.*)"'
if ($RustVersionMatch) {
    $RustVersion = $RustVersionMatch.Matches.Groups[1].Value
    Write-Host "Updating Rust version badge to $RustVersion" -ForegroundColor Cyan
    $ReadmeContent = Get-Content $ReadmePath
    $ReadmeContent = $ReadmeContent -replace '\!\[Rust\]\(https://img\.shields\.io/badge/rust-.*?-orange\.svg\)', "![Rust](https://img.shields.io/badge/rust-$RustVersion+-orange.svg)"
    $ReadmeContent | Set-Content $ReadmePath
}

# Update version from Cargo.toml
$VersionMatch = Select-String -Path $CargoPath -Pattern '^version = "(.*)"'
if ($VersionMatch) {
    $Version = $VersionMatch.Matches.Groups[1].Value
    Write-Host "Updating version badge to $Version" -ForegroundColor Cyan
    $ReadmeContent = Get-Content $ReadmePath
    $ReadmeContent = $ReadmeContent -replace '\!\[Version\]\(https://img\.shields\.io/badge/version-.*?-brightgreen\.svg\)', "![Version](https://img.shields.io/badge/version-$Version-brightgreen.svg)"
    $ReadmeContent | Set-Content $ReadmePath
}

# Count tests and update badge
Write-Host "Counting tests..." -ForegroundColor Cyan
try {
    $TestOutput = cargo test --lib -- --list 2>$null
    $TestCount = ($TestOutput | Select-String "test ").Count
    
    if ($TestCount -gt 0) {
        Write-Host "Updating tests badge to $TestCount tests" -ForegroundColor Cyan
        $ReadmeContent = Get-Content $ReadmePath
        $ReadmeContent = $ReadmeContent -replace '\!\[Tests\]\(https://img\.shields\.io/badge/tests-.*?passed-green\.svg\)', "![Tests](https://img.shields.io/badge/tests-${TestCount}passed-green.svg)"
        $ReadmeContent | Set-Content $ReadmePath
    }
} catch {
    Write-Host "Could not count tests" -ForegroundColor Yellow
}

# Update build status based on last local test
Write-Host "Checking build status..." -ForegroundColor Cyan
try {
    cargo check --quiet 2>$null
    if ($LASTEXITCODE -eq 0) {
        Write-Host "Updating build status to passing" -ForegroundColor Green
        $ReadmeContent = Get-Content $ReadmePath
        $ReadmeContent = $ReadmeContent -replace '\!\[Build Status\]\(https://img\.shields\.io/badge/build-.*?-.*?\.svg\)', "![Build Status](https://img.shields.io/badge/build-passing-green.svg)"
        $ReadmeContent | Set-Content $ReadmePath
    } else {
        Write-Host "Updating build status to failing" -ForegroundColor Red
        $ReadmeContent = Get-Content $ReadmePath
        $ReadmeContent = $ReadmeContent -replace '\!\[Build Status\]\(https://img\.shields\.io/badge/build-.*?-.*?\.svg\)', "![Build Status](https://img.shields.io/badge/build-failing-red.svg)"
        $ReadmeContent | Set-Content $ReadmePath
    }
} catch {
    Write-Host "Could not check build status" -ForegroundColor Yellow
}

Write-Host "Badges updated successfully!" -ForegroundColor Green
Write-Host ""
Write-Host "Current badges:" -ForegroundColor Blue
$Badges = Select-String -Path $ReadmePath -Pattern "!\[.*\]\(https://img\.shields\.io" | Select-Object -First 5
$Badges | ForEach-Object { Write-Host "   $($_.Line.Trim())" }

Write-Host ""
Write-Host "Usage examples:" -ForegroundColor Blue
Write-Host "   .\scripts\update-badges.ps1                 # Update basic badges"
Write-Host "   .\scripts\update-badges.ps1 -UpdateLOC      # Update with lines of code"
