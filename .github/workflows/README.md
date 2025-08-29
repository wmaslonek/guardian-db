# Guardian-DB CI/CD

This directory contains GitHub Actions workflows for the Guardian-DB project.

## Workflows

### 🔨 [`rust.yml`](./rust.yml) - Main CI Pipeline
**Triggers:** Push/PR to main/develop branches
- **Test Suite**: Runs on stable, beta, and nightly Rust
- **Code Quality**: Format checking and Clippy linting
- **Security**: Dependency vulnerability scanning
- **Coverage**: Code coverage reporting (main branch only)

**Features:**
- Multi-Rust version testing
- Cargo caching for faster builds
- Documentation generation
- Security audit with `cargo-audit`
- Code coverage with `cargo-llvm-cov`

### ⚡ [`check.yml`](./check.yml) - Quick Compilation Check
**Triggers:** Push to feature branches, PRs affecting source code
- Fast compilation check
- Multiple feature flag combinations
- Minimal resource usage

### 🚀 [`release.yml`](./release.yml) - Release Automation
**Triggers:** Git tags starting with `v*`
- Multi-platform testing (Ubuntu, Windows, macOS)
- Automatic GitHub release creation
- crates.io publishing (stable releases only)
- Release notes generation

## Setup Requirements

### Repository Secrets
For the release workflow to work properly, add these secrets to your repository:

1. **`CARGO_REGISTRY_TOKEN`** - Token for publishing to crates.io
   - Get from [crates.io tokens page](https://crates.io/me)
   - Add to repository Settings → Secrets and variables → Actions

### Branch Protection
Recommended branch protection rules for `main`:
- Require status checks to pass before merging
- Require up-to-date branches before merging
- Include administrators in restrictions

## Workflow Status Badges

Add these to your main README.md:

```markdown
[![CI](https://github.com/wmaslonek/guardian-db/workflows/Guardian-DB%20CI/badge.svg)](https://github.com/wmaslonek/guardian-db/actions/workflows/rust.yml)
[![Security](https://github.com/wmaslonek/guardian-db/workflows/Guardian-DB%20CI/badge.svg)](https://github.com/wmaslonek/guardian-db/actions/workflows/rust.yml)
[![codecov](https://codecov.io/gh/wmaslonek/guardian-db/branch/main/graph/badge.svg)](https://codecov.io/gh/wmaslonek/guardian-db)
```

## Local Development

To run similar checks locally:

```bash
# Format check
cargo fmt --all -- --check

# Linting
cargo clippy --all-targets --all-features -- -D warnings

# Build with all features
cargo build --all-features

# Run tests
cargo test --all-features

# Security audit
cargo install cargo-audit
cargo audit

# Generate documentation
cargo doc --no-deps --all-features
```

## Customization

### Adding New Features
When adding new optional features to `Cargo.toml`, update the workflows to test with those features:

```yaml
- name: Test with new feature
  run: cargo test --features "new-feature"
```

### Platform-Specific Testing
To add platform-specific tests:

```yaml
strategy:
  matrix:
    os: [ubuntu-latest, windows-latest, macos-latest]
    include:
      - os: windows-latest
        target: x86_64-pc-windows-msvc
      - os: macos-latest
        target: x86_64-apple-darwin
```

### Performance Benchmarks
Add benchmark running to CI:

```yaml
- name: Run benchmarks
  run: cargo bench --all-features
```
