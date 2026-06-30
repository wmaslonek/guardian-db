use crate::guardian::error::{GuardianError, Result};
use iroh_blobs::Hash;
use std::fmt;

pub trait Address: fmt::Display + fmt::Debug + Send + Sync {
    /// Returns the database's root hash.
    fn get_root(&self) -> Hash;

    /// Returns the database path.
    fn get_path(&self) -> &str;

    /// Helper method for equality comparison.
    fn equals(&self, other: &dyn Address) -> bool;
}

/// SimpleAddress is a basic implementation of the Address trait
/// for testing and prototyping purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SimpleAddress {
    path: String,
}

impl SimpleAddress {
    /// Creates a new SimpleAddress with the specified path.
    pub fn new<S: Into<String>>(path: S) -> Self {
        Self { path: path.into() }
    }
}

impl Address for SimpleAddress {
    fn get_root(&self) -> Hash {
        // Return a zero hash for testing.
        Hash::from([0u8; 32])
    }

    fn get_path(&self) -> &str {
        &self.path
    }

    fn equals(&self, other: &dyn Address) -> bool {
        self.get_path() == other.get_path()
    }
}

impl fmt::Display for SimpleAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.path)
    }
}

/// A GuardianDB address composed of a root content Hash and an optional path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardianDBAddress {
    root: Hash,
    path: String,
}

impl GuardianDBAddress {
    /// Creates a new GuardianDBAddress with the specified Hash and path.
    pub fn new(root: Hash, path: String) -> Self {
        Self { root, path }
    }

    /// Creates a GuardianDBAddress with only a Hash (no path).
    pub fn from_hash(root: Hash) -> Self {
        Self {
            root,
            path: String::new(),
        }
    }

    /// Checks whether the address has an associated path.
    pub fn has_path(&self) -> bool {
        !self.path.is_empty()
    }

    /// Returns the root address (without a path).
    pub fn root_address(&self) -> Self {
        Self {
            root: self.root,
            path: String::new(),
        }
    }

    /// Adds or modifies the address path.
    pub fn with_path<P: Into<String>>(mut self, path: P) -> Self {
        self.path = path.into();
        self
    }
}

impl Address for GuardianDBAddress {
    fn get_root(&self) -> Hash {
        self.root
    }

    fn get_path(&self) -> &str {
        &self.path
    }

    fn equals(&self, other: &dyn Address) -> bool {
        self.get_root() == other.get_root() && self.get_path() == other.get_path()
    }
}

/// Converts the address to its string representation, such as "/GuardianDB/{hex_hash}/path".
impl fmt::Display for GuardianDBAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let base = format!("/GuardianDB/{}", hex::encode(self.root.as_bytes()));
        if self.path.is_empty() {
            write!(f, "{}", base)
        } else {
            write!(f, "{}/{}", base, self.path)
        }
    }
}

/// Checks whether a given string is a syntactically valid GuardianDB address.
pub fn is_valid(addr: &str) -> Result<()> {
    // Check whether the string is empty.
    if addr.trim().is_empty() {
        return Err(GuardianError::InvalidArgument(
            "Address cannot be empty".to_string(),
        ));
    }

    // Remove the prefix if it exists. unwrap_or returns the original slice if the prefix is not found.
    let trimmed = addr.strip_prefix("/GuardianDB/").unwrap_or(addr);

    // Check whether there is still content after removing the prefix.
    if trimmed.is_empty() {
        return Err(GuardianError::InvalidArgument(
            "Invalid address: only the GuardianDB prefix found".to_string(),
        ));
    }

    // Take the first part, which should be the hash in hex (64 characters).
    let hash_part = trimmed.split('/').next().ok_or_else(|| {
        GuardianError::InvalidArgument("Invalid address: incorrect or empty format".to_string())
    })?;

    // Check whether the hash is not empty.
    if hash_part.is_empty() {
        return Err(GuardianError::InvalidArgument(
            "Invalid address: hash cannot be empty".to_string(),
        ));
    }

    // Validate that it is a valid 64-character hex hash (32 bytes).
    if hash_part.len() != 64 {
        return Err(GuardianError::InvalidArgument(format!(
            "Invalid address: hash must be 64 hex characters, found {}",
            hash_part.len()
        )));
    }

    // Try to decode it as hex.
    hex::decode(hash_part).map_err(|e| {
        GuardianError::InvalidArgument(format!(
            "Invalid address: invalid hex hash '{}': {}",
            hash_part, e
        ))
    })?;

    Ok(())
}

