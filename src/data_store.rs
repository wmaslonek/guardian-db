use crate::guardian::error::Result as DbResult;
use std::fmt::{Display, Formatter, Result as FmtResult};

/// Main trait for datastore operations.
///
/// Provides an asynchronous interface for basic CRUD operations
/// and queries with advanced filters.
#[async_trait::async_trait]
pub trait Datastore: Send + Sync + std::any::Any {
    /// Retrieves the value associated with the key.
    async fn get(&self, key: &[u8]) -> DbResult<Option<Vec<u8>>>;

    /// Stores a value with the specified key.
    async fn put(&self, key: &[u8], value: &[u8]) -> DbResult<()>;

    /// Checks whether a key exists in the datastore.
    async fn has(&self, key: &[u8]) -> DbResult<bool>;

    /// Removes a key and its value from the datastore.
    async fn delete(&self, key: &[u8]) -> DbResult<()>;

    /// Runs a query with filters and returns paginated results.
    async fn query(&self, query: &Query) -> DbResult<Results>;

    /// Returns all keys with a given prefix.
    async fn list_keys(&self, prefix: &[u8]) -> DbResult<Vec<Key>>;

    /// Helper method for downcasting.
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Represents a hierarchical key in the datastore.
///
/// Allows navigating a directory-like structure with parent/child
/// operations and conversions to different formats.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    segments: Vec<String>,
}

impl Key {
    /// Creates a new key from a path.
    ///
    /// # Examples
    /// ```ignore
    /// use guardian_db::data_store::Key;
    ///
    /// let key = Key::new("/users/alice/profile");
    /// let key = Key::new("config/database/host");
    /// ```
    pub fn new<S: Into<String>>(path: S) -> Self {
        let s = path.into();
        let segments = s
            .split('/')
            .filter(|p| !p.is_empty())
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty()) // Remove empty segments after trimming.
            .collect();
        Self { segments }
    }

    /// Creates an empty root key.
    #[allow(dead_code)]
    pub fn root() -> Self {
        Self { segments: vec![] }
    }

    /// Creates a child key by adding a segment.
    pub fn child<S: Into<String>>(&self, name: S) -> Self {
        let child_name = name.into().trim().to_string();
        if child_name.is_empty() {
            return self.clone();
        }

        let mut segs = self.segments.clone();
        segs.push(child_name);
        Self { segments: segs }
    }

    /// Returns the parent key, if it exists.
    #[allow(dead_code)]
    pub fn parent(&self) -> Option<Self> {
        if self.segments.is_empty() {
            None
        } else {
            let mut segs = self.segments.clone();
            segs.pop();
            Some(Self { segments: segs })
        }
    }

    /// Checks whether the key is empty (root).
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Returns the last segment of the key.
    pub fn name(&self) -> Option<&str> {
        self.segments.last().map(|s| s.as_str())
    }

    /// Returns all segments of the key.
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Returns the depth of the key (number of segments).
    pub fn depth(&self) -> usize {
        self.segments.len()
    }

    /// Checks whether this key is a descendant of another.
    pub fn is_descendant_of(&self, other: &Key) -> bool {
        if other.segments.len() >= self.segments.len() {
            return false;
        }

        self.segments[..other.segments.len()] == other.segments
    }

    /// Converts to a string in path format.
    pub fn as_str(&self) -> String {
        if self.segments.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", self.segments.join("/"))
        }
    }

    /// Converts to UTF-8 bytes.
    #[allow(dead_code)]
    pub fn as_bytes(&self) -> Vec<u8> {
        self.as_str().into_bytes()
    }
}

impl Display for Key {
    fn fmt(&self, f: &mut Formatter<'_>) -> FmtResult {
        f.write_str(&self.as_str())
    }
}

impl From<&str> for Key {
    fn from(path: &str) -> Self {
        Key::new(path)
    }
}

impl From<String> for Key {
    fn from(path: String) -> Self {
        Key::new(path)
    }
}

/// Sort order for queries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    #[default]
    Asc,
    Desc,
}

/// Query configuration for searching the datastore.
///
/// Allows filtering by prefix, limiting results and defining ordering.
#[derive(Clone, Debug, Default)]
pub struct Query {
    /// Prefix to filter keys (None = all keys).
    pub prefix: Option<Key>,
    /// Maximum number of results (None = no limit).
    pub limit: Option<usize>,
    /// Sort order.
    pub order: Order,
    /// Offset for pagination.
    pub offset: Option<usize>,
}

impl Query {
    /// Creates a builder for constructing complex queries.
    pub fn builder() -> QueryBuilder {
        QueryBuilder::default()
    }

    /// Creates a simple query with only a prefix.
    pub fn with_prefix<K: Into<Key>>(prefix: K) -> Self {
        Self {
            prefix: Some(prefix.into()),
            limit: None,
            order: Order::default(),
            offset: None,
        }
    }

    /// Creates a query that returns all items.
    #[allow(dead_code)]
    pub fn all() -> Self {
        Self::default()
    }
}

