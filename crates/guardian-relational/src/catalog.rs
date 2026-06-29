//! The relational catalog: schemas, tables, columns, constraints, indexes,
//! sequences and views.
//!
//! The catalog is the authoritative, serializable description of the relational
//! schema. It is persisted as a single JSON document in GuardianDB's reserved
//! `__gdb_sql_catalog` collection and snapshotted for transaction isolation.

use crate::error::{RelError, Result};
use crate::types::SqlType;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::BTreeMap;

/// First OID handed out to user objects (mirrors PostgreSQL's `FirstNormalObjectId`).
pub const FIRST_USER_OID: u32 = 16384;

/// A `(schema, name)` key used throughout the catalog.
///
/// Because it is used as a `BTreeMap` key and serialized to JSON (where map keys
/// must be strings), it (de)serializes to a `"schema\u{1f}name"` string.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QualifiedName {
    pub schema: String,
    pub name: String,
}

impl Serialize for QualifiedName {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&format!("{}\u{1f}{}", self.schema, self.name))
    }
}

impl<'de> Deserialize<'de> for QualifiedName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.split_once('\u{1f}') {
            Some((schema, name)) => Ok(QualifiedName::new(schema, name)),
            None => Err(D::Error::custom("malformed qualified name key")),
        }
    }
}

