//! The relational catalog: schemas, tables, columns, constraints, indexes,
//! sequences and views.
//!
//! The catalog is the authoritative, serializable description of the relational
//! schema. It is persisted as a single JSON document in GuardianDB's reserved
//! `__gdb_sql_catalog` collection and snapshotted for transaction isolation.

use crate::relational::error::{RelError, Result};
use crate::relational::types::SqlType;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::{BTreeMap, HashMap};

/// First OID handed out to user objects (mirrors PostgreSQL's `FirstNormalObjectId`).
pub const FIRST_USER_OID: u32 = 16384;

/// Suffix marking a catalog extension entry as bound to the PostgreSQL
/// sidecar runtime. Stored inside the version string (`"1.10@sidecar"`) so
/// the serialized catalog shape is unchanged and old documents keep loading.
/// Native version strings never contain `@` (they come from the registry).
const SIDECAR_MARKER: &str = "@sidecar";

fn strip_sidecar_marker(version: &str) -> &str {
    version.strip_suffix(SIDECAR_MARKER).unwrap_or(version)
}

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

/// A composite foreign key's `MATCH` mode. `MATCH SIMPLE` is PostgreSQL's
/// default when neither `MATCH FULL` nor `MATCH PARTIAL` is written.
///
/// There is deliberately no `Partial` variant: PostgreSQL's own grammar
/// accepts `MATCH PARTIAL`, but PostgreSQL itself has never implemented it —
/// `CREATE TABLE`'s own reference page (the `REFERENCES` clause, under
/// "Key Match Types") documents it as not yet implemented, and real
/// PostgreSQL raises `MATCH PARTIAL not yet implemented` for it. GuardianDB
/// mirrors that exact non-implementation (typed `0A000`, see
/// `crate::sql::ddl::reject_unsupported_match`) instead of building it out,
/// which *is* 1:1 parity here — so this type only models the two match kinds
/// that actually run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MatchType {
    /// Any NULL FK column exempts the row from the check.
    #[default]
    Simple,
    /// A composite key must be either all-NULL (exempt) or all-non-NULL and
    /// matching a parent row; a mix of NULL and non-NULL components is
    /// itself a violation — PostgreSQL's message (verified against
    /// `src/backend/utils/adt/ri_triggers.c`): "MATCH FULL does not allow
    /// mixing of null and nonnull key values." (`23503`).
    Full,
}

/// A foreign key's `[NOT] DEFERRABLE [INITIALLY {DEFERRED|IMMEDIATE}]`
/// declaration — whether, and when, `SET CONSTRAINTS` can postpone its `NO
/// ACTION` check to `COMMIT` (see `crate::sql::exec::ConstraintModes` and
/// `crate::sql::fk`). `NotDeferrable` is PostgreSQL's default and the only
/// kind that existed before this field was added, so it is also this enum's
/// `Default`/serde default (old catalog documents keep loading unchanged).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Deferrable {
    /// `NOT DEFERRABLE` (or no characteristics at all): always checked
    /// immediately. `SET CONSTRAINTS` can never change that — PostgreSQL
    /// raises `42809` ("constraint ... is not deferrable") if asked to defer
    /// one (see `crate::sql::engine::Session::exec_set_constraints`).
    #[default]
    NotDeferrable,
    /// `DEFERRABLE` (bare, or with an explicit `INITIALLY IMMEDIATE`): a
    /// fresh transaction checks it immediately, but `SET CONSTRAINTS ...
    /// DEFERRED` can defer it to `COMMIT` for the rest of the transaction.
    DeferrableImmediate,
    /// `DEFERRABLE INITIALLY DEFERRED` — or a *bare* `INITIALLY DEFERRED`
    /// with no explicit `DEFERRABLE`/`NOT DEFERRABLE` keyword, which
    /// PostgreSQL's grammar treats as implying `DEFERRABLE` too (see
    /// `processCASbits` / `ConstraintAttributeSpec` in
    /// `src/backend/parser/gram.y`: the `CAS_INITIALLY_DEFERRED` bit alone
    /// sets `deferrable = true`). A fresh transaction checks it at `COMMIT`
    /// by default, unless `SET CONSTRAINTS ... IMMEDIATE` overrides that for
    /// the transaction.
    DeferrableDeferred,
}

impl Deferrable {
    /// Can `SET CONSTRAINTS` ever change this constraint's check timing?
    pub fn is_deferrable(self) -> bool {
        !matches!(self, Deferrable::NotDeferrable)
    }

