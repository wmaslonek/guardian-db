# Contributing to GuardianDB Project

Welcome to the GuardianDB project! We're thrilled that you're considering contributing to our project. Every contribution helps, and we're delighted to have you on board.

## How to Contribute

Here are some steps to guide you through the process of contributing to the GuardianDB project:

### Step 1: Review the Issue Tickets

Before you start working on a contribution, please take a moment to look through the open issues in
the [issue tracker](https://github.com/wmaslonek/guardian-db/issues) for this project. This will give you an
idea of what kind of work is currently being planned or is in progress.

#### Report Bugs

- Use the bug issue template
- Include steps to reproduce the problem
- Provide environment information (Rust version, OS, etc.)
- Add relevant logs

#### Suggest Features

- Use the feature request issue template
- Clearly describe the problem it solves
- Explain why it would be useful for other users
- Consider multiple possible solutions

#### Improve Documentation

- Fix typos or grammar errors
- Add code examples
- Improve existing explanations
- Translate documentation

#### Contribute Code

- Implement new features
- Fix existing bugs
- Improve performance
- Add tests

### Step 2: Get Familiar with the Project

It's crucial to have an understanding of the project. Familiarize
yourself with the structure of the project, the purpose of different components, and how they
interact with each other. This will give you the context needed to make meaningful contributions.

### Step 3: Fork and Clone the Repository

Before you can start making changes, you'll need to fork the GuardianDB repository and clone it to your
local machine. This can be done via the GitHub website or the GitHub Desktop application. Here are
the steps:

1. Click the "Fork" button at the top-right of this page to create a copy of this project in your
   GitHub account.
2. Clone the repository to your local machine. You can do this by clicking the "Code" button on the
   GitHub website and copying the URL. Then open a terminal on your local machine and type
   `git clone [the URL you copied]`.

### Step 4: Create a New Branch

It's a good practice to create a new branch for each contribution you make. This keeps your changes
organized and separated from the main project, which can make the process of reviewing and merging
your changes easier. You can create a new branch by using the command
`git checkout -b [branch-name]`.

### Step 5: Make Your Changes

Once you have set up your local repository and created a new branch, you can start making changes.
Be sure to follow the coding standards and guidelines used in the rest of the project.

### Step 6: Validate code before opening a Pull Request

This will ensure that your changes are in line with our project's standards and guidelines. You can run the validation checks by opening a terminal, navigating to your local project directory, and typing:

```bash
# 1. Install dependencies and verify compilation
cargo build

# 2. Run tests
cargo test

# 3. Format all code before committing
cargo fmt

# 3.1 Check if formatted
cargo fmt --check

# 4. Run clippy
cargo clippy -- -D warnings

# 4.1 For more rigorous code
cargo clippy -- -D clippy::all
```

### Step 7: Submit a Pull Request

After you've made your changes and run the release preparation script you're ready to submit a pull request. This can be done through the GitHub website or the [GitHub Desktop application](https://desktop.github.com/).

When submitting your pull request, please provide a brief description of the changes you've made and the issue or issues that your changes address.

### Commit Types

GuardianDB pull requests titles look like this:

| **`type`** | **When to use** |
|--:         |-- |
| `feat`     | A new feature |
| `test`     | Changes that exclusively affect tests, either by adding new ones or correcting existing ones |
| `fix`      | A bug fix |
| `docs`     | Documentation only changes |
| `refactor` | A code change that neither fixes a bug nor adds a feature |
| `perf`     | A code change that improves performance |
| `deps`     | Dependency only updates |
| `chore`    | Changes to the build process or auxiliary tools and libraries |

Examples:
```bash
git commit -m "feat: add support for document queries"
git commit -m "fix: resolve memory leak in replicator"
git commit -m "docs: update README examples"
```

<details>
<summary>
Rust Conventions
</summary>
<br />

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
</details>

<details>
<summary>
Tests
</summary>
<br />

#### Running Tests

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

#### Test Coverage

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
</details>

## Code Review

#### What to Check

- **Functionality**: Does the code do what it should?
- **Tests**: Are changes adequately tested?
- **Performance**: Are there performance impacts?
- **Security**: Are there security vulnerabilities?
- **Design**: Is the design well thought out?
- **Documentation**: Is it well documented?

#### How to Give Feedback

```markdown
# ✔️ Good feedback
"This method could benefit from more specific error handling. 
Consider using `GuardianError::InvalidInput` instead of generic."

# ❌ Bad feedback  
"This code is wrong."
```

## Communication

### Channels

- **GitHub Issues**: Bugs, features, technical discussions
- **GitHub Discussions**: General questions, ideas
- **Discord**: Real-time chat (link in README)

### Questions?

If you have any questions about contributions, open an issue with the `question` label or contact us through the communication channels.

---

Thank you for contributing to GuardianDB! Your help is essential to make this project better for the entire community.
**Happy coding!** 🦀
