# Extract a single version's section from CHANGELOG.md
# Usage: pwsh -File ./scripts/extract-changelog.ps1 -Version 0.20.26 [-OutFile notes.md]
#
# Prints the body of the "## [<version>] - <date>" section (header excluded) to stdout,
# or writes it to -OutFile. Used by the release workflow to build GitHub release notes,
# and runnable locally to preview what a release will look like.

param(
    [Parameter(Mandatory=$true)]
    [string]$Version,

    [string]$Path = "CHANGELOG.md",

    [string]$OutFile,

    # GitHub rejects release bodies over 125000 characters; leave room for the footer.
    [int]$MaxChars = 120000
)

$ErrorActionPreference = "Stop"

# Deterministic failure: message on stderr, exit code 1 (callers branch on it)
function Fail([string]$Message) {
    [Console]::Error.WriteLine("extract-changelog: $Message")
    exit 1
}

# Accept both "v0.20.26" and "0.20.26"
$normalized = $Version -replace '^v', ''

if (-not (Test-Path $Path)) {
    Fail "changelog not found: $Path"
}

$lines = Get-Content -Path $Path -Encoding UTF8

$sectionPattern = '^##\s+\[' + [regex]::Escape($normalized) + '\](\s|$)'
$anySectionPattern = '^##\s+\['

$start = -1
for ($i = 0; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match $sectionPattern) {
        $start = $i
        break
    }
}

if ($start -lt 0) {
    Fail "no section for version $normalized in $Path (expected a '## [$normalized] - <date>' heading)"
}

# Collect until the next version heading (or end of file)
$end = $lines.Count
for ($i = $start + 1; $i -lt $lines.Count; $i++) {
    if ($lines[$i] -match $anySectionPattern) {
        $end = $i
        break
    }
}

$body = $lines[($start + 1)..($end - 1)] -join "`n"

# Trim leading/trailing blank lines without touching internal formatting
$body = $body -replace '^(\r?\n)+', '' -replace '(\r?\n)+$', ''

if (-not $body.Trim()) {
    Fail "section for version $normalized is empty"
}

if ($body.Length -gt $MaxChars) {
    $body = $body.Substring(0, $MaxChars).TrimEnd() +
        "`n`n_(Notas truncadas por limite de tamanho do GitHub - veja o [CHANGELOG.md](https://github.com/wmaslonek/guardian-db/blob/main/CHANGELOG.md) completo.)_"
}

if ($OutFile) {
    # UTF-8 without BOM: the release API and gh CLI both expect plain UTF-8
    [System.IO.File]::WriteAllText(
        [System.IO.Path]::GetFullPath($OutFile),
        $body,
        (New-Object System.Text.UTF8Encoding($false))
    )
    Write-Host "Wrote $($body.Length) chars of release notes for v$normalized to $OutFile"
} else {
    Write-Output $body
}

# Explicit: without it, a caller invoking this script with '&' sees a stale $LASTEXITCODE
exit 0