/// Builder for constructing queries fluently.
#[derive(Default)]
pub struct QueryBuilder {
    prefix: Option<Key>,
    limit: Option<usize>,
    order: Order,
    offset: Option<usize>,
}

impl QueryBuilder {
    /// Sets the prefix to filter keys.
    pub fn prefix<K: Into<Key>>(mut self, prefix: K) -> Self {
        self.prefix = Some(prefix.into());
        self
    }

    /// Sets the maximum number of results.
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Sets the sort order.
    #[allow(dead_code)]
    pub fn order(mut self, o: Order) -> Self {
        self.order = o;
        self
    }

    /// Sets the offset for pagination.
    pub fn offset(mut self, n: usize) -> Self {
        self.offset = Some(n);
        self
    }

    /// Builds the final query.
    pub fn build(self) -> Query {
        Query {
            prefix: self.prefix,
            limit: self.limit,
            order: self.order,
            offset: self.offset,
        }
    }
}

/// Result item of a query.
///
/// Contains a key and its associated value.
#[derive(Clone, Debug)]
pub struct ResultItem {
    pub key: Key,
    pub value: Vec<u8>,
}

impl ResultItem {
    /// Creates a new result item.
    pub fn new(key: Key, value: Vec<u8>) -> Self {
        Self { key, value }
    }

    /// Converts the value to a UTF-8 string, if possible.
    pub fn value_as_string(&self) -> std::result::Result<String, std::string::FromUtf8Error> {
        String::from_utf8(self.value.clone())
    }

    /// Checks whether the value is empty.
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    /// Returns the value size in bytes.
    pub fn size(&self) -> usize {
        self.value.len()
    }
}

/// Collection of results from a query.
pub type Results = Vec<ResultItem>;

/// Useful extensions for working with Results.
pub trait ResultsExt {
    /// Filters results by the minimum value size.
    fn filter_by_min_size(&self, min_size: usize) -> Results;

    /// Returns only the keys of the results.
    fn keys(&self) -> Vec<Key>;

    /// Returns the total number of bytes across all values.
    fn total_size(&self) -> usize;
}

impl ResultsExt for Results {
    fn filter_by_min_size(&self, min_size: usize) -> Results {
        self.iter()
            .filter(|item| item.value.len() >= min_size)
            .cloned()
            .collect()
    }

    fn keys(&self) -> Vec<Key> {
        self.iter().map(|item| item.key.clone()).collect()
    }

    fn total_size(&self) -> usize {
        self.iter().map(|item| item.value.len()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_creation() {
        let key = Key::new("/users/alice/profile");
        assert_eq!(key.segments(), &["users", "alice", "profile"]);
        assert_eq!(key.as_str(), "/users/alice/profile");
        assert_eq!(key.depth(), 3);
    }

    #[test]
    fn test_key_operations() {
        let root = Key::root();
        assert!(root.is_empty());
        assert_eq!(root.as_str(), "/");

        let child = root.child("config").child("database");
        assert_eq!(child.as_str(), "/config/database");

        let parent = child.parent().unwrap();
        assert_eq!(parent.as_str(), "/config");

        assert_eq!(child.name().unwrap(), "database");
    }

    #[test]
    fn test_key_hierarchy() {
        let parent = Key::new("/users/alice");
        let child = Key::new("/users/alice/profile");
        let other = Key::new("/users/bob");

        assert!(child.is_descendant_of(&parent));
        assert!(!other.is_descendant_of(&parent));
        assert!(!parent.is_descendant_of(&child));
    }

    #[test]
    fn test_query_builder() {
        let query = Query::builder()
            .prefix("/users")
            .limit(10)
            .order(Order::Desc)
            .offset(5)
            .build();

        assert_eq!(query.prefix.as_ref().unwrap().as_str(), "/users");
        assert_eq!(query.limit, Some(10));
        assert_eq!(query.order, Order::Desc);
        assert_eq!(query.offset, Some(5));
    }

    #[test]
    fn test_result_item() {
        let key = Key::new("/test/key");
        let value = b"test value".to_vec();
        let item = ResultItem::new(key.clone(), value.clone());

        assert_eq!(item.key, key);
        assert_eq!(item.value, value);
        assert_eq!(item.size(), 10);
        assert!(!item.is_empty());
        assert_eq!(item.value_as_string().unwrap(), "test value");
    }

    #[test]
    fn test_results_ext() {
        let results = vec![
            ResultItem::new(Key::new("/small"), b"hi".to_vec()),
            ResultItem::new(Key::new("/large"), b"hello world".to_vec()),
            ResultItem::new(Key::new("/medium"), b"hello".to_vec()),
        ];

        let filtered = results.filter_by_min_size(5);
        assert_eq!(filtered.len(), 2);

        let keys = results.keys();
        assert_eq!(keys.len(), 3);

        let total = results.total_size();
        assert_eq!(total, 2 + 11 + 5); // "hi" + "hello world" + "hello"
    }
}
