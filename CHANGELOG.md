# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.15.0] - 2026-02-17

## [0.14.0] - 2026-01-08

### Added
- **Access Control Integration Test Suite**: Comprehensive test suite with 15 integration tests in `tests/integration_access_control.rs`
  - All 15 tests passing with complete coverage of access control system
  - Tests for SimpleAccessController: basic operations, permissions, can_append, wildcard access
  - Tests for GuardianDBAccessController: basic operations, persistence with skip_manifest
  - Tests for IrohAccessController: basic operations, permissions, can_append
  - Integration tests: access control with keyvalue stores, multiple controllers, type validation
  - Complete validation of authorization system with cryptographic verification
- **Integration Replication Test Suite**: Comprehensive test suite with 13 integration tests in `tests/integration_replication.rs`
  - All 13 tests passing with proper cache isolation
  - Tests covering two-node replication, concurrent operations, network partition recovery, and store isolation
  - Sequential operations, high-frequency updates, multi-node scenarios
  - Complete validation of P2P replication system using iroh-gossip
- **GuardianDBAccessController Comprehensive Test Suite**: New robust test suite with 33 integration tests in `src/tests/acl_guardian_comprehensive_test.rs`
  - All 33 tests passing when executed sequentially (`--test-threads=1`)
  - Complete coverage of GuardianDBAccessController functionality
  - Permission management: grant, revoke, duplicate permissions, role-based access
  - Access control: can_append with authorized/unauthorized users, wildcard support, admin inheritance
  - Concurrency tests: concurrent grants and mixed grant/revoke operations
  - Persistence: save/load operations and address validation
  - Edge cases: special characters in identities, very long identities, empty roles, stress tests with 100+ identities
  - Mock implementations for testing: MockLogEntry, MockCanAppendContext, MockIdentityProvider
- **IrohAccessController Comprehensive Test Suite**: New robust test suite with 26 integration tests in `src/tests/iroh_access_controller_test.rs`
  - All 26 tests passing with proper isolation using unique temporary directories
  - Complete coverage of IrohAccessController functionality
  - Basic tests: controller creation, default permissions, address validation
  - Permission management: grant, revoke, duplicate permissions, invalid capabilities
  - Access control: can_append with authorized/unauthorized users, wildcard support
  - Role-based access: write, read, admin, and unknown roles
  - Persistence: save controller, save/load round-trip validation
  - Concurrency: concurrent grants, mixed grant/revoke operations
  - CBOR serialization: simple data, empty lists, special characters/unicode
  - Edge cases: close controller, empty identity, many permissions (100+)
  - Test isolation achieved with unique directory generation per test
- **Comprehensive Guardian Module Test Suite**: New robust test suite with 40 integration tests in `src/tests/guardian_mod_test.rs`
  - 33 tests passing
  - Complete coverage of GuardianDB creation and configuration
  - EventLogStore tests: add_operation, get_by_hash, list operations, multiple operations
  - KeyValueStore tests: put, get, delete, all, update, concurrent operations, special characters, large values
  - DocumentStore tests: put, delete, query with filters, batch operations, complex JSON handling
  - Integration tests: access controller registration, event bus integration, multiple stores
  - Edge cases: empty stores, concurrent operations, store load and sync
- **Interior Mutability API Refactoring**: Comprehensive refactoring of trait method signatures from `&mut self` to `&self`
  - Updated 13 trait method signatures across `Store`, `EventLogStore`, `KeyValueStore`, and `DocumentStore` traits
  - Improved ergonomics when using `Arc<dyn Trait>` by exposing existing interior mutability pattern
  - Thread-safety maintained through existing `Arc<RwLock<T>>` implementation in BaseStore
  - Methods affected: `drop()`, `load()`, `sync()`, `load_more_from()`, `load_from_snapshot()`, `add_operation()`, `add()`, `put()`, `delete()`, `put_batch()`, `put_all()`
- **Guardian Wrapper Integration Tests**: New comprehensive test suite with 15 integration tests
  - Tests for GuardianDB creation and configuration
  - Tests for EventLogStore, KeyValueStore, and DocumentStore creation and basic operations
  - Tests for access control registration
  - Tests for multiple stores and different addresses
  - All tests passing with proper isolation
- **Reactive Synchronization System**: New `reactive_synchronizer` module with `SyncObserver` pattern for real-time observability
  - `SyncObserver` allows external components (UI, monitoring) to observe sync operations in real-time
  - `SyncProgress` tracking with completion percentage and state management
  - `SyncEvent` enum for Started, Progress, Ready, Replicated, and Error events
  - Integrated into BaseStore's `load()`, `load_more_from()`, and `sync()` methods
  - Provides `sync_observer()` getter for external access
- New helper method `Entry::payload_str()` for convenient UTF-8 string conversion from binary payload

### Changed
- **BREAKING**: Complete migration from secp256k1 to ed25519 for cryptographic operations
  - `DefaultIdentificator` now uses `ed25519_dalek` for all signing and verification
  - Public keys reduced from 65 bytes (secp256k1 uncompressed) to 32 bytes (ed25519)
  - Signatures now 64 bytes, hex-encoded for storage
  - Alignment with Iroh's native ed25519 usage for better compatibility
  - Identity creation signs `id + type` (e.g., "hash" + "GuardianDB") for verification
  - Fixed `signatures_map()` to properly decode hex signatures to bytes
  - Removed SHA256 message hashing - ed25519 signs raw bytes directly
