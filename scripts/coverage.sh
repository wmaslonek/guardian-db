#!/bin/bash
# Guardian-DB Coverage Script
# Generate code coverage reports in multiple formats

set -e

echo "Guardian-DB Code Coverage Generator"
echo "======================================"

# Check if tarpaulin is installed
if ! command -v cargo-tarpaulin &> /dev/null; then
    echo "cargo-tarpaulin not found. Installing..."
    cargo install cargo-tarpaulin
fi

# Check if cargo-llvm-cov is installed
if ! command -v cargo-llvm-cov &> /dev/null; then
    echo "cargo-llvm-cov not found. Installing..."
    cargo install cargo-llvm-cov
fi

# Create coverage directory
mkdir -p ./coverage

echo ""
echo "Generating coverage reports..."

# Generate LCOV for Codecov compatibility
echo "Generating LCOV report..."
cargo llvm-cov --all-features --workspace --lcov --output-path ./coverage/lcov.info

# Generate XML and HTML with tarpaulin
echo "Generating XML and HTML reports..."
cargo tarpaulin --all-features --workspace --out Xml --out Html --output-dir ./coverage/

echo ""
echo "Coverage reports generated:"
echo "LCOV:  ./coverage/lcov.info"
echo "XML:   ./coverage/cobertura.xml"
echo "HTML:  ./coverage/tarpaulin-report.html"

# Calculate coverage percentage
COVERAGE=$(grep -o "lines......: [0-9.]*%" ./coverage/lcov.info | head -1 | grep -o "[0-9.]*")
echo ""
echo "Coverage: ${COVERAGE}%"

# Check threshold
THRESHOLD=70
if (( $(echo "$COVERAGE >= $THRESHOLD" | bc -l) )); then
    echo "Coverage meets threshold (≥${THRESHOLD}%)"
else
    echo "Coverage below threshold (<${THRESHOLD}%)"
    exit 1
fi

# Open HTML report if on macOS or Linux with display
if [[ "$OSTYPE" == "darwin"* ]]; then
    echo ""
    echo "Opening HTML report in browser..."
    open ./coverage/tarpaulin-report.html
elif command -v xdg-open &> /dev/null; then
    echo ""
    echo "Opening HTML report in browser..."
    xdg-open ./coverage/tarpaulin-report.html
else
    echo ""
    echo "To view the HTML report, open: ./coverage/tarpaulin-report.html"
fi

echo ""
echo "Coverage analysis complete!"