/// Parses a string and returns a `GuardianDBAddress` instance if it is a valid address.
pub fn parse(addr: &str) -> Result<GuardianDBAddress> {
    // First, validate the address. On error, return a more descriptive error.
    if is_valid(addr).is_err() {
        return Err(GuardianError::InvalidArgument(format!(
            "Not a valid GuardianDB address: '{}'",
            addr
        )));
    }

    let trimmed = addr.strip_prefix("/GuardianDB/").unwrap_or(addr);

    // Split the string into at most two parts: the hash and the rest (the path).
    // This is more efficient than `split` followed by `join`.
    let mut parts = trimmed.splitn(2, '/');

    // `is_valid` already ensured that the first part exists and is a valid hash.
    // The `unwrap` here is safe.
    let hash_part = parts.next().unwrap();

    // Decode hex into bytes.
    let hash_bytes = hex::decode(hash_part).map_err(|e| {
        GuardianError::InvalidArgument(format!("Invalid address: could not decode the hash: {}", e))
    })?;

    // Convert into a 32-byte array.
    if hash_bytes.len() != 32 {
        return Err(GuardianError::InvalidArgument(format!(
            "Invalid address: hash must be 32 bytes, found {}",
            hash_bytes.len()
        )));
    }

    let mut hash_array = [0u8; 32];
    hash_array.copy_from_slice(&hash_bytes);
    let root_hash = Hash::from_bytes(hash_array);

    // The second part, if it exists, is the path. Otherwise, it is an empty string.
    let path = parts.next().unwrap_or("").to_string();

    Ok(GuardianDBAddress {
        root: root_hash,
        path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh_blobs::Hash;

    // Test hash (32 bytes = 64 hex chars).
    fn test_hash() -> Hash {
        Hash::from([0x12; 32])
    }

    fn test_hash_string() -> String {
        hex::encode(test_hash().as_bytes())
    }

    #[test]
    fn test_is_valid_success() {
        // Valid test hash (64 hex chars).
        let valid_addr = format!("/GuardianDB/{}", test_hash_string());
        assert!(is_valid(&valid_addr).is_ok());

        // Without the prefix should also work.
        let without_prefix = test_hash_string();
        assert!(is_valid(&without_prefix).is_ok());

        // With a path.
        let with_path = format!("/GuardianDB/{}/path/to/resource", test_hash_string());
        assert!(is_valid(&with_path).is_ok());
    }

    #[test]
    fn test_is_valid_failures() {
        // Empty string.
        assert!(is_valid("").is_err());
        assert!(is_valid("   ").is_err());

        // Prefix only.
        assert!(is_valid("/GuardianDB/").is_err());

        // Invalid hash (not valid hex).
        assert!(is_valid("/GuardianDB/invalid-hash").is_err());
        assert!(is_valid("invalid-hash").is_err());

        // Hash too short.
        assert!(is_valid("/GuardianDB/123abc").is_err());
    }

    #[test]
    fn test_parse_success() {
        let addr_str = format!("/GuardianDB/{}/path/to/resource", test_hash_string());
        let parsed = parse(&addr_str).unwrap();

        assert_eq!(parsed.get_path(), "path/to/resource");
        assert_eq!(parsed.to_string(), addr_str);
    }

    #[test]
    fn test_parse_without_path() {
        let addr_str = format!("/GuardianDB/{}", test_hash_string());
        let parsed = parse(&addr_str).unwrap();

        assert_eq!(parsed.get_path(), "");
        assert_eq!(parsed.to_string(), addr_str);
    }

    #[test]
    fn test_guardian_db_address_methods() {
        let hash = test_hash();

        // Test from_hash
        let addr = GuardianDBAddress::from_hash(hash);
        assert_eq!(addr.get_root(), hash);
        assert_eq!(addr.get_path(), "");
        assert!(!addr.has_path());

        // Test with_path
        let addr_with_path = addr.with_path("test/path");
        assert_eq!(addr_with_path.get_path(), "test/path");
        assert!(addr_with_path.has_path());

        // Test root_address
        let root_addr = addr_with_path.root_address();
        assert_eq!(root_addr.get_path(), "");
        assert!(!root_addr.has_path());
    }

    #[test]
    fn test_address_equality() {
        let hash = test_hash();
        let addr1 = GuardianDBAddress::new(hash, "path".to_string());
        let addr2 = GuardianDBAddress::new(hash, "path".to_string());
        let addr3 = GuardianDBAddress::new(hash, "different".to_string());

        assert!(addr1.equals(&addr2));
        assert!(!addr1.equals(&addr3));
    }
}
