# Guardian-DB Integration Tests

## Overview

This directory contains end-to-end integration tests for Guardian-DB. Unlike unit tests in `src/tests/`, these tests validate complete scenarios and interactions between multiple components.

## Structure

```
tests/
├── README.md                          # This file
├── test_determinism.rs                # Serialization determinism tests
├── integration_lifecycle.rs           # Complete GuardianDB lifecycle
├── integration_replication.rs         # P2P replication tests
├── integration_access_control.rs      # Access control tests
├── integration_persistence.rs         # Persistence and recovery tests
└── common/                            # Shared utilities
    └── mod.rs
```

## Test Categories

### 1. Lifecycle Tests (`integration_lifecycle.rs`)
- GuardianDB creation and initialization
- CRUD operations on different store types
- Proper resource cleanup and destruction
- Intermediate state validation

### 2. P2P Replication Tests (`integration_replication.rs`)
- Synchronization between multiple nodes
- Update propagation via Gossipsub
- CRDT conflict resolution
- Peer discovery and connection

### 3. Access Control Tests (`integration_access_control.rs`)
- SimpleAccessController (development)
- GuardianDBAccessController (production)
- IrohAccessController (Iroh-based)
- Permission and signature validation

### 4. Persistence Tests (`integration_persistence.rs`)
- Disk read and write operations
- Recovery after simulated crashes
- Cache vs storage consistency
- Data migrations

### 5. Determinism Tests (`test_determinism.rs`)
- Deterministic serialization with Postcard
- Consistent hashing with BLAKE3
- Guaranteed ordering with BTreeMap

## Running the Tests

```bash
# All integration tests
cargo test --test '*' -- --test-threads=1

# Specific test
cargo test --test integration_lifecycle

# With detailed logs
RUST_LOG=debug cargo test --test integration_lifecycle -- --nocapture

# Ignoring slow tests
cargo test --test integration_replication -- --skip slow_

# With increased timeout (P2P tests may take longer)
cargo test --test integration_replication -- --test-threads=1 --timeout=300
```

## Best Practices

1. **Isolation**: Each test should use unique temporary directories
2. **Cleanup**: Always clean up resources (use `tempfile::TempDir`)
3. **Timeouts**: Set appropriate timeouts for async operations
4. **Logs**: Use `tracing` to facilitate debugging
5. **Determinism**: Avoid flakiness with unnecessary sleeps
6. **Parallelism**: P2P tests should run sequentially (`--test-threads=1`)

## Requirements

- Rust 1.90+
- Execution time: ~5-10 minutes (all tests)
- Disk space: ~100MB temporary
- Network ports: Dynamic allocation (no fixed ports required)

## Contributing

When adding new integration tests:

1. Follow the naming pattern: `integration_<category>.rs`
2. Document the purpose of each test
3. Use helpers from the `common/` module for recurring setup
4. Add to CI in `.github/workflows/rust.yml`
5. Update this README with new categories
