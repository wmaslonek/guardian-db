# Guardian-DB Release Script for Windows
# Example: powershell -ExecutionPolicy Bypass -File .\scripts\release.ps1 -Version "0.x.x"

param(
    [Parameter(Mandatory=$true)]
    [string]$Version
)

# Run a git command and abort the release if it fails
function Invoke-Git {
    param([string[]]$Arguments)

    & git @Arguments
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Error: 'git $($Arguments -join ' ')' failed with exit code $LASTEXITCODE." -ForegroundColor Red
        Write-Host "Release aborted. Repository may be in a partial state - check 'git log' and 'git tag'." -ForegroundColor Yellow
        exit 1
    }
}

# Validate version format
if ($Version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9]+(\.[0-9]+)?)?$') {
    Write-Host "Error: Invalid version format. Use semantic versioning (e.g., 1.0.0, 1.0.0-alpha.1)" -ForegroundColor Red
    exit 1
}

Write-Host "Preparing release for Guardian-DB v$Version" -ForegroundColor Green

# Check if we're on main branch
$currentBranch = git branch --show-current
if ($currentBranch -ne "main") {
    Write-Host "Error: Not on main branch. Current branch: $currentBranch" -ForegroundColor Red
    Write-Host "Please switch to main branch: git checkout main" -ForegroundColor Yellow
    exit 1
}

# Check if working directory is clean
$gitStatus = git status --porcelain
if ($gitStatus) {
    Write-Host "Error: Working directory is not clean. Please commit or stash changes." -ForegroundColor Red
    exit 1
}

# Update version in Cargo.toml
Write-Host "Updating version in Cargo.toml..." -ForegroundColor Cyan
$cargoContent = Get-Content Cargo.toml
$cargoContent = $cargoContent -replace '^version = ".*"', "version = `"$Version`""
$cargoContent | Set-Content Cargo.toml

# Verify update
$updatedVersion = Select-String -Path Cargo.toml -Pattern "version = `"$Version`""
if (-not $updatedVersion) {
    Write-Host "Error: Failed to update version in Cargo.toml" -ForegroundColor Red
    exit 1
}

# Update version badge in README.md
Write-Host "Updating version badge in README.md..." -ForegroundColor Cyan
$readmeContent = Get-Content README.md
$readmeContent = $readmeContent -replace '\!\[Version\]\(https://img\.shields\.io/badge/version-.*?-brightgreen\.svg\)', "![Version](https://img.shields.io/badge/version-$Version-brightgreen.svg)"
$readmeContent | Set-Content README.md

# Update Rust version badge if needed (optional - checks Cargo.toml rust-version)
$rustVersionMatch = Select-String -Path Cargo.toml -Pattern 'rust-version = "(.*)"'
$rustVersion = if ($rustVersionMatch) { $rustVersionMatch.Matches.Groups[1].Value } else { $null }
if ($rustVersion) {
    Write-Host "Updating Rust version badge to $rustVersion..." -ForegroundColor Cyan
    $readmeContent = Get-Content README.md
    $readmeContent = $readmeContent -replace '\!\[Rust\]\(https://img\.shields\.io/badge/rust-.*?-orange\.svg\)', "![Rust](https://img.shields.io/badge/rust-$rustVersion+-orange.svg)"
    $readmeContent | Set-Content README.md
}

# Update other badges using the update-badges script (cosmetic - non-fatal on failure)
Write-Host "Updating other badges..." -ForegroundColor Cyan
$updateBadges = Join-Path $PSScriptRoot "update-badges.ps1"
if (Test-Path $updateBadges) {
    powershell -ExecutionPolicy Bypass -File $updateBadges
    if ($LASTEXITCODE -ne 0) {
        Write-Host "Warning: update-badges.ps1 exited with code $LASTEXITCODE. Continuing." -ForegroundColor Yellow
    }
} else {
    Write-Host "Warning: update-badges.ps1 not found at $updateBadges. Skipping." -ForegroundColor Yellow
}

# Update CHANGELOG.md
Write-Host "Updating CHANGELOG.md..." -ForegroundColor Cyan
$date = Get-Date -Format "yyyy-MM-dd"
$changelogContent = Get-Content CHANGELOG.md
$changelogContent = $changelogContent -replace '## \[Unreleased\]', "## [Unreleased]`n`n## [$Version] - $date"
$changelogContent | Set-Content CHANGELOG.md

# Verify that everything still builds
Write-Host "Building project to verify changes..." -ForegroundColor Cyan
cargo check --all-features
if ($LASTEXITCODE -ne 0) {
    Write-Host "Error: Build failed. Please fix issues before releasing." -ForegroundColor Red
    exit 1
}

# Commit changes (Cargo.lock is tracked and gets rewritten by cargo check)
Write-Host "Committing changes..." -ForegroundColor Cyan
Invoke-Git @("add", "Cargo.toml", "Cargo.lock", "CHANGELOG.md", "README.md")
Invoke-Git @("commit", "-m", "chore: release v$Version")

# Create tag, then publish commit and tag
Write-Host "Creating and pushing tag v$Version..." -ForegroundColor Cyan
Invoke-Git @("tag", "v$Version")
Invoke-Git @("push", "origin", "main")
Invoke-Git @("push", "origin", "v$Version")

Write-Host "Release v$Version has been created!" -ForegroundColor Green
Write-Host ""
Write-Host "Next steps:" -ForegroundColor Cyan
Write-Host "1. GitHub Actions will automatically create the release"
Write-Host "2. Multi-platform builds will be created"
Write-Host "3. Package will be published to crates.io (for stable releases)"
Write-Host ""
Write-Host "Monitor progress at: https://github.com/wmaslonek/guardian-db/actions" -ForegroundColor Blue
Write-Host "Release page: https://github.com/wmaslonek/guardian-db/releases/tag/v$Version" -ForegroundColor Blue