    /// The mode a fresh transaction starts in, absent any `SET CONSTRAINTS`
    /// naming this constraint (or `ALL`) earlier in the same transaction.
    pub fn initially_deferred(self) -> bool {
        matches!(self, Deferrable::DeferrableDeferred)
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
    /// `MATCH SIMPLE` (default) or `MATCH FULL`. `#[serde(default)]` so
    /// catalogs written before this field existed keep loading as `MATCH
    /// SIMPLE`, their only possible pre-existing meaning.
    #[serde(default)]
    pub match_type: MatchType,
    /// `[NOT] DEFERRABLE [INITIALLY ...]`. Same backward-compatible serde
    /// default as `match_type` — `NOT DEFERRABLE` is also the only meaning a
    /// pre-existing catalog document could have had.
    #[serde(default)]
    pub deferrable: Deferrable,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckConstraint {
    pub name: String,
    /// Raw SQL text of the CHECK expression.
    pub expr: String,
}

/// The command class a row-security policy applies to (`FOR ALL | SELECT |
/// INSERT | UPDATE | DELETE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PolicyCmd {
    All,
    Select,
    Insert,
    Update,
    Delete,
}

impl PolicyCmd {
    /// The `pg_policies.cmd` spelling.
    pub fn as_sql(&self) -> &'static str {
        match self {
            PolicyCmd::All => "ALL",
            PolicyCmd::Select => "SELECT",
            PolicyCmd::Insert => "INSERT",
            PolicyCmd::Update => "UPDATE",
            PolicyCmd::Delete => "DELETE",
        }
    }
}

/// A row-level security policy (`CREATE POLICY`). Expressions are stored as
/// raw SQL text (validated to parse at CREATE time) and evaluated per row at
/// execution time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    pub name: String,
    pub cmd: PolicyCmd,
    /// Roles the policy applies to; empty means PUBLIC (every role).
    pub roles: Vec<String>,
    /// Raw SQL text of the `USING` expression (visibility of existing rows).
    pub using_expr: Option<String>,
    /// Raw SQL text of the `WITH CHECK` expression (validity of new rows).
    pub check_expr: Option<String>,
    /// `AS PERMISSIVE` (ORed) vs `AS RESTRICTIVE` (ANDed).
    pub permissive: bool,
}

/// When a trigger fires relative to the triggering event (`BEFORE`/`AFTER`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerTiming {
    Before,
    After,
    /// `INSTEAD OF` — only on views, always `FOR EACH ROW`.
    InsteadOf,
}

impl TriggerTiming {
    /// The `TG_WHEN` spelling.
    pub fn as_sql(&self) -> &'static str {
        match self {
            TriggerTiming::Before => "BEFORE",
            TriggerTiming::After => "AFTER",
            TriggerTiming::InsteadOf => "INSTEAD OF",
        }
    }
}

/// `FOR EACH ROW` vs `FOR EACH STATEMENT` (PostgreSQL's default when the
/// clause is omitted is `STATEMENT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerLevel {
    Row,
    Statement,
}

impl TriggerLevel {
    /// The `TG_LEVEL` spelling.
    pub fn as_sql(&self) -> &'static str {
        match self {
            TriggerLevel::Row => "ROW",
            TriggerLevel::Statement => "STATEMENT",
        }
    }
}

/// One triggering event of a trigger definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TriggerEventDef {
    Insert,
    /// UPDATE, with the optional `OF col, ...` list (empty = every UPDATE).
    /// Matching is on the UPDATE statement's assignment-target column names,
    /// NOT on value changes — PostgreSQL's `UPDATE OF` semantics.
    Update {
        columns: Vec<String>,
    },
    Delete,
    /// TRUNCATE event — only valid for `FOR EACH STATEMENT` triggers.
    Truncate,
}

fn default_true() -> bool {
    true
}

/// A trigger (`CREATE TRIGGER`). Stored on its [`Table`] — like [`Policy`] —
/// so `DROP TABLE` cleanup is implicit. The `WHEN` condition is stored as raw
/// SQL text and re-parsed at fire time, the exact pattern `Policy.using_expr`
/// follows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerDef {
    pub oid: u32,
    /// Case-folded trigger name, unique per table.
    pub name: String,
    pub timing: TriggerTiming,
    /// At least one event; `INSERT OR UPDATE` etc. store several.
    pub events: Vec<TriggerEventDef>,
    pub level: TriggerLevel,
    /// Raw SQL text of the `WHEN` condition (row triggers only).
    pub when_expr: Option<String>,
    /// The trigger function, resolved to (schema, name) at `CREATE TRIGGER`
    /// time (search path applied once, like `ForeignKey.ref_schema`). Trigger
    /// functions always have arity 0.
    pub function_schema: String,
    pub function_name: String,
    /// `ALTER TABLE ... ENABLE/DISABLE TRIGGER` (`pg_trigger.tgenabled`
    /// `'O'`/`'D'`). Defaults to `true` so catalog documents written without
    /// the field deserialize as enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// True for `CREATE CONSTRAINT TRIGGER`. Must be AFTER FOR EACH ROW.
    #[serde(default)]
    pub is_constraint: bool,
    /// Whether the constraint trigger is deferrable.
    #[serde(default)]
    pub deferrable: bool,
    /// Whether the constraint trigger fires at end-of-transaction by default.
    #[serde(default)]
    pub initially_deferred: bool,
    /// REFERENCING OLD TABLE AS <alias> (statement-level only).
    #[serde(default)]
    pub referencing_old: Option<String>,
    /// REFERENCING NEW TABLE AS <alias> (statement-level only).
    #[serde(default)]
    pub referencing_new: Option<String>,
    /// True when attached to a view (timing must be InsteadOf).
    #[serde(default)]
    pub on_view: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "TableDe")]
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
    /// `ALTER TABLE ... ENABLE ROW LEVEL SECURITY`. Defaults to `false` so
    /// catalogs written before this field existed keep loading unchanged.
    #[serde(default)]
    pub rls_enabled: bool,
    /// `ALTER TABLE ... FORCE ROW LEVEL SECURITY`: owner roles lose their
    /// row-security exemption on this table. Same backward-compatible serde
    /// default as `rls_enabled`.
    #[serde(default)]
    pub rls_forced: bool,
    /// Row-security policies (`CREATE POLICY`). Same backward-compatible
    /// serde default as `rls_enabled`.
    #[serde(default)]
    pub policies: Vec<Policy>,
    /// Triggers (`CREATE TRIGGER`). Same backward-compatible serde default as
    /// `rls_enabled`/`policies`: catalogs written before this field existed
    /// keep loading unchanged.
    #[serde(default)]
    pub triggers: Vec<TriggerDef>,
    /// O(1) column lookup by name. Not serialized; rebuilt from `columns` on
    /// construction and after any structural column change.
    #[serde(skip)]
    pub(crate) column_map: HashMap<String, usize>,
}

