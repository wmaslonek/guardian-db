# Build the GitHub release notes for a tag: CHANGELOG section + a fixed footer.
# Usage: pwsh -File ./scripts/build-release-notes.ps1 -Tag v0.20.26 -OutFile release-notes.md
#
# Writes the notes to -OutFile and prints the source used on stdout:
#   "changelog" - the tag's CHANGELOG section was found and used
#   "fallback"  - no usable section; a minimal body was written and the caller
#                 should ask GitHub to append auto-generated notes
# Exits non-zero only on unexpected errors, so callers can branch on the source.

param(
    [Parameter(Mandatory=$true)]
    [string]$Tag,

    [Parameter(Mandatory=$true)]
    [string]$OutFile,

    [string]$Repo = "wmaslonek/guardian-db",

    [string]$ChangelogPath = "CHANGELOG.md"
)

$ErrorActionPreference = "Stop"

$version = $Tag -replace '^v', ''
$extractor = Join-Path $PSScriptRoot "extract-changelog.ps1"

if (-not (Test-Path $extractor)) {
    [Console]::Error.WriteLine("build-release-notes: missing $extractor")
    exit 1
}

# Reset first: '&' on a script leaves $LASTEXITCODE untouched unless the script exits explicitly
$global:LASTEXITCODE = 0
$body = & $extractor -Version $version -Path $ChangelogPath 2>&1
$source = if ($LASTEXITCODE -eq 0) { "changelog" } else { "fallback" }

if ($source -eq "fallback") {
    [Console]::Error.WriteLine("build-release-notes: no CHANGELOG section for $version ($($body -join ' '))")
    $body = "Release ``$Tag``."
} else {
    $body = $body -join "`n"
}

$footer = @"

---

## Installation

``````toml
[dependencies]
guardian-db = "$version"
``````

## Documentation
- [API documentation](https://docs.rs/guardian-db/$version)
- [Full changelog](https://github.com/$Repo/blob/main/CHANGELOG.md)
"@

$notes = $body + "`n" + $footer

# UTF-8 without BOM: a BOM shows up as a stray character in the rendered release
[System.IO.File]::WriteAllText(
    [System.IO.Path]::GetFullPath($OutFile),
    $notes,
    (New-Object System.Text.UTF8Encoding($false))
)

Write-Output $source

# A fallback is a normal outcome, not an error: don't leak the extractor's exit code
exit 0
