# Guardian-DB Release Script for Windows
# Usage: powershell -ExecutionPolicy Bypass -File .\scripts\release.ps1 -Version "0.x.x"
# Example: powershell -ExecutionPolicy Bypass -File .\scripts\release.ps1 -Version "0.x.x"

param(
    [Parameter(Mandatory=$true)]
    [string]$Version
)

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
$rustVersion = (Select-String -Path Cargo.toml -Pattern 'rust-version = "(.*)"').Matches.Groups[1].Value
if ($rustVersion) {
    Write-Host "Updating Rust version badge to $rustVersion..." -ForegroundColor Cyan
    $readmeContent = Get-Content README.md
    $readmeContent = $readmeContent -replace '\!\[Rust\]\(https://img\.shields\.io/badge/rust-.*?-orange\.svg\)', "![Rust](https://img.shields.io/badge/rust-$rustVersion+-orange.svg)"
    $readmeContent | Set-Content README.md
}

# Update other badges using the update-badges script
Write-Host "Updating other badges..." -ForegroundColor Cyan
powershell -ExecutionPolicy Bypass -File ".\scripts\update-badges.ps1" -ErrorAction SilentlyContinue

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

# Commit changes
Write-Host "Committing changes..." -ForegroundColor Cyan
git add Cargo.toml CHANGELOG.md README.md
git commit -m "chore: release v$Version"

# Create and push tag
Write-Host "Creating and pushing tag v$Version..." -ForegroundColor Cyan
git tag "v$Version"
git push origin main
git push origin "v$Version"

Write-Host "Release v$Version has been created!" -ForegroundColor Green
Write-Host ""
Write-Host "Next steps:" -ForegroundColor Cyan
Write-Host "1. GitHub Actions will automatically create the release"
Write-Host "2. Multi-platform builds will be created"
Write-Host "3. Package will be published to crates.io (for stable releases)"
Write-Host ""
Write-Host "Monitor progress at: https://github.com/wmaslonek/guardian-db/actions" -ForegroundColor Blue
Write-Host "Release page: https://github.com/wmaslonek/guardian-db/releases/tag/v$Version" -ForegroundColor Blue
