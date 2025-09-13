# Guardian-DB Coverage Script for Windows
# Generate code coverage reports in multiple formats

param(
    [switch]$SkipThreshold,
    [int]$Threshold = 70
)

Write-Host "Guardian-DB Code Coverage Generator" -ForegroundColor Green
Write-Host "======================================" -ForegroundColor Green

# Check if tarpaulin is installed
$tarpaulinInstalled = Get-Command cargo-tarpaulin -ErrorAction SilentlyContinue
if (-not $tarpaulinInstalled) {
    Write-Host "cargo-tarpaulin not found. Installing..." -ForegroundColor Yellow
    cargo install cargo-tarpaulin
}

# Check if cargo-llvm-cov is installed
$llvmCovInstalled = Get-Command cargo-llvm-cov -ErrorAction SilentlyContinue
if (-not $llvmCovInstalled) {
    Write-Host "cargo-llvm-cov not found. Installing..." -ForegroundColor Yellow
    cargo install cargo-llvm-cov
}

# Create coverage directory
if (-not (Test-Path "./coverage")) {
    New-Item -ItemType Directory -Path "./coverage" | Out-Null
}

Write-Host ""
Write-Host "Generating coverage reports..." -ForegroundColor Cyan

# Generate LCOV for Codecov compatibility
Write-Host "Generating LCOV report..." -ForegroundColor Gray
cargo llvm-cov --all-features --workspace --lcov --output-path ./coverage/lcov.info

if ($LASTEXITCODE -ne 0) {
    Write-Host "Failed to generate LCOV report" -ForegroundColor Red
    exit 1
}

# Generate XML and HTML with tarpaulin
Write-Host "→ Generating XML and HTML reports..." -ForegroundColor Gray
cargo tarpaulin --all-features --workspace --out Xml --out Html --output-dir ./coverage/

if ($LASTEXITCODE -ne 0) {
    Write-Host "Failed to generate tarpaulin reports" -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "Coverage reports generated:" -ForegroundColor Green
Write-Host "LCOV:  ./coverage/lcov.info"
Write-Host "XML:   ./coverage/cobertura.xml"
Write-Host "HTML:  ./coverage/tarpaulin-report.html"

# Calculate coverage percentage from tarpaulin output
$coverageFile = "./coverage/cobertura.xml"
if (Test-Path $coverageFile) {
    $xml = [xml](Get-Content $coverageFile)
    $coverage = [math]::Round([double]$xml.coverage.'line-rate' * 100, 2)
    
    Write-Host ""
    Write-Host "Coverage: $coverage%" -ForegroundColor Cyan
    
    # Check threshold
    if (-not $SkipThreshold) {
        if ($coverage -ge $Threshold) {
            Write-Host "Coverage meets threshold (≥$Threshold%)" -ForegroundColor Green
        } else {
            Write-Host "Coverage below threshold (<$Threshold%)" -ForegroundColor Red
            exit 1
        }
    }
} else {
    Write-Host "Could not read coverage percentage from XML file" -ForegroundColor Yellow
}

# Open HTML report
$htmlReport = "./coverage/tarpaulin-report.html"
if (Test-Path $htmlReport) {
    Write-Host ""
    Write-Host "Opening HTML report in browser..." -ForegroundColor Cyan
    Start-Process $htmlReport
} else {
    Write-Host ""
    Write-Host "HTML report not found at: $htmlReport" -ForegroundColor Yellow
}

Write-Host ""
Write-Host "Coverage analysis complete!" -ForegroundColor Green

# Usage examples
Write-Host ""
Write-Host "Usage examples:" -ForegroundColor Blue
Write-Host "   .\scripts\coverage.ps1                    # Standard coverage with 70% threshold"
Write-Host "   .\scripts\coverage.ps1 -Threshold 80      # Custom threshold"
Write-Host "   .\scripts\coverage.ps1 -SkipThreshold     # Skip threshold check"