/// Deserialization helper for [`Table`]. Mirrors all serialized fields; the
/// `column_map` acceleration index is rebuilt via [`From<TableDe>`].
#[derive(Deserialize)]
struct TableDe {
    oid: u32,
    schema: String,
    name: String,
    columns: Vec<Column>,
    primary_key: Option<PrimaryKey>,
    uniques: Vec<UniqueConstraint>,
    foreign_keys: Vec<ForeignKey>,
    checks: Vec<CheckConstraint>,
    storage_collection: String,
    #[serde(default)]
    rls_enabled: bool,
    #[serde(default)]
    rls_forced: bool,
    #[serde(default)]
    policies: Vec<Policy>,
    #[serde(default)]
    triggers: Vec<TriggerDef>,
}

impl From<TableDe> for Table {
    fn from(d: TableDe) -> Self {
        let mut t = Table {
            oid: d.oid,
            schema: d.schema,
            name: d.name,
            columns: d.columns,
            primary_key: d.primary_key,
            uniques: d.uniques,
            foreign_keys: d.foreign_keys,
            checks: d.checks,
            storage_collection: d.storage_collection,
            rls_enabled: d.rls_enabled,
            rls_forced: d.rls_forced,
            policies: d.policies,
            triggers: d.triggers,
            column_map: HashMap::new(),
        };
        t.rebuild_column_map();
        t
    }
}

impl Table {
    /// Rebuild the O(1) `column_map` from `self.columns`.
    ///
    /// Must be called after any structural change to `self.columns` (push,
    /// retain, rename, etc.) to keep the acceleration index in sync.
    pub fn rebuild_column_map(&mut self) {
        self.column_map.clear();
        for (i, c) in self.columns.iter().enumerate() {
            self.column_map.insert(c.name.clone(), i);
        }
    }

    pub fn column(&self, name: &str) -> Option<&Column> {
        self.column_map.get(name).map(|&i| &self.columns[i])
    }

    pub fn policy(&self, name: &str) -> Option<&Policy> {
        self.policies.iter().find(|p| p.name == name)
    }

    pub fn trigger(&self, name: &str) -> Option<&TriggerDef> {
        self.triggers.iter().find(|t| t.name == name)
    }

    pub fn column_mut(&mut self, name: &str) -> Option<&mut Column> {
        // Resolve the index first (immutable borrow ends before the mutable
        // borrow of self.columns begins, satisfying the borrow checker).
        let idx = *self.column_map.get(name)?;
        Some(&mut self.columns[idx])
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.column_map.get(name).copied()
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
    /// Operator class per key column (e.g. `vector_cosine_ops` for `hnsw`
    /// indexes). Empty for methods that take none; `serde(default)` so
    /// catalogs persisted before this field existed keep loading.
    #[serde(default)]
    pub opclasses: Vec<String>,
    /// `WITH (...)` storage parameters (e.g. `m`, `ef_construction`),
    /// normalized to lower-case keys.
    #[serde(default)]
    pub with_options: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sequence {
    pub schema: String,
    pub name: String,
    pub current: i64,
    pub increment: i64,
    pub start: i64,
}

/// A declared auto-embedding rule, persisted in the catalog so it replicates
/// like any other DDL and is read by the async embedding service. This is the
/// catalog-level, dependency-free mirror of `guardian_db::embedding::EmbeddingRule`
/// (the relational core must not depend on the embedding module); the service
/// converts it. Identity is `(schema, table, vector_column)` — a vector column
/// has at most one rule. Declared and dropped through the SQL surface
/// (`guardian_embed(...)` / `guardian_unembed(...)`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingRuleDef {
    pub schema: String,
    pub table: String,
    pub text_column: String,
    pub vector_column: String,
    pub model: String,
    /// Execution policy, lower-case: `"local"` or `"delegated"`.
    pub policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    pub oid: u32,
    pub schema: String,
    pub name: String,
    /// The SQL text of the SELECT defining the view.
    pub query: String,
    pub columns: Vec<String>,
    /// INSTEAD OF triggers attached to this view.
    #[serde(default)]
    pub triggers: Vec<TriggerDef>,
}

impl View {
    pub fn trigger(&self, name: &str) -> Option<&TriggerDef> {
        self.triggers.iter().find(|t| t.name == name)
    }
}

/// The implementation language of a user-defined function body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionLanguage {
    Sql,
    PlPgSql,
}

