# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
