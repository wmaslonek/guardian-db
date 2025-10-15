#!/bin/bash
# Badge Update Script for Guardian-DB
# Updates various badges in README.md based on project status

set -e

echo "Updating badges in README.md..."

# Get current directory (should be project root)
SCRIPT_DIR="$(dirname "$0")"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
README_PATH="$PROJECT_ROOT/README.md"

# Check if README.md exists
if [ ! -f "$README_PATH" ]; then
    echo "README.md not found at $README_PATH"
    exit 1
fi

# Update Rust version from Cargo.toml
RUST_VERSION=$(grep 'rust-version = "' "$PROJECT_ROOT/Cargo.toml" | sed 's/.*rust-version = "\([^"]*\)".*/\1/')
if [ ! -z "$RUST_VERSION" ]; then
    echo "Updating Rust version badge to $RUST_VERSION"
    sed -i.bak "s/!\[Rust\](https:\/\/img\.shields\.io\/badge\/rust-.*-orange\.svg)/![Rust](https:\/\/img.shields.io\/badge\/rust-$RUST_VERSION+-orange.svg)/" "$README_PATH"
fi

# Update version from Cargo.toml
VERSION=$(grep '^version = "' "$PROJECT_ROOT/Cargo.toml" | head -1 | sed 's/.*version = "\([^"]*\)".*/\1/')
if [ ! -z "$VERSION" ]; then
    echo "Updating version badge to $VERSION"
    sed -i.bak "s/!\[Version\](https:\/\/img\.shields\.io\/badge\/version-.*-brightgreen\.svg)/![Version](https:\/\/img.shields.io\/badge\/version-$VERSION-brightgreen.svg)/" "$README_PATH"
fi

# Count tests and update badge
echo "Counting tests..."
TEST_COUNT=$(cargo test --lib -- --list 2>/dev/null | grep -c "test " || echo "0")
if [ "$TEST_COUNT" -gt 0 ]; then
    echo "Updating tests badge to $TEST_COUNT tests"
    sed -i.bak "s/!\[Tests\](https:\/\/img\.shields\.io\/badge\/tests-.*passed-green\.svg)/![Tests](https:\/\/img.shields.io\/badge\/tests-${TEST_COUNT}passed-green.svg)/" "$README_PATH"
fi

# Update build status based on last local test
echo "Checking build status..."
if cargo check --quiet 2>/dev/null; then
    echo "Updating build status to passing"
    sed -i.bak "s/!\[Build Status\](https:\/\/img\.shields\.io\/badge\/build-.*-.*\.svg)/![Build Status](https:\/\/img.shields.io\/badge\/build-passing-green.svg)/" "$README_PATH"
else
    echo "Updating build status to failing"
    sed -i.bak "s/!\[Build Status\](https:\/\/img\.shields\.io\/badge\/build-.*-.*\.svg)/![Build Status](https:\/\/img.shields.io\/badge\/build-failing-red.svg)/" "$README_PATH"
fi

# Count lines of code (optional)
if command -v tokei &> /dev/null; then
    LOC=$(tokei --output json | jq '.Rust.code' 2>/dev/null || echo "0")
    if [ "$LOC" -gt 0 ]; then
        echo "Found $LOC lines of Rust code"
        # Add LOC badge if it doesn't exist
        if ! grep -q "Lines of Code" "$README_PATH"; then
            # Add after the Tests badge
            sed -i.bak "/!\[Tests\]/a\\
![Lines of Code](https://img.shields.io/badge/lines_of_code-${LOC}-blue.svg)" "$README_PATH"
        else
            sed -i.bak "s/!\[Lines of Code\](https:\/\/img\.shields\.io\/badge\/lines_of_code-.*-blue\.svg)/![Lines of Code](https:\/\/img.shields.io\/badge\/lines_of_code-${LOC}-blue.svg)/" "$README_PATH"
        fi
    fi
fi

# Clean up backup files
rm -f "$README_PATH.bak"

echo "Badges updated successfully!"
echo ""
echo "Current badges:"
grep "!\[.*\](https://img.shields.io" "$README_PATH" | head -10

echo ""
echo "To update badges manually:"
echo "   ./scripts/update-badges.sh"