- **Architecture Simplification**: Removed redundant RawPubSub wrapper layer in P2P messaging
  - BaseStore now uses EpidemicPubSub directly via trait downcast for replication
  - Guardian core instantiates EpidemicPubSub directly from IrohBackend
  - Simplified architecture eliminates unnecessary middleware layer
  - Updated all unit tests to use EpidemicPubSub directly
- **Cache Isolation**: Fixed cache directory isolation for concurrent test execution
  - BaseStore.create_cache() now accepts configurable cache directory parameter
  - Each GuardianDB instance uses isolated cache directory under its data path
  - Eliminates Sled DB file lock conflicts when multiple nodes run simultaneously
  - Enables parallel test execution without cache contention
- **BREAKING**: API method signatures changed from `&mut self` to `&self` for better Arc compatibility
  - All Store trait implementations updated across 6 core files
  - Updated files: `src/traits.rs`, `src/guardian/mod.rs`, `src/access_control/mod.rs`, `src/stores/base_store/mod.rs`, `src/stores/document_store/mod.rs`, `src/stores/event_log_store/mod.rs`, `src/stores/kv_store/mod.rs`
  - No functional changes - interior mutability was already present, now properly exposed
- **BREAKING**: Complete migration from JSON to Postcard binary serialization for all internal CRDT structures
  - Entry.payload type changed from `String` to `Vec<u8>` for efficient binary storage
  - MessageMarshaler now uses Postcard for all message serialization
  - Operation serialization migrated to Postcard
  - Snapshot serialization migrated to Postcard
  - AccessController operations migrated to Postcard
  - ACL Guardian and ACL Iroh permission storage migrated to Postcard
  - Entry.from_hash() deserialization migrated to Postcard
  - Log.snapshot() now uses Postcard for consistent Entry serialization
- Replaced HashMap with BTreeMap in serialized structures for deterministic BLAKE3 hashing
- Added dedicated serialization module (`src/guardian/serializer.rs`) wrapping Postcard with comprehensive size comparison tests
- Updated all test files to work with binary Entry.payload (using `b"..."` byte strings)
- Updated example files to use `.as_bytes()` and `.into_bytes()` for Entry creation

### Removed
- **P2P Messaging**: Removed redundant RawPubSub module from `src/p2p/messaging/raw.rs`
  - Eliminated ~200 lines of unnecessary middleware code
  - Direct use of EpidemicPubSub reduces complexity and improves maintainability
  - No functional changes - all replication features preserved
- **BREAKING**: Removed entire Replicator module (~1000 lines) that duplicated Iroh's native functionality
  - Removed `Replicator` struct and all associated methods from `src/stores/replicator/`
  - Removed `replicator()` method from `Store` trait
  - Removed replicator field and methods from `BaseStore`, `DocumentStore`, `KeyValueStore`, `EventLogStore`
  - Removed 4 replicator method implementations from Guardian wrappers
  - Kept minimal `Replcache isolation issue preventing concurrent test execution
  - Modified BaseStore to accept cache directory as parameter instead of hardcoded "./cache"
  - Each node now uses unique cache path based on its configured directory
  - Resolves "could not acquire lock" errors from Sled DB file conflicts
- **Critical**: Fixed "RawPubSub requires mutable access" error in BaseStore replication
  - BaseStore now correctly downcasts to EpidemicPubSub for topic subscription
  - Enables proper replication via iroh-gossip protocol
- **Critical**: Fixed ReplicationInfo` struct for compatibility with existing progress tracking
  - All replication now handled natively by Iroh's gossipsub and docs protocols

### Fixed
- **Critical**: Fixed GuardianDBAccessController initialization using hash address instead of name
  - Changed from using `params.address().to_string()` (64-char hex) to `params.get_name()`
  - Prevents "O nome do banco de dados fornecido já é um endereço válido" error
  - Generates unique timestamp-based names when no name is provided
  - Properly configures EventBus in CreateDBOptions to prevent "EventBus is a required option" errors
- **Critical**: Fixed identity signature verification in access control system
  - Aligned signature creation in `DefaultIdentificator::create()` with verification in `verify_identity()`
  - Signatures now verify `id + type` format consistently across the system
  - Enables cryptographic verification in can_append operations
  - All access controller tests now pass with ed25519 verification
- **Critical**: Fixed DocumentStore DELETE operation not being respected in oplog fallback queries
  - Modified `search_documents_from_oplog` in `src/guardian/mod.rs` to properly handle DELETE operations
  - Implemented reverse iteration through oplog (newest to oldest) to process only the most recent operation per key
  - Added tracking of processed keys to prevent duplicate processing
  - DELETE operations now correctly filter out deleted documents from query results
  - Ensures consistency between index-based queries and oplog fallback queries
