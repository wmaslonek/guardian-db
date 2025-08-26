use std::fmt::{Display, Formatter, Result};
use crate::error::{GuardianError, Result as DbResult};

#[async_trait::async_trait]
pub trait Datastore: Send + Sync {
    async fn get(&self, key: &[u8]) -> DbResult<Option<Vec<u8>>>;
    async fn put(&self, key: &[u8], value: &[u8]) -> DbResult<()>;
    async fn has(&self, key: &[u8]) -> DbResult<bool>;
    async fn delete(&self, key: &[u8]) -> DbResult<()>;
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Key {
    segments: Vec<String>,
}

impl Key {
    pub fn new<S: Into<String>>(path: S) -> Self {
        let s = path.into();
        let segments = s
            .split('/')
            .filter(|p| !p.is_empty())
            .map(|p| p.to_string())
            .collect();
        Self { segments }
    }

    pub fn child<S: Into<String>>(&self, name: S) -> Self {
        let mut segs = self.segments.clone();
        segs.push(name.into());
        Self { segments: segs }
    }

    pub fn parent(&self) -> Option<Self> {
        if self.segments.is_empty() {
            None
        } else {
            let mut segs = self.segments.clone();
            segs.pop();
            Some(Self { segments: segs })
        }
    }

    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    pub fn as_str(&self) -> String {
        format!("/{}", self.segments.join("/"))
    }

    pub fn as_bytes(&self) -> Vec<u8> {
        self.as_str().into_bytes()
    }
}

impl Display for Key {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        f.write_str(&self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub enum Order {
    #[default]
    Asc,
    Desc,
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub prefix: Option<Vec<u8>>,
    pub limit: Option<usize>,
    pub order: Order,
}

impl Query {
    pub fn builder() -> QueryBuilder {
        QueryBuilder::default()
    }
}

#[derive(Default)]
pub struct QueryBuilder {
    prefix: Option<Vec<u8>>,
    limit: Option<usize>,
    order: Order,
}

impl QueryBuilder {
    pub fn prefix<S: Into<String>>(mut self, p: S) -> Self {
        self.prefix = Some(p.into().into_bytes());
        self
    }
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }
    pub fn order(mut self, o: Order) -> Self {
        self.order = o;
        self
    }
    pub fn build(self) -> Query {
        Query {
            prefix: self.prefix,
            limit: self.limit,
            order: self.order,
        }
    }
}

#[derive(Clone)]
pub struct ResultItem {
    pub key: Key,
    pub value: Vec<u8>,
}

pub type Results = Vec<ResultItem>;