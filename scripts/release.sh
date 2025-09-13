#!/bin/bash

# Guardian-DB Release Script
# Usage: ./scripts/release.sh <version>
# Example: ./scripts/release.sh 0.9.5

set -e

VERSION=$1

if [ -z "$VERSION" ]; then
    echo "Error: Version not specified"
    echo "Usage: $0 <version>"
    echo "Example: $0 0.9.5"
    exit 1
fi

# Validate version format
if ! echo "$VERSION" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9]+(\.[0-9]+)?)?$'; then
    echo "Error: Invalid version format. Use semantic versioning (e.g., 1.0.0, 1.0.0-alpha.1)"
    exit 1
fi

echo "Preparing release for Guardian-DB v$VERSION"

# Check if we're on main branch
CURRENT_BRANCH=$(git branch --show-current)
if [ "$CURRENT_BRANCH" != "main" ]; then
    echo "Error: Not on main branch. Current branch: $CURRENT_BRANCH"
    echo "Please switch to main branch: git checkout main"
    exit 1
fi

# Check if working directory is clean
if ! git diff-index --quiet HEAD --; then
    echo "Error: Working directory is not clean. Please commit or stash changes."
    exit 1
fi

# Update version in Cargo.toml
echo "Updating version in Cargo.toml..."
sed -i.bak "s/^version = \".*\"/version = \"$VERSION\"/" Cargo.toml

# Check if Cargo.toml was updated
if ! grep -q "version = \"$VERSION\"" Cargo.toml; then
    echo "Error: Failed to update version in Cargo.toml"
    exit 1
fi

# Update CHANGELOG.md
echo "Updating CHANGELOG.md..."
DATE=$(date +%Y-%m-%d)
sed -i.bak "s/## \[Unreleased\]/## [Unreleased]\n\n## [$VERSION] - $DATE/" CHANGELOG.md

# Verify that everything still builds
echo "Building project to verify changes..."
cargo check --all-features

# Commit changes
echo "Committing changes..."
git add Cargo.toml CHANGELOG.md
git commit -m "chore: release v$VERSION"

# Create and push tag
echo "Creating and pushing tag v$VERSION..."
git tag "v$VERSION"
git push origin main
git push origin "v$VERSION"

echo "Release v$VERSION has been created!"
echo ""
echo "Next steps:"
echo "1. GitHub Actions will automatically create the release"
echo "2. Multi-platform builds will be created"
echo "3. Package will be published to crates.io (for stable releases)"
echo ""
echo "Monitor progress at: https://github.com/wmaslonek/guardian-db/actions"
echo "Release page: https://github.com/wmaslonek/guardian-db/releases/tag/v$VERSION"