- **Critical**: Fixed manifest loading issue when creating stores with `overwrite` option
  - `GuardianDB::open()` now checks if `overwrite` is true and uses `store_type` directly instead of trying to read non-existent manifests
  - `GuardianDBAccessController::new()` now properly passes `skip_manifest` flag as `overwrite` to store creation options
  - Fixes "entity not found" errors when creating new stores in tests and production scenarios
  - Enables proper test execution with skip_manifest flag
- **Critical**: Fixed CBOR/Postcard serialization inconsistency in IrohAccessController
  - Removed incorrect binary-to-string conversion using `String::from_utf8_lossy` that could corrupt data
  - Changed `CborWriteAccess.write` field from `String` to `Vec<String>` for native CBOR serialization
  - Eliminated unnecessary double serialization (Postcard wrapped in CBOR)
  - `save()` method now serializes permissions directly to CBOR without intermediate Postcard step
  - `load()` method now deserializes directly from CBOR, extracting `Vec<String>` natively
  - Ensures data integrity and consistency with manifest serialization format
  - Maintains separation of concerns: CBOR for protocol/metadata, Postcard for application data
- **Critical**: Removed 55+ unnecessary downcast blocks in `src/guardian/mod.rs` that were causing runtime errors
  - Fixed "Não foi possível fazer downcast para BaseStore" errors in EventLogStoreWrapper, KeyValueStoreWrapper, DocumentStoreWrapper, and KeyValueStoreBoxWrapper
  - Wrappers now delegate directly to trait methods instead of downcasting to BaseStore
  - Improved code reliability and eliminated runtime panics in store operations
- **Critical**: Fixed `load_more_from` method signature in all Store implementations
  - Method now correctly passes both `amount: u64` and `entries` parameters
- **Critical**: Fixed EventBus loss bug in `core.rs` where event_bus was not preserved when recreating CreateDBOptions
  - EventBus is now explicitly preserved when creating new options in the `open()` method
  - Ensures all stores receive proper EventBus configuration
- Fixed EventBus propagation in GuardianDB wrapper methods (`log()`, `key_value()`, `docs()`)
  - Methods now explicitly pass EventBus from GuardianDB to store creation options
  - Prevents "EventBus is a required option" errors during store creation
- Fixed test isolation issues by using unique store names per test
- Fixed `io::Error` to `GuardianError` conversion to properly handle `ErrorKind::NotFound` and `ErrorKind::TimedOut`
- Fixed `IrohClient` node_id synchronization - now uses backend's persistent secret_key instead of generating separate key
- Added `secret_key()` getter to `IrohBackend` for key consistency

### Performance
- **65-84% reduction** in serialized data size compared to JSON
- **~6x faster** serialization/deserialization performance
- **Deterministic hashing**: Consistent BLAKE3 hashes across all operations

### Security
- Deterministic serialization prevents hash collision attacks
- Binary format reduces attack surface compared to JSON parsing

## [0.11.18] - 2025-11-18

### Changed
- Integrated the batch processor with the Iroh backend, enabling more efficient batch operations.
- Fully implemented the create_controller function in ac/utils.rs, improving internal component orchestration.
- Refactored the former pubsub module, now renamed to p2p, including:

Peer connection system with verified handshake in DirectChannel.
Discovery beacon mechanism for automatic peer discovery.
Connection retry logic with proper timeout management.
Comprehensive peer identity verification during handshake.

- Renamed modules for improved architectural clarity:
src/iface.rs → traits.rs
ipfs_log/iface.rs → traits.rs

- General system improvements, including better stability, organization, and performance.

### Fixed
- Architecture improvements

## [0.10.15] - 2025-10-15

### Added
- New documentations
- Migration to Tracing complete
- Introducing the Embedded Iroh IPFS Node

### Fixed
- Fixed design error in the implementation of the trait BaseGuardianDB for GuardianDB
- Architecture improvements

## [0.9.13] - 2025-09-13

### Added
- Event system improvements and cleanup
- Protocol Buffer support for all workflows
- Multi-platform CI/CD pipeline

### Fixed
- Removed unused DummyEmitterInterface
- Fixed GitHub Actions compilation issues with libp2p-core

### Security
- Comprehensive security audit integration
- Dependency vulnerability scanning

## How to Release

1. Update version in `Cargo.toml`
2. Update this CHANGELOG.md
3. Commit changes: `git commit -am "chore: release v0.X.Y"`
4. Create and push tag: `git tag v0.X.Y && git push origin v0.X.Y`
5. GitHub Actions will automatically:
   - Create GitHub release
   - Build multi-platform binaries
   - Publish to crates.io (for stable releases)

## Version Strategy - Supported Release Types

- **Major (X.0.0)**: Breaking API changes (Automatically publishes to crates.io)
- **Minor (0.X.0)**: New features, backward compatible (Automatically publishes to crates.io)
- **Patch (0.0.X)**: Bug fixes, backward compatible (Automatically publishes to crates.io)
- **Pre-release**: `v1.0.0-alpha.1` (Publishes only on GitHub)
- **Beta**:  `v1.0.0-beta.1` (Publishes only on GitHub)
- **Release Candidate**: `v1.0.0-rc.1` (Publishes only on GitHub)
