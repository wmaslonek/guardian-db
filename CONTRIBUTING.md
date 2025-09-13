# Contributing to GuardianDB

Thank you for considering contributing to GuardianDB! This document provides guidelines for contributions that will help maintain the quality and consistency of the project.

## 📋 Table of Contents

- [Code of Conduct](#code-of-conduct)
- [How to Contribute](#how-to-contribute)
- [Environment Setup](#environment-setup)
- [Development Process](#development-process)
- [Code Standards](#code-standards)
- [Testing](#testing)
- [Documentation](#documentation)
- [Pull Requests](#pull-requests)
- [Issues](#issues)
- [Code Review](#code-review)

## 🤝 Code of Conduct

This project adopts the [Contributor Covenant](https://www.contributor-covenant.org/) as its code of conduct. By participating, you agree to maintain a respectful and inclusive environment for everyone.

### Expected Behavior

- Use welcoming and inclusive language
- Respect different viewpoints and experiences
- Accept constructive criticism gracefully
- Focus on what is best for the community
- Show empathy towards other community members

### Unacceptable Behavior

- Sexualized language or imagery
- Trolling, insulting or derogatory comments
- Public or private harassment
- Publishing private information without permission
- Other conduct inappropriate in a professional environment

## 🚀 How to Contribute

There are several ways to contribute to GuardianDB:

### 🐛 Report Bugs

- Use the bug issue template
- Include steps to reproduce the problem
- Provide environment information (Rust version, OS, etc.)
- Add relevant logs

### 💡 Suggest Features

- Use the feature request issue template
- Clearly describe the problem it solves
- Explain why it would be useful for other users
- Consider multiple possible solutions

### 📝 Improve Documentation

- Fix typos or grammar errors
- Add code examples
- Improve existing explanations
- Translate documentation

### 💻 Contribute Code

- Implement new features
- Fix existing bugs
- Improve performance
- Add tests

## ⚙️ Environment Setup

### Prerequisites

```bash
# Rust 1.70 or higher
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Git
# Install via your OS package manager

# Recommended editor: VS Code with rust-analyzer
```

### Initial Setup

```bash
# 1. Fork the repository on GitHub

# 2. Clone your fork
git clone https://github.com/wmaslonek/guardian-db.git
cd guardian-db

# 3. Add the original repository as upstream remote
git remote add upstream https://github.com/wmaslonek/guardian-db.git

# 4. Install dependencies and verify compilation
cargo build

# 5. Run tests
cargo test

# 6. Check formatting
cargo fmt --check

# 7. Run clippy
cargo clippy -- -D warnings
```

### Recommended Tools

```bash
# Install development tools
rustup component add rustfmt clippy

# For benchmark tests
cargo install cargo-criterion

# For coverage analysis
cargo install cargo-tarpaulin

# For documentation
cargo install mdbook
```

## 🔄 Development Process

### 1. Planning

1. Discuss major changes in issues first
2. Check if there's no similar work in progress
3. Understand the impact of the change on the project

### 2. Development

```bash
# 1. Create a branch for your feature
git checkout -b feature/new-functionality

# 2. Make small, focused commits
git commit -m "type: concise description"

# 3. Keep your branch updated
git fetch upstream
git rebase upstream/main

# 4. Run tests frequently
cargo test
```

### 3. Commit Types

Use [Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` - New functionality
- `fix:` - Bug fix
- `docs:` - Documentation changes
- `style:` - Formatting (no code changes)
- `refactor:` - Code refactoring
- `test:` - Add or modify tests
- `chore:` - Changes to tools, configs, etc.

Examples:
```bash
git commit -m "feat: add support for document queries"
git commit -m "fix: resolve memory leak in replicator"
git commit -m "docs: update README examples"
```

## 📏 Code Standards

### Formatting

```bash
# Format all code before committing
cargo fmt

# Check if formatted
cargo fmt --check
```

### Linting

```bash
# Run clippy to check for issues
cargo clippy -- -D warnings

# For more rigorous code
cargo clippy -- -D clippy::all
```

### Rust Conventions

#### Naming

```rust
// Structs: PascalCase
pub struct EventLogStore;

// Functions and variables: snake_case
pub fn create_database() -> Result<()>;
let database_name = "my-db";

// Constants: SCREAMING_SNAKE_CASE
const MAX_RETRIES: usize = 3;

// Traits: PascalCase, preferably with descriptive suffix
pub trait StorageProvider;
pub trait Replicatable;
```

#### Documentation

```rust
/// Creates a new instance of EventLogStore.
///
/// # Arguments
///
/// * `name` - Unique name for the store
/// * `options` - Configuration options
///
/// # Returns
///
/// Returns `Result<EventLogStore, GuardianError>`
///
/// # Examples
///
/// ```rust
/// use guardian_db::EventLogStore;
///
/// let store = EventLogStore::new("my-log", None)?;
/// ```
///
/// # Errors
///
/// Returns error if:
/// - Name already exists
/// - Invalid configuration
pub fn new(name: &str, options: Option<StoreOptions>) -> Result<Self> {
    // implementation
}
```

#### Error Handling

```rust
// Use thiserror for custom errors
#[derive(Debug, thiserror::Error)]
pub enum GuardianError {
    #[error("Database not found: {name}")]
    DatabaseNotFound { name: String },
    
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    
    #[error("IPFS error: {0}")]
    Ipfs(String),
}

// Prefer Result<T> over unwrap/expect
pub fn get_database(name: &str) -> Result<Database> {
    databases.get(name)
        .ok_or_else(|| GuardianError::DatabaseNotFound { 
            name: name.to_string() 
        })
}
```

#### Async/Await

```rust
// Use async/await consistently
pub async fn replicate_data(&self) -> Result<()> {
    // asynchronous operations
}

// For traits, use async-trait
#[async_trait]
pub trait Replicator {
    async fn start_replication(&self) -> Result<()>;
}
```

## 🧪 Tests

### Running Tests

```bash
# All tests
cargo test

# Specific tests
cargo test test_event_log

# Tests with output
cargo test -- --nocapture

# Tests with logs
RUST_LOG=debug cargo test

# Integration tests
cargo test --test integration

# Benchmarks
cargo bench
```

### Writing Tests

#### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tokio_test;

    #[tokio::test]
    async fn test_create_database() {
        let db = GuardianDB::new_mock().await.unwrap();
        let result = db.log("test-db", None).await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_error_handling() {
        let error = GuardianError::DatabaseNotFound { 
            name: "missing".to_string() 
        };
        assert_eq!(error.to_string(), "Database not found: missing");
    }
}
```

#### Integration Tests

```rust
// tests/integration.rs
use guardian_db::*;

#[tokio::test]
async fn test_full_replication_flow() {
    let node1 = setup_node("node1").await;
    let node2 = setup_node("node2").await;
    
    // Create database on node1
    let db1 = node1.log("shared-log", None).await.unwrap();
    db1.add(b"test data").await.unwrap();
    
    // Connect nodes
    node1.connect_peer(&node2.peer_id()).await.unwrap();
    
    // Verify replication
    let db2 = node2.log("shared-log", None).await.unwrap();
    wait_for_replication(&db2, 1).await;
    
    assert_eq!(db2.iterator(None).await.unwrap().count(), 1);
}
```

### Test Coverage

```bash
# Install coverage tools
cargo install cargo-tarpaulin
cargo install cargo-llvm-cov

# Quick coverage check (LCOV format)
cargo llvm-cov --all-features --workspace --lcov --output-path lcov.info

# Detailed coverage with HTML report (uses tarpaulin.toml config)
cargo tarpaulin

# Generate XML coverage for CI/CD tools
cargo tarpaulin --out Xml --output-dir ./coverage/

# Generate multiple formats
cargo tarpaulin --out Html --out Xml --out Lcov --output-dir ./coverage/

# View HTML report
open ./coverage/tarpaulin-report.html

# Check coverage threshold (fails if below 70%)
cargo tarpaulin --fail-under 70

# Coverage for specific package only
cargo tarpaulin --package guardian-db --out Html
```

#### Coverage Reports Location

- **LCOV**: `lcov.info` (for Codecov)
- **XML**: `./coverage/cobertura.xml` (for SonarQube, etc.)
- **HTML**: `./coverage/tarpaulin-report.html` (for viewing)

#### CI Coverage

The CI automatically generates coverage reports in multiple formats:
- **Codecov**: Automatic upload for pull request analysis
- **Artifacts**: HTML and XML reports available for download
- **Threshold**: Currently set to 70% minimum coverage

## 📚 Documentation

### Code Documentation

- Document all public functions
- Use code examples in comments
- Explain complex parameters
- Document error behavior

### External Documentation

```bash
# Generate documentation
cargo doc --open

# Check broken links
cargo doc --document-private-items

# Update README
# Keep examples synchronized with code
```

### Examples

- Add examples in the `examples/` folder
- Keep examples simple and focused
- Test examples as part of CI

```rust
// examples/basic_usage.rs
use guardian_db::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Simple and functional example
    let db = GuardianDB::new_mock().await?;
    let log = db.log("example", None).await?;
    log.add(b"Hello, GuardianDB!").await?;
    Ok(())
}
```

## 🔀 Pull Requests

### Before Submitting

- [ ] Code compiles without warnings
- [ ] All tests pass
- [ ] Code is formatted (`cargo fmt`)
- [ ] Clippy reports no issues (`cargo clippy`)
- [ ] Documentation updated
- [ ] Tests added for new features
- [ ] CHANGELOG.md updated (if applicable)

### PR Template

```markdown
## Description

Clear description of what this PR does.

## Type of Change

- [ ] Bug fix (change that fixes an issue)
- [ ] New feature (change that adds functionality)
- [ ] Breaking change (change that breaks compatibility)
- [ ] Documentation (documentation-only change)

## How to Test

1. Compile the project
2. Run `cargo test`
3. Specific test: `cargo test test_new_functionality`

## Checklist

- [ ] Code follows project standards
- [ ] Tests pass locally
- [ ] Documentation updated
- [ ] No clippy warnings
```

### Review Process

1. **Automated Checks**: CI must pass
2. **Code Review**: At least one maintainer approves
3. **Testing**: Manual tests if necessary
4. **Documentation**: Verify docs are updated
5. **Merge**: Squash commits if necessary

## 🐛 Issues

### Reporting Bugs

Use the bug report template:

```markdown
**Describe the bug**
Clear description of the problem.

**Steps to Reproduce**
1. Go to '...'
2. Click on '....'
3. Execute '....'
4. See error

**Expected Behavior**
What should happen.

**Screenshots**
If applicable, add screenshots.

**Environment:**
 - OS: [e.g. Ubuntu 20.04]
 - Rust Version: [e.g. 1.70.0]
 - GuardianDB Version: [e.g. 0.8.26]

**Additional Context**
Any other relevant information.
```

### Suggesting Features

```markdown
**Desired Feature**
Clear description of the feature.

**Problem Solved**
What problem does this feature solve?

**Proposed Solution**
How would you like it to work?

**Alternatives Considered**
Other solutions you considered?

**Additional Context**
Screenshots, mockups, etc.
```

## 👥 Code Review

### For Reviewers

#### What to Check

- **Functionality**: Does the code do what it should?
- **Tests**: Are changes adequately tested?
- **Performance**: Are there performance impacts?
- **Security**: Are there security vulnerabilities?
- **Design**: Is the design well thought out?
- **Documentation**: Is it well documented?

#### How to Give Feedback

```markdown
# ✅ Good feedback
"This method could benefit from more specific error handling. 
Consider using `GuardianError::InvalidInput` instead of generic."

# ❌ Bad feedback  
"This code is wrong."
```

#### Review Checklist

- [ ] Code compiles and tests pass
- [ ] Logic is correct
- [ ] Adequate error handling
- [ ] Clear documentation
- [ ] Acceptable performance
- [ ] No memory leaks
- [ ] Thread safety (if applicable)

### For Authors

#### Responding to Feedback

- Be receptive to suggestions
- Ask questions if you don't understand
- Explain design decisions when relevant
- Update code based on feedback

#### After Approval

```bash
# Rebasing if necessary
git rebase -i upstream/main

# Squash related commits
# Force push if necessary (on your branch)
git push --force-with-lease origin feature/my-feature
```

## 📊 Metrics and Quality

### Quality Goals

- **Test coverage**: > 85%
- **Performance**: No significant regressions
- **Documentation**: All public APIs documented
- **Clippy warnings**: Zero warnings
- **Memory leaks**: Zero leaks

### Monitoring Tools

```bash
# Continuous benchmarking
cargo bench

# Memory profiling
valgrind --tool=memcheck target/debug/guardian-db

# Dependency analysis
cargo audit

# License verification
cargo license
```

## 🎯 Priority Areas

### For New Contributors

1. **Documentation**: Improve examples and tutorials
2. **Tests**: Add integration tests
3. **Error handling**: Improve error messages
4. **Performance**: Benchmarks and optimizations

### For Experienced Contributors

1. **Core features**: New store functionalities
2. **IPFS integration**: Improve native integration
3. **P2P networking**: Optimize peer communication
4. **Security**: Security audits and improvements

## 📞 Communication

### Channels

- **GitHub Issues**: Bugs, features, technical discussions
- **GitHub Discussions**: General questions, ideas
- **Discord**: Real-time chat (link in README)

### Responding to Issues

- Respond within 48 hours when possible
- Be helpful and constructive
- Forward to experts when necessary
- Use appropriate labels

## 🏆 Recognition

Contributors are recognized through:

- **All Contributors**: Bot that adds contributors to README
- **Release Notes**: Mention in changelogs
- **Hall of Fame**: Special section for frequent contributors

## 📝 Conclusion

Thank you for contributing to GuardianDB! Your help is essential to make this project better for the entire community.

### Useful Resources

- [Rust Book](https://doc.rust-lang.org/book/)
- [Async Programming in Rust](https://rust-lang.github.io/async-book/)
- [API Guidelines](https://rust-lang.github.io/api-guidelines/)
- [OrbitDB Docs](https://orbitdb.org/) (reference)

### Questions?

If you have any questions about contributions, open an issue with the `question` label or contact us through the communication channels.

---

**Happy coding!** 🦀