impl FunctionLanguage {
    /// The `pg_proc.prolang`-ish spelling (GuardianDB has no `pg_language`
    /// OID table, so the language name is stored directly as text).
    pub fn as_sql(&self) -> &'static str {
        match self {
            FunctionLanguage::Sql => "sql",
            FunctionLanguage::PlPgSql => "plpgsql",
        }
    }
}

/// `IMMUTABLE | STABLE | VOLATILE`, stored and reported truthfully but not
/// acted on by the optimizer (GuardianDB has no plan cache keyed on
/// volatility) — see `docs/postgres-compat.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FunctionVolatility {
    Immutable,
    Stable,
    Volatile,
}

impl FunctionVolatility {
    /// The `pg_proc.provolatile` character (`i`/`s`/`v`).
    pub fn as_char(&self) -> char {
        match self {
            FunctionVolatility::Immutable => 'i',
            FunctionVolatility::Stable => 's',
            FunctionVolatility::Volatile => 'v',
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionArgDef {
    /// Declared parameter name (PL/pgSQL binds by this; SQL-language bodies
    /// use `$1`/`$2` regardless). Synthesized as `$N` for unnamed arguments.
    pub name: String,
    pub ty: SqlType,
}

/// A user-defined function (`CREATE FUNCTION`). Bodies are stored as raw
/// source text (`prosrc`) and re-parsed on use, the same pattern
/// [`Policy`]'s `using_expr`/`check_expr` already follows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub oid: u32,
    pub schema: String,
    pub name: String,
    pub args: Vec<FunctionArgDef>,
    pub return_type: SqlType,
    pub language: FunctionLanguage,
    pub volatility: FunctionVolatility,
    /// `STRICT` / `RETURNS NULL ON NULL INPUT`: any `NULL` argument short-circuits
    /// to a `NULL` result without invoking the body.
    pub strict: bool,
    /// Raw body text between `AS $$ ... $$` (`prosrc`).
    pub body: String,
    /// `RETURNS trigger` (PL/pgSQL only): callable exclusively through a
    /// trigger firing, never as a scalar function. `return_type` stays
    /// [`SqlType::Unknown`] (there is deliberately no `SqlType::Trigger`
    /// variant — that would make `CREATE TABLE t (x trigger)` representable);
    /// `pg_proc.prorettype` special-cases this flag to OID 2279 (PostgreSQL's
    /// `trigger` pseudo-type). Defaults to `false` so catalogs written before
    /// this field existed keep loading unchanged.
    #[serde(default)]
    pub returns_trigger: bool,
}

impl FunctionDef {
    pub fn arity(&self) -> usize {
        self.args.len()
    }

    pub fn qualified(&self) -> QualifiedName {
        QualifiedName::new(self.schema.clone(), self.name.clone())
    }
}

/// Outcome of resolving an unqualified `DROP FUNCTION name` (no argument
/// list) against the catalog.
pub enum DropFunctionByName {
    Removed,
    NotFound,
    /// More than one signature shares the name; PostgreSQL requires the
    /// argument list to disambiguate (SQLSTATE 42725).
    Ambiguous,
}

/// A user-defined text search dictionary (currently: synonym type only).
/// Persisted in the catalog so synonym expansions survive restarts and
/// replicate to peers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TsDictionaryDef {
    pub name: String,
    pub schema: String,
    pub oid: u32,
    /// Inline synonym map: word (lowercased) -> list of synonym strings.
    /// Built from the `SYNONYMS = 'word:syn1,syn2;word2:syn3'` option at
    /// CREATE TIME.
    pub synonyms: BTreeMap<String, Vec<String>>,
    /// Thesaurus entries: phrase (space-separated, lowercased) → canonical terms.
    /// Populated when `TEMPLATE = thesaurus`. Empty for synonym dicts.
    #[serde(default)]
    pub thesaurus_entries: BTreeMap<String, Vec<String>>,
}

/// The authoritative, serializable relational catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "CatalogRaw")]
pub struct Catalog {
    pub database: String,
    schemas: BTreeMap<String, Schema>,
    tables: BTreeMap<QualifiedName, Table>,
    indexes: BTreeMap<QualifiedName, Index>,
    sequences: BTreeMap<QualifiedName, Sequence>,
    views: BTreeMap<QualifiedName, View>,
    next_oid: u32,
    pub search_path: Vec<String>,
    /// Installed extensions: name -> installed version. Persisted with the
    /// catalog document, so installs replicate like any other DDL.
    #[serde(default)]
    extensions: BTreeMap<String, String>,
    /// User-defined functions (`CREATE FUNCTION`). Defaults to empty so
    /// catalogs written before this field existed keep loading unchanged,
    /// the same backward-compatible pattern as `rls_enabled`.
    #[serde(default)]
    functions: Vec<FunctionDef>,
    /// User-defined text search dictionaries (`CREATE TEXT SEARCH DICTIONARY`).
    /// Serde-defaulted so catalogs predating this field keep loading.
    #[serde(default)]
    ts_dictionaries: Vec<TsDictionaryDef>,
    /// Declared auto-embedding rules (RFC 0005 §6.3), read by the embedding
    /// service. Serde-defaulted for backward compatibility.
    #[serde(default)]
    embedding_rules: Vec<EmbeddingRuleDef>,
    /// Reverse map: (schema, table) -> list of index QualifiedNames for O(1)
    /// `indexes_for_table` lookup. Not persisted — rebuilt on deserialization
    /// and maintained by `insert_index` / `drop_index`.
    #[serde(skip)]
    table_to_indexes: HashMap<(String, String), Vec<QualifiedName>>,
}

/// Deserialization shim for [`Catalog`]. Mirrors all persisted fields;
/// `table_to_indexes` is not stored — it is rebuilt via [`From<CatalogRaw>`].
#[derive(Deserialize)]
struct CatalogRaw {
    database: String,
    schemas: BTreeMap<String, Schema>,
    tables: BTreeMap<QualifiedName, Table>,
    indexes: BTreeMap<QualifiedName, Index>,
    sequences: BTreeMap<QualifiedName, Sequence>,
    views: BTreeMap<QualifiedName, View>,
    next_oid: u32,
    search_path: Vec<String>,
    #[serde(default)]
    extensions: BTreeMap<String, String>,
    #[serde(default)]
    functions: Vec<FunctionDef>,
    #[serde(default)]
    ts_dictionaries: Vec<TsDictionaryDef>,
    #[serde(default)]
    embedding_rules: Vec<EmbeddingRuleDef>,
}

impl From<CatalogRaw> for Catalog {
    fn from(raw: CatalogRaw) -> Self {
        let mut table_to_indexes: HashMap<(String, String), Vec<QualifiedName>> = HashMap::new();
        for (q, idx) in &raw.indexes {
            table_to_indexes
                .entry((idx.schema.clone(), idx.table.clone()))
                .or_default()
                .push(q.clone());
        }
        Catalog {
            database: raw.database,
            schemas: raw.schemas,
            tables: raw.tables,
            indexes: raw.indexes,
            sequences: raw.sequences,
            views: raw.views,
            next_oid: raw.next_oid,
            search_path: raw.search_path,
            extensions: raw.extensions,
            functions: raw.functions,
            ts_dictionaries: raw.ts_dictionaries,
            embedding_rules: raw.embedding_rules,
            table_to_indexes,
        }
    }
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
            extensions: BTreeMap::new(),
            functions: Vec::new(),
            ts_dictionaries: Vec::new(),
            embedding_rules: Vec::new(),
            table_to_indexes: HashMap::new(),
        };
        // PostgreSQL databases have plpgsql installed by default.
        catalog
            .extensions
            .insert("plpgsql".to_string(), "1.0".to_string());
        // System schemas always present.
        for sys in ["pg_catalog", "information_schema"] {
            let oid = catalog.allocate_oid();
            catalog.schemas.insert(
                sys.to_string(),
                Schema {
                    name: sys.to_string(),
                    oid,
                    owner: "guardian".into(),
                },
            );
        }
        let oid = catalog.allocate_oid();
        catalog.schemas.insert(
            "public".to_string(),
            Schema {
                name: "public".into(),
                oid,
                owner: "guardian".into(),
            },
        );
        catalog
    }

    /// Installed extensions as (name, version) pairs, sorted by name.
    pub fn extensions(&self) -> impl Iterator<Item = (&str, &str)> {
        self.extensions
            .iter()
            .map(|(k, v)| (k.as_str(), strip_sidecar_marker(v)))
    }

    /// The installed version of `name`, if installed.
    pub fn extension_version(&self, name: &str) -> Option<&str> {
        self.extensions.get(name).map(|v| strip_sidecar_marker(v))
    }

    pub fn extension_installed(&self, name: &str) -> bool {
        self.extensions.contains_key(name)
    }

    /// Whether `name` is installed *and* bound to the PostgreSQL sidecar
    /// runtime (see [`Catalog::install_sidecar_extension`]).
    pub fn extension_is_sidecar(&self, name: &str) -> bool {
        self.extensions
            .get(name)
            .map(|v| v.ends_with(SIDECAR_MARKER))
            .unwrap_or(false)
    }

    /// Record an extension as installed. Returns `false` if it already was.
    pub fn install_extension(&mut self, name: &str, version: &str) -> bool {
        self.extensions
            .insert(name.to_string(), version.to_string())
            .is_none()
    }

    /// Record an extension as installed on the PostgreSQL sidecar runtime.
    /// The binding is encoded as a `@sidecar` suffix inside the stored version
    /// string, so old catalog documents (plain version strings) keep loading
    /// unchanged. Returns `false` if the extension was already installed.
    pub fn install_sidecar_extension(&mut self, name: &str, version: &str) -> bool {
        self.extensions
            .insert(name.to_string(), format!("{version}{SIDECAR_MARKER}"))
            .is_none()
    }

    /// Remove an installed extension. Returns `false` if it was not installed.
    pub fn uninstall_extension(&mut self, name: &str) -> bool {
        self.extensions.remove(name).is_some()
    }

    /// Update the stored version of an installed extension (`ALTER EXTENSION
    /// ... UPDATE`), preserving a sidecar binding. Returns `false` if the
    /// extension is not installed.
    pub fn set_extension_version(&mut self, name: &str, version: &str) -> bool {
        match self.extensions.get_mut(name) {
            Some(v) => {
                *v = if v.ends_with(SIDECAR_MARKER) {
                    format!("{version}{SIDECAR_MARKER}")
                } else {
                    version.to_string()
                };
                true
            }
            None => false,
        }
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
        self.schemas.insert(
            name.to_string(),
            Schema {
                name: name.to_string(),
                oid,
                owner: "guardian".into(),
            },
        );
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
        table.rebuild_column_map();
        self.tables.insert(q, table);
        Ok(())
    }

    /// Foreign keys on *other* tables whose referenced table is `q` (the
    /// constraint's declaring table plus the constraint itself).
    pub fn referencing_foreign_keys(&self, q: &QualifiedName) -> Vec<(QualifiedName, ForeignKey)> {
        let mut out = Vec::new();
        for table in self.tables.values() {
            for fk in &table.foreign_keys {
                if fk.ref_schema == q.schema && fk.ref_table == q.name {
                    out.push((table.qualified(), fk.clone()));
                }
            }
        }
        out
    }

    /// Foreign keys named `name`, for `SET CONSTRAINTS` (which — unlike
    /// `ALTER TABLE ... DROP CONSTRAINT` — resolves a constraint by name
    /// alone, with no owning table given). Mirrors PostgreSQL's own
    /// resolution (`AfterTriggerSetState` in
    /// `src/backend/commands/trigger.c`): an explicit schema restricts the
    /// search to it; a bare name searches the schema search path and returns
    /// every match in the *first* schema that has any. PostgreSQL allows more
    /// than one constraint to share a name within a schema (e.g. on
    /// different tables) and toggles them together — so does this.
    pub fn foreign_keys_named(
        &self,
        schema: Option<&str>,
        name: &str,
    ) -> Vec<(QualifiedName, ForeignKey)> {
        let schemas: Vec<&str> = match schema {
            Some(s) => vec![s],
            None => self.search_path.iter().map(String::as_str).collect(),
        };
        for s in schemas {
            let found: Vec<(QualifiedName, ForeignKey)> = self
                .tables
                .values()
                .filter(|t| t.schema == s)
                .flat_map(|t| {
                    t.foreign_keys
                        .iter()
                        .filter(|fk| fk.name == name)
                        .map(move |fk| (t.qualified(), fk.clone()))
                })
                .collect();
            if !found.is_empty() {
                return found;
            }
        }
        Vec::new()
    }

    pub fn drop_table_qualified(&mut self, q: &QualifiedName) -> Result<Table> {
        let table = self
            .tables
            .remove(q)
            .ok_or_else(|| RelError::UndefinedTable(q.to_string_qualified()))?;
        // Foreign keys on other tables referencing the dropped table go with it
        // (DROP ... CASCADE drops the dependent constraint in PostgreSQL; the
        // executor guards the non-CASCADE path with 2BP01 before calling this).
        for t in self.tables.values_mut() {
            t.foreign_keys
                .retain(|fk| !(fk.ref_schema == q.schema && fk.ref_table == q.name));
        }
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
        self.table_to_indexes
            .remove(&(q.schema.clone(), q.name.clone()));
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
        let key = (schema.to_string(), table.to_string());
        self.table_to_indexes
            .get(&key)
            .map(|names| names.iter().filter_map(|q| self.indexes.get(q)).collect())
            .unwrap_or_default()
    }

    pub fn get_index(&self, q: &QualifiedName) -> Option<&Index> {
        self.indexes.get(q)
    }

    pub fn insert_index(&mut self, index: Index) -> Result<()> {
        let q = QualifiedName::new(index.schema.clone(), index.name.clone());
        if self.indexes.contains_key(&q) {
            return Err(RelError::DuplicateIndex(q.to_string_qualified()));
        }
        self.table_to_indexes
            .entry((index.schema.clone(), index.table.clone()))
            .or_default()
            .push(q.clone());
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
        if let Some(removed) = self.indexes.remove(&q) {
            let tkey = (removed.schema.clone(), removed.table.clone());
            if let Some(v) = self.table_to_indexes.get_mut(&tkey) {
                v.retain(|iq| iq != &q);
                if v.is_empty() {
                    self.table_to_indexes.remove(&tkey);
                }
            }
        } else if !if_exists {
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
        if let Some(seq) = self.sequences.get_mut(&q)
            && value > seq.current
        {
            seq.current = value;
        }
    }

    // ---- views ---------------------------------------------------------

    pub fn views(&self) -> impl Iterator<Item = &View> {
        self.views.values()
    }

    pub fn get_view(&self, q: &QualifiedName) -> Option<&View> {
        self.views.get(q)
    }

    pub fn get_view_mut(&mut self, q: &QualifiedName) -> Option<&mut View> {
        self.views.get_mut(q)
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

    // ---- functions -------------------------------------------------------
    //
    // Signatures are matched by (schema, name, arity) — arity only, not
    // per-argument types. PostgreSQL allows overloading two functions with
    // the same name and arity but different argument types; GuardianDB's
    // call dispatch resolves by name+arity alone (see `funcs::call_scalar`),
    // so two `CREATE FUNCTION`s with the same name+arity are treated as the
    // same signature regardless of declared argument types — a deliberate,
    // documented simplification (see `docs/postgres-compat.md`).

    pub fn functions(&self) -> impl Iterator<Item = &FunctionDef> {
        self.functions.iter()
    }

    fn resolve_function_index(
        &self,
        schema: Option<&str>,
        name: &str,
        arity: usize,
    ) -> Option<usize> {
        if let Some(s) = schema {
            return self
                .functions
                .iter()
                .position(|f| f.schema == s && f.name == name && f.arity() == arity);
        }
        for s in &self.search_path {
            if let Some(i) = self
                .functions
                .iter()
                .position(|f| &f.schema == s && f.name == name && f.arity() == arity)
            {
                return Some(i);
            }
        }
        None
    }

    /// Resolve `name(arity args)` via `schema` (or the search path when
    /// `None`), the same rule [`Catalog::resolve_table_name`] uses for tables.
    pub fn find_function(
        &self,
        schema: Option<&str>,
        name: &str,
        arity: usize,
    ) -> Option<&FunctionDef> {
        self.resolve_function_index(schema, name, arity)
            .map(|i| &self.functions[i])
    }

    /// `CREATE FUNCTION` (without `OR REPLACE`): errors with `42723` if a
    /// same-schema, same-name, same-arity function already exists.
    pub fn insert_function(&mut self, def: FunctionDef) -> Result<()> {
        if self
            .functions
            .iter()
            .any(|f| f.schema == def.schema && f.name == def.name && f.arity() == def.arity())
        {
            return Err(RelError::DuplicateFunction(format!(
                "function \"{}\" already exists with same argument count",
                def.name
            )));
        }
        self.functions.push(def);
        Ok(())
    }

    /// `CREATE OR REPLACE FUNCTION`: overwrites the same-signature function
    /// in place (preserving its OID), or inserts a new one.
    pub fn replace_function(&mut self, mut def: FunctionDef) {
        match self
            .functions
            .iter()
            .position(|f| f.schema == def.schema && f.name == def.name && f.arity() == def.arity())
        {
            Some(i) => {
                def.oid = self.functions[i].oid;
                self.functions[i] = def;
            }
            None => self.functions.push(def),
        }
    }

    /// `DROP FUNCTION name(arg types)`. Returns `false` if no such signature
    /// exists (the caller decides whether that is an error, i.e. `IF EXISTS`).
    pub fn drop_function(&mut self, schema: Option<&str>, name: &str, arity: usize) -> bool {
        match self.resolve_function_index(schema, name, arity) {
            Some(i) => {
                self.functions.remove(i);
                true
            }
            None => false,
        }
    }

    /// `DROP FUNCTION name` with no argument list: only valid when the name
    /// is not overloaded.
    pub fn drop_function_by_name(
        &mut self,
        schema: Option<&str>,
        name: &str,
    ) -> DropFunctionByName {
        let matches: Vec<usize> = self
            .functions
            .iter()
            .enumerate()
            .filter(|(_, f)| f.name == name && schema.map(|s| f.schema == s).unwrap_or(true))
            .map(|(i, _)| i)
            .collect();
        match matches.len() {
            0 => DropFunctionByName::NotFound,
            1 => {
                self.functions.remove(matches[0]);
                DropFunctionByName::Removed
            }
            _ => DropFunctionByName::Ambiguous,
        }
    }

    // ---- text search dictionaries --------------------------------------

    /// All registered text search dictionaries.
    pub fn ts_dictionaries(&self) -> impl Iterator<Item = &TsDictionaryDef> {
        self.ts_dictionaries.iter()
    }

    /// Declared auto-embedding rules (RFC 0005 §6.3).
    pub fn embedding_rules(&self) -> &[EmbeddingRuleDef] {
        &self.embedding_rules
    }

    /// Declare (or replace) an embedding rule. Identity is
    /// `(schema, table, vector_column)`: re-declaring the same vector column
    /// replaces the rule (last write wins), like a `CREATE OR REPLACE`.
    pub fn put_embedding_rule(&mut self, rule: EmbeddingRuleDef) {
        self.embedding_rules.retain(|r| {
            !(r.schema == rule.schema
                && r.table == rule.table
                && r.vector_column == rule.vector_column)
        });
        self.embedding_rules.push(rule);
    }

    /// Drop the rule for `(schema, table, vector_column)`. Returns whether one
    /// was removed.
    pub fn drop_embedding_rule(&mut self, schema: &str, table: &str, vector_column: &str) -> bool {
        let before = self.embedding_rules.len();
        self.embedding_rules.retain(|r| {
            !(r.schema == schema && r.table == table && r.vector_column == vector_column)
        });
        self.embedding_rules.len() != before
    }

    /// `CREATE TEXT SEARCH DICTIONARY`. Returns `Ok(())` on `IF NOT EXISTS`
    /// duplicates. Errors with `42710` on plain duplicates.
    pub fn insert_ts_dictionary(
        &mut self,
        def: TsDictionaryDef,
        if_not_exists: bool,
    ) -> Result<()> {
        if self
            .ts_dictionaries
            .iter()
            .any(|d| d.schema == def.schema && d.name == def.name)
        {
            if if_not_exists {
                return Ok(());
            }
            return Err(RelError::DuplicateObject(format!(
                "text search dictionary \"{}.{}\"",
                def.schema, def.name
            )));
        }
        self.ts_dictionaries.push(def);
        Ok(())
    }

    /// `DROP TEXT SEARCH DICTIONARY`. Returns `true` if removed.
    pub fn drop_ts_dictionary(
        &mut self,
        schema: Option<&str>,
        name: &str,
        if_exists: bool,
    ) -> Result<bool> {
        let pos = self
            .ts_dictionaries
            .iter()
            .position(|d| d.name == name && schema.map(|s| d.schema == s).unwrap_or(true));
        match pos {
            Some(i) => {
                self.ts_dictionaries.remove(i);
                Ok(true)
            }
            None => {
                if if_exists {
                    Ok(false)
                } else {
                    Err(RelError::UndefinedObject(format!(
                        "text search dictionary \"{name}\""
                    )))
                }
            }
        }
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
                Column {
                    name: "id".into(),
                    ty: SqlType::Integer,
                    nullable: false,
                    default: None,
                    identity_sequence: None,
                    ordinal: 0,
                },
                Column {
                    name: "email".into(),
                    ty: SqlType::Text,
                    nullable: false,
                    default: None,
                    identity_sequence: None,
                    ordinal: 1,
                },
            ],
            primary_key: Some(PrimaryKey {
                name: "users_pkey".into(),
                columns: vec!["id".into()],
            }),
            uniques: vec![],
            foreign_keys: vec![],
            checks: vec![],
            storage_collection: String::new(),
            rls_enabled: false,
            rls_forced: false,
            policies: vec![],
            triggers: vec![],
            column_map: HashMap::new(),
        }
    }

    #[test]
    fn create_and_resolve_table() {
        let mut cat = Catalog::new("app");
        let t = sample_table(&mut cat);
        cat.insert_table(t).unwrap();
        let q = cat.resolve_table_name(None, "users").unwrap();
        assert_eq!(q.schema, "public");
        assert!(
            cat.get_table(&q)
                .unwrap()
                .storage_collection
                .starts_with("__gdb_sql_rows_")
        );
    }

    #[test]
    fn duplicate_table_errors() {
        let mut cat = Catalog::new("app");
        let t = sample_table(&mut cat);
        cat.insert_table(t.clone()).unwrap();
        let t2 = sample_table(&mut cat);
        assert!(matches!(
            cat.insert_table(t2),
            Err(RelError::DuplicateTable(_))
        ));
    }

    #[test]
    fn sequence_advances() {
        let mut cat = Catalog::new("app");
        cat.create_sequence("public", "users_id_seq").unwrap();
        assert_eq!(
            cat.next_sequence_value("public", "users_id_seq").unwrap(),
            1
        );
        assert_eq!(
            cat.next_sequence_value("public", "users_id_seq").unwrap(),
            2
        );
        cat.observe_sequence_value("public", "users_id_seq", 10);
        assert_eq!(
            cat.next_sequence_value("public", "users_id_seq").unwrap(),
            11
        );
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
    fn sidecar_extension_marker_round_trips() {
        let mut cat = Catalog::new("app");
        cat.install_sidecar_extension("pg_stat_statements", "1.10");
        assert!(cat.extension_installed("pg_stat_statements"));
        assert!(cat.extension_is_sidecar("pg_stat_statements"));
        // Readers never see the marker.
        assert_eq!(cat.extension_version("pg_stat_statements"), Some("1.10"));
        assert!(
            cat.extensions()
                .any(|(n, v)| n == "pg_stat_statements" && v == "1.10")
        );
        // Version updates preserve the binding; native installs never carry it.
        cat.set_extension_version("pg_stat_statements", "1.11");
        assert!(cat.extension_is_sidecar("pg_stat_statements"));
        assert_eq!(cat.extension_version("pg_stat_statements"), Some("1.11"));
        assert!(!cat.extension_is_sidecar("plpgsql"));
        // The serialized shape is still a plain name -> string map.
        let json = serde_json::to_value(&cat).unwrap();
        let back: Catalog = serde_json::from_value(json).unwrap();
        assert!(back.extension_is_sidecar("pg_stat_statements"));
        assert_eq!(back.extension_version("plpgsql"), Some("1.0"));
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

// Maintenance note 9: documents compatibility expectations without changing runtime behavior.

// Maintenance note 21: documents compatibility expectations without changing runtime behavior.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// SQL compatibility note 14: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 14: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.