impl QualifiedName {
    pub fn new(schema: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            schema: schema.into(),
            name: name.into(),
        }
    }

    pub fn to_string_qualified(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema {
    pub name: String,
    pub oid: u32,
    pub owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    pub ty: SqlType,
    pub nullable: bool,
    /// Raw SQL text of the DEFAULT expression, if any.
    pub default: Option<String>,
    /// Name of the backing sequence when the column is `serial`/`bigserial`.
    pub identity_sequence: Option<String>,
    pub ordinal: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimaryKey {
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UniqueConstraint {
    pub name: String,
    pub columns: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReferentialAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl ReferentialAction {
    pub fn as_sql(&self) -> &'static str {
        match self {
            ReferentialAction::NoAction => "NO ACTION",
            ReferentialAction::Restrict => "RESTRICT",
            ReferentialAction::Cascade => "CASCADE",
            ReferentialAction::SetNull => "SET NULL",
            ReferentialAction::SetDefault => "SET DEFAULT",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
    pub on_delete: ReferentialAction,
    pub on_update: ReferentialAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckConstraint {
    pub name: String,
    /// Raw SQL text of the CHECK expression.
    pub expr: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Table {
    pub oid: u32,
    pub schema: String,
    pub name: String,
    pub columns: Vec<Column>,
    pub primary_key: Option<PrimaryKey>,
    pub uniques: Vec<UniqueConstraint>,
    pub foreign_keys: Vec<ForeignKey>,
    pub checks: Vec<CheckConstraint>,
    /// Opaque storage collection name for this table's rows.
    pub storage_collection: String,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn column_mut(&mut self, name: &str) -> Option<&mut Column> {
        self.columns.iter_mut().find(|c| c.name == name)
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    pub fn qualified(&self) -> QualifiedName {
        QualifiedName::new(self.schema.clone(), self.name.clone())
    }

    /// The columns that make up the primary key, or empty if none.
    pub fn pk_columns(&self) -> Vec<String> {
        self.primary_key
            .as_ref()
            .map(|pk| pk.columns.clone())
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub oid: u32,
    pub name: String,
    pub schema: String,
    pub table: String,
    pub columns: Vec<String>,
    pub unique: bool,
    pub primary: bool,
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sequence {
    pub schema: String,
    pub name: String,
    pub current: i64,
    pub increment: i64,
    pub start: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub oid: u32,
    pub schema: String,
    pub name: String,
    /// The SQL text of the SELECT defining the view.
    pub query: String,
    pub columns: Vec<String>,
}

/// The authoritative, serializable relational catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub database: String,
    schemas: BTreeMap<String, Schema>,
    tables: BTreeMap<QualifiedName, Table>,
    indexes: BTreeMap<QualifiedName, Index>,
    sequences: BTreeMap<QualifiedName, Sequence>,
    views: BTreeMap<QualifiedName, View>,
    next_oid: u32,
    pub search_path: Vec<String>,
}

impl Catalog {
    /// A fresh catalog containing only the `public` and system schemas.
    pub fn new(database: impl Into<String>) -> Self {
        let mut catalog = Self {
            database: database.into(),
            schemas: BTreeMap::new(),
            tables: BTreeMap::new(),
            indexes: BTreeMap::new(),
            sequences: BTreeMap::new(),
            views: BTreeMap::new(),
            next_oid: FIRST_USER_OID,
            search_path: vec!["public".to_string()],
        };
        // System schemas always present.
        for sys in ["pg_catalog", "information_schema"] {
            let oid = catalog.allocate_oid();
            catalog.schemas.insert(
                sys.to_string(),
                Schema { name: sys.to_string(), oid, owner: "guardian".into() },
            );
        }
        let oid = catalog.allocate_oid();
        catalog
            .schemas
            .insert("public".to_string(), Schema { name: "public".into(), oid, owner: "guardian".into() });
        catalog
    }

    pub fn allocate_oid(&mut self) -> u32 {
        let oid = self.next_oid;
        self.next_oid += 1;
        oid
    }

    // ---- schemas -------------------------------------------------------

    pub fn has_schema(&self, name: &str) -> bool {
        self.schemas.contains_key(name)
    }

    pub fn schemas(&self) -> impl Iterator<Item = &Schema> {
        self.schemas.values()
    }

    pub fn create_schema(&mut self, name: &str, if_not_exists: bool) -> Result<()> {
        if self.schemas.contains_key(name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(RelError::DuplicateSchema(name.to_string()));
        }
        let oid = self.allocate_oid();
        self.schemas
            .insert(name.to_string(), Schema { name: name.to_string(), oid, owner: "guardian".into() });
        Ok(())
    }

    pub fn drop_schema(&mut self, name: &str, if_exists: bool, cascade: bool) -> Result<()> {
        if !self.schemas.contains_key(name) {
            if if_exists {
                return Ok(());
            }
            return Err(RelError::UndefinedSchema(name.to_string()));
        }
        let table_names: Vec<QualifiedName> = self
            .tables
            .keys()
            .filter(|k| k.schema == name)
            .cloned()
            .collect();
        if !table_names.is_empty() && !cascade {
            return Err(RelError::FeatureNotSupported(format!(
                "cannot drop schema {name} because it contains objects (use CASCADE)"
            )));
        }
        for t in table_names {
            self.drop_table_qualified(&t)?;
        }
        self.schemas.remove(name);
        Ok(())
    }

    // ---- resolution ----------------------------------------------------

    /// Resolve a possibly-unqualified table name using the search path.
    pub fn resolve_table_name(&self, schema: Option<&str>, name: &str) -> Option<QualifiedName> {
        if let Some(schema) = schema {
            let q = QualifiedName::new(schema, name);
            if self.tables.contains_key(&q) || self.views.contains_key(&q) {
                return Some(q);
            }
            return None;
        }
        for schema in &self.search_path {
            let q = QualifiedName::new(schema.clone(), name);
            if self.tables.contains_key(&q) || self.views.contains_key(&q) {
                return Some(q);
            }
        }
        None
    }

    /// The schema an unqualified, to-be-created object should live in.
    pub fn creation_schema(&self, schema: Option<&str>) -> Result<String> {
        match schema {
            Some(s) => {
                if !self.schemas.contains_key(s) {
                    return Err(RelError::UndefinedSchema(s.to_string()));
                }
                Ok(s.to_string())
            }
            None => Ok(self
                .search_path
                .first()
                .cloned()
                .unwrap_or_else(|| "public".to_string())),
        }
    }

    // ---- tables --------------------------------------------------------

    pub fn tables(&self) -> impl Iterator<Item = &Table> {
        self.tables.values()
    }

    pub fn get_table(&self, q: &QualifiedName) -> Option<&Table> {
        self.tables.get(q)
    }

    pub fn get_table_mut(&mut self, q: &QualifiedName) -> Option<&mut Table> {
        self.tables.get_mut(q)
    }

    pub fn require_table(&self, q: &QualifiedName) -> Result<&Table> {
        self.tables
            .get(q)
            .ok_or_else(|| RelError::UndefinedTable(q.to_string_qualified()))
    }

    pub fn has_table(&self, q: &QualifiedName) -> bool {
        self.tables.contains_key(q)
    }

    /// Register a new table. The storage collection name is derived from the oid.
    pub fn insert_table(&mut self, mut table: Table) -> Result<()> {
        let q = table.qualified();
        if self.tables.contains_key(&q) || self.views.contains_key(&q) {
            return Err(RelError::DuplicateTable(q.to_string_qualified()));
        }
        if !self.schemas.contains_key(&table.schema) {
            return Err(RelError::UndefinedSchema(table.schema.clone()));
        }
        if table.storage_collection.is_empty() {
            table.storage_collection = format!("__gdb_sql_rows_{}", table.oid);
        }
        self.tables.insert(q, table);
        Ok(())
    }

    pub fn drop_table_qualified(&mut self, q: &QualifiedName) -> Result<Table> {
        let table = self
            .tables
            .remove(q)
            .ok_or_else(|| RelError::UndefinedTable(q.to_string_qualified()))?;
        // Drop dependent indexes and sequences.
        let idx_keys: Vec<QualifiedName> = self
            .indexes
            .iter()
            .filter(|(_, i)| i.schema == q.schema && i.table == q.name)
            .map(|(k, _)| k.clone())
            .collect();
        for k in idx_keys {
            self.indexes.remove(&k);
        }
        for col in &table.columns {
            if let Some(seq) = &col.identity_sequence {
                let sk = QualifiedName::new(q.schema.clone(), seq.clone());
                self.sequences.remove(&sk);
            }
        }
        Ok(table)
    }

    // ---- indexes -------------------------------------------------------

    pub fn indexes(&self) -> impl Iterator<Item = &Index> {
        self.indexes.values()
    }

    pub fn indexes_for_table(&self, schema: &str, table: &str) -> Vec<&Index> {
        self.indexes
            .values()
            .filter(|i| i.schema == schema && i.table == table)
            .collect()
    }

    pub fn get_index(&self, q: &QualifiedName) -> Option<&Index> {
        self.indexes.get(q)
    }

    pub fn insert_index(&mut self, index: Index) -> Result<()> {
        let q = QualifiedName::new(index.schema.clone(), index.name.clone());
        if self.indexes.contains_key(&q) {
            return Err(RelError::DuplicateIndex(q.to_string_qualified()));
        }
        self.indexes.insert(q, index);
        Ok(())
    }

    pub fn drop_index(&mut self, schema: Option<&str>, name: &str, if_exists: bool) -> Result<()> {
        let q = match schema {
            Some(s) => QualifiedName::new(s, name),
            None => {
                // Search path lookup for the index name.
                let found = self
                    .search_path
                    .iter()
                    .map(|s| QualifiedName::new(s.clone(), name))
                    .find(|q| self.indexes.contains_key(q));
                match found {
                    Some(q) => q,
                    None => {
                        if if_exists {
                            return Ok(());
                        }
                        return Err(RelError::UndefinedIndex(name.to_string()));
                    }
                }
            }
        };
        if self.indexes.remove(&q).is_none() && !if_exists {
            return Err(RelError::UndefinedIndex(q.to_string_qualified()));
        }
        Ok(())
    }

    // ---- sequences -----------------------------------------------------

    pub fn sequences(&self) -> impl Iterator<Item = &Sequence> {
        self.sequences.values()
    }

    pub fn create_sequence(&mut self, schema: &str, name: &str) -> Result<()> {
        let q = QualifiedName::new(schema, name);
        self.sequences.entry(q).or_insert(Sequence {
            schema: schema.to_string(),
            name: name.to_string(),
            current: 0,
            increment: 1,
            start: 1,
        });
        Ok(())
    }

    /// Advance a sequence and return the next value.
    pub fn next_sequence_value(&mut self, schema: &str, name: &str) -> Result<i64> {
        let q = QualifiedName::new(schema, name);
        let seq = self
            .sequences
            .get_mut(&q)
            .ok_or_else(|| RelError::UndefinedObject(format!("sequence {schema}.{name}")))?;
        let next = if seq.current == 0 {
            seq.start
        } else {
            seq.current + seq.increment
        };
        seq.current = next;
        Ok(next)
    }

    /// Ensure a sequence's current value is at least `value` (used after explicit inserts).
    pub fn observe_sequence_value(&mut self, schema: &str, name: &str, value: i64) {
        let q = QualifiedName::new(schema, name);
        if let Some(seq) = self.sequences.get_mut(&q) {
            if value > seq.current {
                seq.current = value;
            }
        }
    }

    // ---- views ---------------------------------------------------------

    pub fn views(&self) -> impl Iterator<Item = &View> {
        self.views.values()
    }

    pub fn get_view(&self, q: &QualifiedName) -> Option<&View> {
        self.views.get(q)
    }

    pub fn insert_view(&mut self, view: View) -> Result<()> {
        let q = QualifiedName::new(view.schema.clone(), view.name.clone());
        if self.tables.contains_key(&q) || self.views.contains_key(&q) {
            return Err(RelError::DuplicateTable(q.to_string_qualified()));
        }
        self.views.insert(q, view);
        Ok(())
    }

    pub fn drop_view(&mut self, q: &QualifiedName, if_exists: bool) -> Result<()> {
        if self.views.remove(q).is_none() && !if_exists {
            return Err(RelError::UndefinedTable(q.to_string_qualified()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_table(cat: &mut Catalog) -> Table {
        let oid = cat.allocate_oid();
        Table {
            oid,
            schema: "public".into(),
            name: "users".into(),
            columns: vec![
                Column { name: "id".into(), ty: SqlType::Integer, nullable: false, default: None, identity_sequence: None, ordinal: 0 },
                Column { name: "email".into(), ty: SqlType::Text, nullable: false, default: None, identity_sequence: None, ordinal: 1 },
            ],
            primary_key: Some(PrimaryKey { name: "users_pkey".into(), columns: vec!["id".into()] }),
            uniques: vec![],
            foreign_keys: vec![],
            checks: vec![],
            storage_collection: String::new(),
        }
    }

    #[test]
    fn create_and_resolve_table() {
        let mut cat = Catalog::new("app");
        let t = sample_table(&mut cat);
        cat.insert_table(t).unwrap();
        let q = cat.resolve_table_name(None, "users").unwrap();
        assert_eq!(q.schema, "public");
        assert!(cat.get_table(&q).unwrap().storage_collection.starts_with("__gdb_sql_rows_"));
    }

    #[test]
    fn duplicate_table_errors() {
        let mut cat = Catalog::new("app");
        let t = sample_table(&mut cat);
        cat.insert_table(t.clone()).unwrap();
        let t2 = sample_table(&mut cat);
        assert!(matches!(cat.insert_table(t2), Err(RelError::DuplicateTable(_))));
    }

    #[test]
    fn sequence_advances() {
        let mut cat = Catalog::new("app");
        cat.create_sequence("public", "users_id_seq").unwrap();
        assert_eq!(cat.next_sequence_value("public", "users_id_seq").unwrap(), 1);
        assert_eq!(cat.next_sequence_value("public", "users_id_seq").unwrap(), 2);
        cat.observe_sequence_value("public", "users_id_seq", 10);
        assert_eq!(cat.next_sequence_value("public", "users_id_seq").unwrap(), 11);
    }

    #[test]
    fn drop_schema_requires_cascade() {
        let mut cat = Catalog::new("app");
        cat.create_schema("app", false).unwrap();
        let oid = cat.allocate_oid();
        let mut t = sample_table(&mut cat);
        t.schema = "app".into();
        t.oid = oid;
        cat.insert_table(t).unwrap();
        assert!(cat.drop_schema("app", false, false).is_err());
        assert!(cat.drop_schema("app", false, true).is_ok());
    }

    #[test]
    fn catalog_round_trips_json() {
        let mut cat = Catalog::new("app");
        let t = sample_table(&mut cat);
        cat.insert_table(t).unwrap();
        let json = serde_json::to_value(&cat).unwrap();
        let back: Catalog = serde_json::from_value(json).unwrap();
        assert!(back.resolve_table_name(None, "users").is_some());
    }
}
