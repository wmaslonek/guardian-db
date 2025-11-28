# Coverage Setup Guide

This guide explains how to set up code coverage reporting for Guardian-DB.

## Local Development

### Prerequisites

Install the required coverage tools:

```bash
# Using cargo
cargo install cargo-tarpaulin
cargo install cargo-llvm-cov

# On Ubuntu/Debian (for tarpaulin dependencies)
sudo apt-get install pkg-config libssl-dev
```

### Generate Coverage Reports

```bash
# Quick coverage (PowerShell on Windows)
.\scripts\coverage.ps1

# Quick coverage (Bash on Linux/macOS)
./scripts/coverage.sh

# Manual generation
cargo tarpaulin --all-features --workspace --out Html --out Xml
```

## CI/CD Integration

### GitHub Actions

The project automatically generates coverage reports on every push to `main`:

1. **LCOV format** → Uploaded to Codecov
2. **XML format** → Available as artifact (Cobertura)
3. **HTML format** → Available as artifact (for viewing)

### Codecov Setup

To enable Codecov integration:

1. **Go to [Codecov.io](https://about.codecov.io/)**
2. **Sign in with GitHub**
3. **Add the `guardian-db` repository**
4. **Copy the repository token**
5. **Add as GitHub Secret**:
   - Go to: Repository → Settings → Secrets and variables → Actions
   - Add new secret: `CODECOV_TOKEN`
   - Paste the token value

### Supported Output Formats

| Format | File | Use Case |
|--------|------|----------|
| **LCOV** | `lcov.info` | Codecov, VS Code extensions |
| **XML** | `cobertura.xml` | SonarQube, Jenkins, IDEs |
| **HTML** | `tarpaulin-report.html` | Human-readable reports |
| **JSON** | `tarpaulin-report.json` | Custom tooling |

## Coverage Configuration

### Thresholds

- **Minimum coverage**: 70%
- **PR target**: 80%
- **Patch threshold**: 5% change tolerance

### Excluded Files

The following are excluded from coverage:
- `tests/` - Test files
- `examples/` - Example code
- `benches/` - Benchmark files
- `build.rs` - Build scripts
- Generated protobuf files

### Configuration Files

- **`tarpaulin.toml`** - Tarpaulin configuration
- **`codecov.yml`** - Codecov behavior
- **`.github/workflows/rust.yml`** - CI coverage job

## Coverage Reports

### Local HTML Report

After running coverage, open:
```
./coverage/tarpaulin-report.html
```

### CI Artifacts

Download coverage reports from GitHub Actions:
1. Go to the workflow run
2. Scroll to "Artifacts"
3. Download "coverage-reports"

### Codecov Dashboard

View detailed coverage analysis at:
```
https://app.codecov.io/gh/wmaslonek/guardian-db
```

## Troubleshooting

### Common Issues

**Issue**: `cargo-tarpaulin` fails to install
**Solution**: Install dependencies first:
```bash
# Ubuntu/Debian
sudo apt-get install pkg-config libssl-dev

# Fedora/CentOS
sudo dnf install pkg-config openssl-devel
```

**Issue**: Low coverage on certain modules
**Solution**: Check if files are excluded in `tarpaulin.toml`

**Issue**: CI coverage job fails
**Solution**: Verify protobuf-compiler is installed in the workflow

### Performance Tips

- Use `cargo-llvm-cov` for quick checks
```bash
# to check for limited test functions in development using cargo-llvm-cov
cargo llvm-cov test <TEST Fn NAME> --html --open --output-dir ./coverage/
```

- Use `cargo-tarpaulin` for detailed analysis
```bash
# to check for detailed analysis on limited number of files in development 
# using cargo-tarpaulin
cargo tarpaulin --include-files <PATH TO FILE> --out Html --output-dir ./coverage/
```

- Set `--test-threads=1` for deterministic results
- Use `--timeout 300s` for long-running tests

## Coverage Goals

| Component | Target Coverage |
|-----------|----------------|
| Core library (`src/`) | ≥ 85% |
| Event system | ≥ 90% |
| Storage modules | ≥ 80% |
| Network layer | ≥ 75% |
| Integration tests | N/A (excluded) |

## Useful Links

- [Tarpaulin Documentation](https://github.com/xd009642/tarpaulin)
- [Codecov Documentation](https://docs.codecov.io/)
- [LLVM Coverage](https://doc.rust-lang.org/rustc/instrument-coverage.html)
