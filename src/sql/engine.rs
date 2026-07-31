//! The database engine: sessions, transactions, statement dispatch, and the
//! load → execute → commit lifecycle.

use crate::relational::catalog::QualifiedName;
use crate::relational::{Catalog, RelationalStorage, SqlValue};
use crate::sql::error::{Result, SqlError};
use crate::sql::exec::{ConstraintModes, DeferredFkCheck, Exec};
use crate::sql::lock::{LockManager, LockMode, LockObject, LockScope, SessionId, WaitPolicy};
use crate::sql::result::ExecResult;
use crate::sql::store::{LoadedTable, Mutation};
use crate::sql::trigger::DeferredTriggerFiring;
use serde_json::Value as Json;
use sqlparser::ast::{Query, Statement};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

/// A shared, storage-backed relational database.
pub struct Database<S: RelationalStorage> {
    storage: Arc<S>,
    pub name: String,
    locks: Arc<LockManager>,
    /// Registered row-change listeners (see [`Database::subscribe_changes`]).
    /// Closed receivers are pruned lazily on the next emission.
    change_listeners: std::sync::RwLock<Vec<tokio::sync::mpsc::UnboundedSender<ChangeEvent>>>,
    /// Cross-statement ANN index runtime (RFC 0005): HNSW graphs are too
    /// expensive to rebuild per statement, so they live here and are
    /// reconciled against each statement's freshly loaded table view.
    #[cfg(feature = "vector-index")]
    ann: Arc<crate::sql::ann::AnnRuntime>,
    /// Decoded-table cache: the engine's local-first model reloads and
    /// decodes a whole table view per statement. When the backend reports a
    /// per-collection change counter ([`RelationalStorage::generation`]),
    /// the decoded [`LoadedTable`] is cached here keyed by that counter, so a
    /// statement that finds the counter unchanged skips the
    /// rescan+decode+index-rebuild entirely and shares the cached view by
    /// `Arc` (zero copy for reads). Bypassed inside a transaction whose
    /// overlay touches the collection (those need the staged writes merged in).
    table_cache: std::sync::RwLock<HashMap<String, CachedTable>>,
}

/// One cached decoded table view, tagged with the storage change counter it
/// was built at (see [`Database::table_cache`]).
struct CachedTable {
    generation: u64,
    table: Arc<LoadedTable>,
}

impl<S: RelationalStorage> Database<S> {
    pub fn new(storage: Arc<S>, name: impl Into<String>) -> Self {
        Self {
            storage,
            name: name.into(),
            locks: Arc::new(LockManager::new()),
            change_listeners: std::sync::RwLock::new(Vec::new()),
            #[cfg(feature = "vector-index")]
            ann: Arc::new(crate::sql::ann::AnnRuntime::new()),
            table_cache: std::sync::RwLock::new(HashMap::new()),
        }
    }

    pub fn storage(&self) -> &Arc<S> {
        &self.storage
    }

    /// Load a fresh snapshot of the persisted catalog (schemas, tables,
    /// columns, indexes). Used by out-of-band consumers such as the
    /// auto-embedding service ([`crate::embedding`]) that need column types
    /// and storage-collection names without opening a full statement.
    pub async fn catalog_snapshot(&self) -> Result<Catalog> {
        match self.storage.load_catalog().await? {
            Some(json) => serde_json::from_value(json)
                .map_err(|e| SqlError::Storage(format!("corrupt catalog: {e}"))),
            None => Ok(Catalog::new(&self.name)),
        }
    }

    /// Merge column updates into a stored row and persist it, bumping the
    /// row's version. Column values are the engine's JSON encoding (a
    /// `vector` is a JSON array of numbers, text is a JSON string).
    ///
    /// This is the auto-embedding service's write-back path
    /// ([`crate::embedding`]). It deliberately goes straight to storage
    /// rather than through a SQL `UPDATE`, for two reasons:
    /// - a direct storage write emits **no** [`ChangeEvent`], which is what
    ///   structurally breaks the embed → write-back → embed loop (the
    ///   source-hash sidecar is the second line of defence, not the only one);
    /// - it needs neither the row's primary key nor a constructed SQL
    ///   statement — it addresses the row by its stored id.
    ///
    /// The row is re-read here (not taken from a possibly-stale change event)
    /// so the merge lands on the *current* document and only the named columns
    /// are touched — a concurrent change to the source text is preserved (and
    /// the next change event re-embeds it).
    ///
    /// **Concurrency semantics.** The `get` + `put` are not one atomic
    /// operation, so a write that commits to the same row *between* them is
    /// resolved last-writer-wins on the whole document — exactly like the
    /// engine's own autocommit `UPDATE` (which likewise scans, modifies and
    /// writes the whole row without holding a cross-statement lock) and like
    /// GuardianDB's LWW-CRDT model for concurrent document writes. This is not
    /// a stronger guarantee than a normal write; callers needing atomicity
    /// against a specific concurrent writer must serialize at a higher level.
    ///
    /// Returns `Ok(())` with no write if the row no longer exists (deleted
    /// since the change). The generation bump from the underlying `put`
    /// invalidates the decoded-table cache.
    pub async fn patch_row(
        &self,
        collection: &str,
        row_id: &str,
        updates: &[(String, Json)],
    ) -> Result<()> {
        let Some(existing) = self.storage.get(collection, row_id).await? else {
            return Ok(());
        };
        let Json::Object(mut obj) = existing else {
            return Err(SqlError::Storage("row document is not an object".into()));
        };
        for (col, val) in updates {
            obj.insert(col.clone(), val.clone());
        }
        let v = obj
            .get(crate::sql::store::F_VERSION)
            .and_then(Json::as_i64)
            .unwrap_or(1)
            + 1;
        obj.insert(crate::sql::store::F_VERSION.to_string(), Json::from(v));
        self.storage
            .put(collection, row_id, &Json::Object(obj))
            .await
    }

    /// Invalidate the entire decoded-table cache. Called when a catalog
    /// change commits: a `LoadedTable` embeds its table's column set and
    /// secondary-index definitions, which the storage change counter does not
    /// track, so a `CREATE INDEX` / `ALTER TABLE` that leaves rows untouched
    /// must still force a rebuild. DDL is rare, so clearing the whole map
    /// (rather than tracking which tables a statement altered) is the robust,
    /// simple choice.
    pub(crate) fn invalidate_table_cache(&self) {
        self.table_cache.write().unwrap().clear();
    }

    /// Fetch a table view for `collection` at storage change-counter
    /// `generation`, decoding it only on a cache miss (see
    /// [`Database::table_cache`]). The `build` closure turns the scanned
    /// documents into a [`LoadedTable`]; it runs only on a miss.
    async fn cached_table(
        &self,
        collection: &str,
        generation: u64,
        build: impl FnOnce(Vec<(String, Json)>) -> Result<LoadedTable>,
    ) -> Result<Arc<LoadedTable>> {
        // Fast path: a matching cached generation is a zero-copy Arc clone.
        {
            let cache = self.table_cache.read().unwrap();
            if let Some(c) = cache.get(collection)
                && c.generation == generation
            {
                return Ok(c.table.clone());
            }
        }
        // Miss: scan + decode without holding the cache lock.
        let docs = self.storage.scan(collection).await?;
        let built = Arc::new(build(docs)?);
        let mut cache = self.table_cache.write().unwrap();
        // A concurrent builder may have installed an equal-or-newer view
        // while we scanned; keep the freshest rather than regress it.
        match cache.get(collection) {
            Some(c) if c.generation >= generation => Ok(c.table.clone()),
            _ => {
                cache.insert(
                    collection.to_string(),
                    CachedTable {
                        generation,
                        table: built.clone(),
                    },
                );
                Ok(built)
            }
        }
    }

    /// Enable persistent ANN index snapshots (RFC 0005 §6.1) under
    /// `dir/<database-name>/`. Without this, ANN indexes rebuild from rows on
    /// first use each process — always correct, only slower to start.
    #[cfg(feature = "vector-index")]
    pub fn set_ann_snapshot_dir(&self, dir: impl Into<std::path::PathBuf>) {
        self.ann.set_snapshot_dir(Some(dir.into().join(&self.name)));
    }

    /// The shared ANN runtime (RFC 0005).
    #[cfg(feature = "vector-index")]
    pub fn ann(&self) -> &Arc<crate::sql::ann::AnnRuntime> {
        &self.ann
    }

    /// The shared lock manager (single-node coordinator).
    pub fn locks(&self) -> &Arc<LockManager> {
        &self.locks
    }

    /// Subscribe to committed row changes. Every row mutation that reaches
    /// storage through a [`Session`] — autocommit statements and explicit
    /// `COMMIT`s alike — is delivered as a [`ChangeEvent`] *after* it has been
    /// applied. Dropping the receiver unsubscribes (the sender is pruned on the
    /// next emission). When no listener is registered the engine skips event
    /// collection entirely, so the hook costs nothing unless used.
    ///
    /// `TRUNCATE` produces no per-row events, and writes that bypass the
    /// engine (direct [`RelationalStorage`] calls, remote replication) are not
    /// observed — this is a local-commit hook, not a replication changefeed.
    pub fn subscribe_changes(&self) -> tokio::sync::mpsc::UnboundedReceiver<ChangeEvent> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        self.change_listeners.write().unwrap().push(tx);
        rx
    }

    /// Is at least one change listener registered?
    fn has_change_listeners(&self) -> bool {
        !self.change_listeners.read().unwrap().is_empty()
    }

    /// Deliver `events` to every registered listener, pruning closed ones.
    fn emit_changes(&self, events: Vec<ChangeEvent>) {
        if events.is_empty() {
            return;
        }
        let mut listeners = self.change_listeners.write().unwrap();
        if listeners.is_empty() {
            return;
        }
        listeners.retain(|tx| events.iter().all(|e| tx.send(e.clone()).is_ok()));
    }
}

/// A committed row change, delivered to [`Database::subscribe_changes`]
/// receivers after the write reached storage. `old`/`new` carry the stored row
/// documents (the engine's JSON row encoding, including the `__schema` /
/// `__table` metadata fields); consumers decode column values with the catalog
/// column types.
#[derive(Clone, Debug)]
pub struct ChangeEvent {
    pub schema: String,
    pub table: String,
    pub op: ChangeOp,
    /// The row document before the change (`UPDATE` / `DELETE`).
    pub old: Option<Json>,
    /// The row document after the change (`INSERT` / `UPDATE`).
    pub new: Option<Json>,
    /// When the local commit applied this change.
    pub commit_time: chrono::DateTime<chrono::Utc>,
}

impl ChangeEvent {
    /// The stored row id (`_id`), taken from the post-image for
    /// insert/update and the pre-image for delete. `None` only if neither
    /// image is present or lacks the field (never, for engine-produced
    /// events).
    pub fn row_id(&self) -> Option<&str> {
        self.new
            .as_ref()
            .or(self.old.as_ref())
            .and_then(|d| d.get(crate::sql::store::F_ID))
            .and_then(Json::as_str)
    }

    /// A column's raw stored JSON value from the post-image (the value after
    /// this change). `None` if there is no post-image (delete) or the column
    /// is absent.
    pub fn new_field(&self, column: &str) -> Option<&Json> {
        self.new.as_ref().and_then(|d| d.get(column))
    }
}

/// The kind of row change a [`ChangeEvent`] describes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeOp {
    Insert,
    Update,
    Delete,
}

impl ChangeOp {
    /// The PostgreSQL logical-replication spelling (`INSERT`/`UPDATE`/`DELETE`).
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeOp::Insert => "INSERT",
            ChangeOp::Update => "UPDATE",
            ChangeOp::Delete => "DELETE",
        }
    }
}

/// An in-flight explicit transaction (BEGIN ... COMMIT/ROLLBACK).
struct Transaction {
    catalog: Catalog,
    catalog_dirty: bool,
    /// collection -> row_id -> Some(doc) (upsert) / None (delete)
    overlay: HashMap<String, HashMap<String, Option<Json>>>,
    truncated: HashSet<String>,
    /// Set when a statement errors inside the block (PostgreSQL aborts the txn).
    aborted: bool,
    /// This transaction's `SET CONSTRAINTS` state (see
    /// [`ConstraintModes`]), read by every statement's [`Exec`] to decide
    /// whether a deferrable foreign key currently checks immediately or
    /// defers to `COMMIT`.
    constraint_modes: ConstraintModes,
    /// Foreign-key checks postponed by a `DEFERRABLE` constraint currently
    /// running in `DEFERRED` mode (see [`DeferredFkCheck`]), accumulated
    /// across every statement in this transaction so far. Drained and
    /// re-validated at `COMMIT` (`Session::commit`) or by `SET CONSTRAINTS
    /// ... IMMEDIATE` (`Session::exec_set_constraints`). Discarded along
    /// with the rest of `Transaction` on `ROLLBACK` — there is no separate
    /// cleanup step for it.
    pending_deferred: Vec<DeferredFkCheck>,
    /// CONSTRAINT TRIGGER firings deferred to `COMMIT` (see
    /// [`DeferredTriggerFiring`]), accumulated across every statement in this
    /// transaction. Fired at `COMMIT` before writing to storage; discarded on
    /// `ROLLBACK`.
    deferred_triggers: Vec<DeferredTriggerFiring>,
}

/// A connection-scoped session.
pub struct Session<S: RelationalStorage> {
    db: Arc<Database<S>>,
    username: String,
    txn: Option<Transaction>,
    session_id: SessionId,
    lock_timeout: Option<Duration>,
    /// Session variables (`SET name = value`), including extension GUCs.
    vars: HashMap<String, String>,
    /// Lazily pinned connection to the PostgreSQL sidecar runtime (closed
    /// when the session drops, ending the backend session with it).
    sidecar: Option<crate::sql::ext::sidecar::SidecarConn>,
}

impl<S: RelationalStorage> Drop for Session<S> {
    fn drop(&mut self) {
        // Release any locks still held (e.g. session-level advisory locks) when
        // the connection goes away.
        self.db.locks.release_session(self.session_id);
    }
}

/// A parsed, reusable prepared statement.
#[derive(Clone)]
pub struct Prepared {
    pub sql: String,
    pub statement: Statement,
    pub param_count: usize,
}

impl<S: RelationalStorage> Session<S> {
    pub fn new(db: Arc<Database<S>>, username: impl Into<String>) -> Self {
        let session_id = db.locks.new_session();
        Self {
            db,
            username: username.into(),
            txn: None,
            session_id,
            lock_timeout: None,
            vars: HashMap::new(),
            sidecar: None,
        }
    }

    pub fn in_transaction(&self) -> bool {
        self.txn.is_some()
    }

    /// Set a session variable directly (equivalent to `SET name = value`, no
    /// SQL round-trip). Used by the Supabase gateway to inject the request's
    /// JWT claims (`request.jwt.claims`) for row-security policy evaluation.
    pub fn set_var(&mut self, name: &str, value: &str) {
        self.vars
            .insert(name.to_ascii_lowercase(), value.to_string());
    }

    /// Parse and execute a (possibly multi-statement) SQL string.
    ///
    /// The input is split into top-level statements first (quote- and
    /// comment-aware, see [`crate::sql::parser::split_statements`]) so that
    /// `ALTER EXTENSION` — which sqlparser 0.62 has no AST for — can be routed
    /// to its hand parser; every other segment goes through [`parse_sql`]
    /// unchanged and all statements execute in their original order. Parsing
    /// happens up front, so a syntax error anywhere executes nothing.
    pub async fn execute(&mut self, sql: &str) -> Result<Vec<ExecResult>> {
        enum Piece {
            Statements(Vec<Statement>),
            AlterExtension(crate::sql::ext::alter::AlterExtension),
            SetConstraints(SetConstraintsStmt),
            TsDict(crate::sql::fts::TsDictCmd),
        }
        let mut pieces = Vec::new();
        for segment in crate::sql::parser::split_statements(sql) {
            if crate::sql::ext::alter::is_alter_extension(&segment) {
                pieces.push(Piece::AlterExtension(
                    crate::sql::ext::alter::parse_alter_extension(&segment)?,
                ));
            } else if is_set_constraints(&segment) {
                // Like `ALTER EXTENSION`, sqlparser 0.62 has no AST for `SET
                // CONSTRAINTS`, so it is recognized by prefix and hand-parsed
                // here instead of going through the general parser.
                pieces.push(Piece::SetConstraints(parse_set_constraints(&segment)?));
            } else if crate::sql::fts::is_ts_dict_ddl(&segment) {
                // `CREATE TEXT SEARCH DICTIONARY` / `DROP TEXT SEARCH DICTIONARY`
                // — sqlparser 0.62 has no AST for these, so they are routed to
                // the hand parser the same way `ALTER EXTENSION` is.
                pieces.push(Piece::TsDict(crate::sql::fts::parse_ts_dict_ddl(&segment)?));
            } else if let Some(feature) = unsupported_by_prefix(&segment) {
                // Truthfulness contract: statements the engine deliberately
                // does not implement are recognized by keyword prefix *before*
                // parsing, so every syntactic variant fails with the same
                // stable `0A000` — sqlparser accepts some spellings of these
                // and rejects others, which would otherwise leak a
                // form-dependent `42601` instead.
                return Err(SqlError::FeatureNotSupported(format!(
                    "{feature} is not supported"
                )));
            } else {
                pieces.push(Piece::Statements(crate::sql::parser::parse_sql(&segment)?));
            }
        }
        let mut results = Vec::new();
        for piece in pieces {
            match piece {
                Piece::Statements(stmts) => {
                    for stmt in stmts {
                        results.push(self.execute_one(&stmt, &[]).await?);
                    }
                }
                Piece::AlterExtension(cmd) => {
                    results.push(self.execute_alter_extension(&cmd).await?);
                }
                Piece::SetConstraints(cmd) => {
                    results.push(self.execute_set_constraints(&cmd).await?);
                }
                Piece::TsDict(cmd) => {
                    results.push(self.execute_ts_dict(cmd).await?);
                }
            }
        }
        Ok(results)
    }

    /// Prepare a statement for the extended query protocol.
    pub fn prepare(&self, sql: &str) -> Result<Prepared> {
        // sqlparser has no ALTER EXTENSION AST, so it cannot be carried through
        // the extended protocol's parse/bind/execute pipeline.
        if crate::sql::ext::alter::is_alter_extension(sql) {
            return Err(SqlError::Syntax(
                "ALTER EXTENSION is only supported over the simple query protocol — \
                 send it as an unprepared statement"
                    .into(),
            ));
        }
        // Same situation for `SET CONSTRAINTS` (see `is_set_constraints`).
        if is_set_constraints(sql) {
            return Err(SqlError::Syntax(
                "SET CONSTRAINTS is only supported over the simple query protocol — \
                 send it as an unprepared statement"
                    .into(),
            ));
        }
        // Same situation for `CREATE / DROP TEXT SEARCH DICTIONARY`.
        if crate::sql::fts::is_ts_dict_ddl(sql) {
            return Err(SqlError::Syntax(
                "CREATE/DROP TEXT SEARCH DICTIONARY is only supported over the simple query \
                 protocol — send it as an unprepared statement"
                    .into(),
            ));
        }
        // Deliberately-unsupported statements keep their stable `0A000` here
        // too, instead of a form-dependent parser error (see
        // [`unsupported_by_prefix`]).
        if let Some(feature) = unsupported_by_prefix(sql) {
            return Err(SqlError::FeatureNotSupported(format!(
                "{feature} is not supported"
            )));
        }
        let mut statements = crate::sql::parser::parse_sql(sql)?;
        let statement = match statements.len() {
            0 => Statement::Query(Box::new(empty_query())),
            1 => statements.remove(0),
            _ => {
                return Err(SqlError::Syntax(
                    "cannot insert multiple commands into a prepared statement".into(),
                ));
            }
        };
        let param_count = count_placeholders(sql);
        Ok(Prepared {
            sql: sql.to_string(),
            statement,
            param_count,
        })
    }

    /// Execute one statement with bound parameters.
    pub async fn execute_one(
        &mut self,
        stmt: &Statement,
        params: &[SqlValue],
    ) -> Result<ExecResult> {
        // Transaction control bypasses locking/abort handling.
        match stmt {
            Statement::StartTransaction { .. } => return self.begin().await,
            Statement::Commit { .. } => return self.commit().await,
            Statement::Rollback { .. } => return self.rollback().await,
            _ => {}
        }

        // A failed transaction ignores commands until it is ended.
        if self.txn.as_ref().map(|t| t.aborted).unwrap_or(false) {
            return Err(SqlError::InFailedTransaction);
        }

        // `SET lock_timeout = ...` is observed here.
        if matches!(stmt, Statement::Set(_)) {
            self.apply_set(&stmt.to_string());
        }

        let outcome = self.execute_routed(stmt, params).await;
        if outcome.is_err() {
            // Any error inside an explicit transaction aborts it (PostgreSQL);
            // an autocommit statement releases the locks it took.
            match &mut self.txn {
                Some(txn) => txn.aborted = true,
                None => self.db.locks.release_transaction(self.session_id),
            }
        }
        outcome
    }

    /// Sidecar-aware execution wrapper. Routing rules (see
    /// `docs/postgres-compat.md`):
    ///
    /// 1. `CREATE EXTENSION` of a sidecar-strategy extension is forwarded to
    ///    the configured sidecar and recorded in the local catalog with the
    ///    version the sidecar reports.
    /// 2. `DROP EXTENSION` naming a sidecar-bound extension forwards the drop
    ///    before removing the local record.
    /// 3. A statement that fails locally with undefined function/type/table
    ///    is forwarded verbatim when a sidecar DSN is configured — autocommit
    ///    only: inside an explicit transaction the local error is kept with a
    ///    hint, because the sidecar cannot join a local transaction.
    async fn execute_routed(
        &mut self,
        stmt: &Statement,
        params: &[SqlValue],
    ) -> Result<ExecResult> {
        use crate::sql::ext::RuntimeStrategy;
        if let Statement::CreateExtension(ce) = stmt {
            let name = crate::sql::names::ident_name(&ce.name).to_lowercase();
            if let Some(def) = crate::sql::ext::find(&name)
                && def.strategy == RuntimeStrategy::SidecarPostgres
            {
                return self.sidecar_create_extension(stmt, ce, def).await;
            }
        }
        if let Statement::DropExtension(de) = stmt {
            let catalog = match &self.txn {
                Some(txn) => txn.catalog.clone(),
                None => self.load_catalog().await?,
            };
            let any_sidecar = de.names.iter().any(|ident| {
                catalog.extension_is_sidecar(&crate::sql::names::ident_name(ident).to_lowercase())
            });
            if any_sidecar {
                return self.sidecar_drop_extension(de, catalog).await;
            }
        }
        match self.execute_inner(stmt, params).await {
            Err(e) if sidecar_routable(&e) && self.sidecar_dsn().is_some() => {
                if self.in_transaction() {
                    Err(with_sidecar_txn_hint(e))
                } else if params.is_empty() {
                    // The failed statement's autocommit locks are still held.
                    self.db.locks.release_transaction(self.session_id);
                    let mut results = self.sidecar_exec(&stmt.to_string()).await?;
                    results
                        .pop()
                        .ok_or_else(|| SqlError::Storage("sidecar returned no result".into()))
                } else {
                    // Bound parameters cannot be forwarded as verbatim text.
                    Err(e)
                }
            }
            other => other,
        }
    }

    /// `CREATE EXTENSION` of a sidecar-strategy extension: forward verbatim,
    /// then record the install locally with the version the sidecar reports.
    async fn sidecar_create_extension(
        &mut self,
        stmt: &Statement,
        ce: &sqlparser::ast::CreateExtension,
        def: &'static crate::sql::ext::ExtensionDef,
    ) -> Result<ExecResult> {
        let mut catalog = match &self.txn {
            Some(txn) => txn.catalog.clone(),
            None => self.load_catalog().await?,
        };
        if catalog.extension_installed(def.name) {
            if ce.if_not_exists {
                return Ok(ExecResult::empty_command("CREATE EXTENSION"));
            }
            return Err(SqlError::DuplicateObject(format!(
                "extension \"{}\"",
                def.name
            )));
        }
        if self.in_transaction() {
            return Err(SqlError::FeatureNotSupported(format!(
                "CREATE EXTENSION {} cannot run inside a transaction block — the \
                 PostgreSQL sidecar cannot join a local transaction",
                def.name
            )));
        }
        if self.sidecar_dsn().is_none() {
            return Err(crate::sql::ext::sidecar_unconfigured(def.name));
        }
        self.sidecar_exec(&stmt.to_string()).await?;
        let version = self
            .sidecar_scalar(&format!(
                "SELECT extversion FROM pg_extension WHERE extname = '{}'",
                def.name
            ))
            .await?
            .unwrap_or_else(|| def.default_version.to_string());
        catalog.install_sidecar_extension(def.name, &version);
        self.persist_catalog(catalog).await?;
        Ok(ExecResult::empty_command("CREATE EXTENSION"))
    }

    /// `DROP EXTENSION` where at least one name is sidecar-bound: forward each
    /// sidecar drop, apply native semantics to the rest, then persist.
    async fn sidecar_drop_extension(
        &mut self,
        de: &sqlparser::ast::DropExtension,
        mut catalog: Catalog,
    ) -> Result<ExecResult> {
        use sqlparser::ast::ReferentialAction as RA;
        if self.in_transaction() {
            return Err(SqlError::FeatureNotSupported(
                "DROP EXTENSION of a sidecar-bound extension cannot run inside a \
                 transaction block — the PostgreSQL sidecar cannot join a local \
                 transaction"
                    .into(),
            ));
        }
        for ident in &de.names {
            let name = crate::sql::names::ident_name(ident).to_lowercase();
            if catalog.extension_is_sidecar(&name) {
                if self.sidecar_dsn().is_none() {
                    return Err(crate::sql::ext::sidecar_unconfigured(&name));
                }
                let mut forward = String::from("DROP EXTENSION ");
                if de.if_exists {
                    forward.push_str("IF EXISTS ");
                }
                forward.push('"');
                forward.push_str(&name);
                forward.push('"');
                match de.cascade_or_restrict {
                    Some(RA::Cascade) => forward.push_str(" CASCADE"),
                    Some(RA::Restrict) => forward.push_str(" RESTRICT"),
                    _ => {}
                }
                self.sidecar_exec(&forward).await?;
                catalog.uninstall_extension(&name);
            } else {
                crate::sql::ext::drop_native_extension(
                    &mut catalog,
                    &name,
                    de.if_exists,
                    de.cascade_or_restrict,
                )?;
            }
        }
        self.persist_catalog(catalog).await?;
        Ok(ExecResult::empty_command("DROP EXTENSION"))
    }

    /// The configured sidecar DSN: the `guardian.sidecar_dsn` session variable
    /// wins; the `GUARDIAN_PG_SIDECAR_DSN` environment variable is the
    /// fallback. Empty values mean "not configured".
    fn sidecar_dsn(&self) -> Option<String> {
        if let Some(v) = self.vars.get("guardian.sidecar_dsn") {
            let v = v.trim();
            if v.is_empty() {
                return None; // SET guardian.sidecar_dsn = '' disables routing
            }
            return Some(v.to_string());
        }
        std::env::var("GUARDIAN_PG_SIDECAR_DSN")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// Run `sql` on the pinned sidecar connection, connecting lazily and
    /// reconnecting when the DSN changed or the previous connection broke.
    async fn sidecar_exec(&mut self, sql: &str) -> Result<Vec<ExecResult>> {
        let dsn = self
            .sidecar_dsn()
            .ok_or_else(|| crate::sql::ext::sidecar_unconfigured("(sidecar)"))?;
        let reusable = self
            .sidecar
            .as_ref()
            .map(|c| c.dsn() == dsn && !c.is_broken())
            .unwrap_or(false);
        if !reusable {
            self.sidecar = Some(crate::sql::ext::sidecar::SidecarConn::connect(&dsn).await?);
        }
        let conn = self
            .sidecar
            .as_mut()
            .expect("sidecar connection just pinned");
        let result = conn.simple_query(sql).await;
        if conn.is_broken() {
            self.sidecar = None;
        }
        result
    }

    /// First column of the first row of a sidecar query, as text.
    async fn sidecar_scalar(&mut self, sql: &str) -> Result<Option<String>> {
        for result in self.sidecar_exec(sql).await? {
            if let ExecResult::Rows { rows, .. } = result {
                return Ok(rows
                    .first()
                    .and_then(|row| row.first())
                    .and_then(|v| v.to_text()));
            }
        }
        Ok(None)
    }

    async fn execute_inner(&mut self, stmt: &Statement, params: &[SqlValue]) -> Result<ExecResult> {
        let catalog = match &self.txn {
            Some(txn) => txn.catalog.clone(),
            None => self.load_catalog().await?,
        };

        // Explicit `LOCK TABLE ... IN <mode> MODE [NOWAIT]`.
        if let Statement::Lock(lock) = stmt {
            return self.exec_lock_table(lock, &catalog).await;
        }

        // Acquire the implicit table-level locks for this statement.
        for (oid, mode) in table_lock_plan(stmt, &catalog) {
            self.db
                .locks
                .acquire(
                    self.session_id,
                    LockObject::Table(oid),
                    mode,
                    LockScope::Transaction,
                    WaitPolicy::Wait,
                    self.lock_timeout,
                )
                .await?;
        }

        // Preload referenced tables.
        let mut names = Vec::new();
        collect_stmt(stmt, &mut names);
        // Foreign-key enforcement reads parents (existence checks) and, for
        // UPDATE/DELETE/upsert, reads and writes referencing tables
        // transitively (referential actions); preload that ripple too.
        names.extend(fk_preload(stmt, &catalog));
        // Row-security policies may reference other tables (e.g. in EXISTS
        // subqueries); preload those too so policy evaluation can scan them.
        let mut policy_names = Vec::new();
        for (schema, name) in &names {
            if let Some(q) = catalog.resolve_table_name(schema.as_deref(), name)
                && let Some(table) = catalog.get_table(&q)
                && table.rls_enabled
            {
                for policy in &table.policies {
                    for text in policy.using_expr.iter().chain(policy.check_expr.iter()) {
                        if let Ok(expr) = crate::sql::parser::parse_expr(text) {
                            collect_expr(&expr, &mut policy_names);
                        }
                    }
                }
            }
        }
        names.extend(policy_names);
        // A called user-defined function's body can touch tables this
        // statement's own text never mentions (e.g. `SELECT my_proc(1)` where
        // `my_proc` is a PL/pgSQL function that updates some other table).
        // Table loading is synchronous and preload-driven (see
        // `crate::sql::exec`), so — rather than analyzing exactly which
        // functions this statement calls, including transitively through
        // other functions it calls — every user-defined function's body is
        // scanned for table references whenever the catalog has any. Extra
        // preloaded tables are harmless (unused entries in `Exec::tables`);
        // the alternative (a precise call-graph analysis) risks a false
        // "relation does not exist" for a table a called function needs.
        for f in catalog.functions() {
            for body_stmt in crate::sql::udf::body_statements(f) {
                collect_stmt(&body_stmt, &mut names);
            }
        }
        let mut tables: HashMap<QualifiedName, Arc<LoadedTable>> = HashMap::new();
        for (schema, name) in &names {
            if let Some(q) = catalog.resolve_table_name(schema.as_deref(), name)
                && !tables.contains_key(&q)
                && let Some(loaded) = self.load_table(&catalog, &q).await?
            {
                tables.insert(q, loaded);
            }
        }

        // ANN bookkeeping (RFC 0005): snapshot the set of hnsw index oids so
        // any DDL that drops one — DROP INDEX, DROP TABLE/COLUMN, schema
        // cascades — is caught by a single post-statement diff instead of
        // per-path wiring. Eager forgetting inside a transaction that later
        // rolls back only costs a rebuild: derived state self-heals.
        #[cfg(feature = "vector-index")]
        let hnsw_before: Vec<u32> = catalog
            .indexes()
            .filter(|i| i.method == "hnsw")
            .map(|i| i.oid)
            .collect();

        // Build the synchronous execution context and run.
        let now = chrono::Utc::now();
        let mut exec = Exec::new(
            catalog,
            tables,
            params.to_vec(),
            now,
            self.db.name.clone(),
            self.username.clone(),
            self.db.locks.clone(),
            self.session_id,
        );
        exec.vars = std::cell::RefCell::new(self.vars.clone());
        // Shared ANN runtime for the vector planner hook (RFC 0005).
        #[cfg(feature = "vector-index")]
        {
            exec.ann = Some(self.db.ann.clone());
        }
        // This transaction's `SET CONSTRAINTS` state, read-only for the
        // statement (see `ConstraintModes`); `None` in autocommit, which
        // always means "check every foreign key immediately".
        exec.constraint_modes = self.txn.as_ref().map(|t| t.constraint_modes.clone());
        // Row security: compute per-table visibility once, before anything is
        // evaluated (CTEs, scans and DML snapshots all consult it).
        exec.init_rls(stmt)?;
        // Pre-materialize top-level CTEs, in order. Recursive members of a
        // `WITH RECURSIVE` iterate to a fixpoint against a working table (see
        // `Exec::materialize_with`); everything else materializes exactly
        // once, non-recursively.
        if let Statement::Query(q) = stmt
            && let Some(with) = &q.with
        {
            exec.materialize_with(with)?;
        }
        let result = self.dispatch(&mut exec, stmt)?;
        // Persist variable writes made during execution (e.g. `set_limit`).
        self.vars = exec.vars.borrow().clone();

        // Acquire row / blocking-advisory locks queued during execution.
        let pending: Vec<_> = exec.pending_locks.borrow_mut().drain(..).collect();
        for (object, mode, scope) in pending {
            self.db
                .locks
                .acquire(
                    self.session_id,
                    object,
                    mode,
                    scope,
                    WaitPolicy::Wait,
                    self.lock_timeout,
                )
                .await?;
        }

        // Commit or stage the produced mutations / catalog changes.
        let mutations = std::mem::take(&mut *exec.mutations.lock().unwrap());
        let catalog_dirty = exec.catalog_dirty;
        // Foreign-key checks this statement postponed (see `DeferredFkCheck`)
        // because their constraint is currently running `DEFERRED`.
        let new_deferred: Vec<_> = exec.deferred_checks.borrow_mut().drain(..).collect();
        // CONSTRAINT TRIGGER firings deferred to COMMIT (see
        // `DeferredTriggerFiring`) — splice into the transaction's queue.
        let new_deferred_triggers: Vec<_> = std::mem::take(&mut exec.deferred_triggers);
        let new_catalog = exec.catalog;
        // Forget ANN state for hnsw indexes this statement dropped (see the
        // `hnsw_before` snapshot above).
        #[cfg(feature = "vector-index")]
        if catalog_dirty {
            let after: std::collections::HashSet<u32> = new_catalog
                .indexes()
                .filter(|i| i.method == "hnsw")
                .map(|i| i.oid)
                .collect();
            for oid in hnsw_before {
                if !after.contains(&oid) {
                    self.db.ann.forget(oid);
                }
            }
        }
        match &mut self.txn {
            Some(txn) => {
                txn.catalog = new_catalog;
                txn.catalog_dirty |= catalog_dirty;
                txn.pending_deferred.extend(new_deferred);
                txn.deferred_triggers.extend(new_deferred_triggers);
                stage_mutations(txn, mutations);
            }
            None => {
                // Autocommit: `exec.constraint_modes` was `None` above, which
                // `Exec::fk_is_deferred` always treats as "check
                // immediately", so `new_deferred` is always empty here —
                // there is no transaction for it to survive into anyway.
                self.apply_mutations(mutations).await?;
                if catalog_dirty {
                    self.save_catalog(&new_catalog).await?;
                    // Column/index-set changes are invisible to the storage
                    // change counter; drop cached decoded views so the next
                    // load rebuilds against the new schema.
                    self.db.invalidate_table_cache();
                }
                // Autocommit: release the locks this statement acquired.
                self.db.locks.release_transaction(self.session_id);
            }
        }
        Ok(result)
    }

    async fn exec_lock_table(
        &mut self,
        lock: &sqlparser::ast::Lock,
        catalog: &Catalog,
    ) -> Result<ExecResult> {
        let mode = map_lock_table_mode(lock.lock_mode.clone());
        let wait = if lock.nowait {
            WaitPolicy::NoWait
        } else {
            WaitPolicy::Wait
        };
        for target in &lock.tables {
            let (schema, name) = crate::sql::names::split_schema_table(&target.name);
            let q = catalog
                .resolve_table_name(schema.as_deref(), &name)
                .ok_or_else(|| SqlError::UndefinedTable(name.clone()))?;
            let oid = catalog.require_table(&q)?.oid;
            self.db
                .locks
                .acquire(
                    self.session_id,
                    LockObject::Table(oid),
                    mode,
                    LockScope::Transaction,
                    wait,
                    self.lock_timeout,
                )
                .await?;
        }
        Ok(ExecResult::empty_command("LOCK TABLE"))
    }

    /// Observe `SET name = value`. `lock_timeout` feeds the lock manager; every
    /// other variable (extension GUCs like `pg_trgm.similarity_threshold`,
    /// application settings) is stored as a session variable readable via
    /// `SHOW` / `current_setting()`.
    fn apply_set(&mut self, text: &str) {
        let body = text.trim().trim_end_matches(';');
        let Some(eq) = body.find('=') else { return };
        // "SET [LOCAL|SESSION] <name>" — take the last identifier before `=`.
        let name = body[..eq]
            .trim()
            .rsplit(char::is_whitespace)
            .next()
            .unwrap_or("")
            .trim_matches('"')
            .to_ascii_lowercase();
        if name.is_empty() {
            return;
        }
        let raw = body[eq + 1..]
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .to_string();
        if name == "lock_timeout" {
            let ms = parse_timeout_ms(&raw);
            self.lock_timeout = if ms == 0 {
                None
            } else {
                Some(Duration::from_millis(ms))
            };
        }
        self.vars.insert(name, raw);
    }

    /// Execute a hand-parsed `ALTER EXTENSION`, mirroring `execute_one`'s
    /// transaction semantics (ignored while aborted; an error aborts an open
    /// block or releases autocommit locks).
    async fn execute_alter_extension(
        &mut self,
        cmd: &crate::sql::ext::alter::AlterExtension,
    ) -> Result<ExecResult> {
        if self.txn.as_ref().map(|t| t.aborted).unwrap_or(false) {
            return Err(SqlError::InFailedTransaction);
        }
        let outcome = self.alter_extension_inner(cmd).await;
        if outcome.is_err() {
            match &mut self.txn {
                Some(txn) => txn.aborted = true,
                None => self.db.locks.release_transaction(self.session_id),
            }
        }
        outcome
    }

    async fn alter_extension_inner(
        &mut self,
        cmd: &crate::sql::ext::alter::AlterExtension,
    ) -> Result<ExecResult> {
        use crate::sql::ext::alter::AlterExtensionAction as Action;
        let mut catalog = match &self.txn {
            Some(txn) => txn.catalog.clone(),
            None => self.load_catalog().await?,
        };
        // Every form requires the extension to be installed (PostgreSQL's
        // `extension "x" does not exist`, SQLSTATE 42704).
        if !catalog.extension_installed(&cmd.name) {
            return Err(SqlError::UndefinedObject(format!(
                "extension \"{}\"",
                cmd.name
            )));
        }
        match &cmd.action {
            Action::Update { to } => {
                let def = crate::sql::ext::find(&cmd.name).ok_or_else(|| {
                    SqlError::UndefinedObject(format!("extension \"{}\"", cmd.name))
                })?;
                if let Some(v) = to
                    && v != def.default_version
                {
                    return Err(SqlError::UndefinedObject(format!(
                        "extension \"{}\" has no update path to version \"{v}\" \
                         (available version: \"{}\")",
                        def.name, def.default_version
                    )));
                }
                catalog.set_extension_version(def.name, def.default_version);
                self.persist_catalog(catalog).await?;
                Ok(ExecResult::empty_command("ALTER EXTENSION"))
            }
            Action::SetSchema(_) => Err(SqlError::FeatureNotSupported(format!(
                "extension \"{}\" is not relocatable",
                cmd.name
            ))),
            Action::Add(obj) | Action::Drop(obj) => Err(SqlError::FeatureNotSupported(format!(
                "ALTER EXTENSION {} {} {obj}: PostgreSQL reserves extension membership \
                 changes for extension scripts, and GuardianDB extension contents are fixed",
                cmd.name,
                if matches!(cmd.action, Action::Add(_)) {
                    "ADD"
                } else {
                    "DROP"
                },
            ))),
        }
    }

    /// Execute a hand-parsed `CREATE / DROP TEXT SEARCH DICTIONARY`, mirroring
    /// `execute_one`'s transaction semantics (ignored while aborted; an error
    /// aborts an open block or releases autocommit locks).
    async fn execute_ts_dict(&mut self, cmd: crate::sql::fts::TsDictCmd) -> Result<ExecResult> {
        if self.txn.as_ref().map(|t| t.aborted).unwrap_or(false) {
            return Err(SqlError::InFailedTransaction);
        }
        let outcome = self.ts_dict_inner(cmd).await;
        if outcome.is_err() {
            match &mut self.txn {
                Some(txn) => txn.aborted = true,
                None => self.db.locks.release_transaction(self.session_id),
            }
        }
        outcome
    }

    async fn ts_dict_inner(&mut self, cmd: crate::sql::fts::TsDictCmd) -> Result<ExecResult> {
        use crate::relational::catalog::TsDictionaryDef;
        use crate::sql::fts::TsDictCmd;
        let mut catalog = match &self.txn {
            Some(txn) => txn.catalog.clone(),
            None => self.load_catalog().await?,
        };
        match cmd {
            TsDictCmd::Create {
                schema,
                name,
                synonyms,
                thesaurus_entries,
                if_not_exists,
            } => {
                let schema = schema.unwrap_or_else(|| "public".into());
                let oid = catalog.allocate_oid();
                let def = TsDictionaryDef {
                    name: name.clone(),
                    schema,
                    oid,
                    synonyms,
                    thesaurus_entries,
                };
                catalog.insert_ts_dictionary(def, if_not_exists)?;
                self.persist_catalog(catalog).await?;
                Ok(ExecResult::empty_command("CREATE TEXT SEARCH DICTIONARY"))
            }
            TsDictCmd::Drop {
                schema,
                name,
                if_exists,
            } => {
                let schema = schema.as_deref();
                catalog.drop_ts_dictionary(schema, &name, if_exists)?;
                self.persist_catalog(catalog).await?;
                Ok(ExecResult::empty_command("DROP TEXT SEARCH DICTIONARY"))
            }
        }
    }

    /// Execute a hand-parsed `SET CONSTRAINTS`, mirroring `execute_one`'s
    /// transaction semantics (ignored while aborted; an error aborts an open
    /// block).
    async fn execute_set_constraints(&mut self, cmd: &SetConstraintsStmt) -> Result<ExecResult> {
        if self.txn.as_ref().map(|t| t.aborted).unwrap_or(false) {
            return Err(SqlError::InFailedTransaction);
        }
        let outcome = self.set_constraints_inner(cmd).await;
        if outcome.is_err() {
            match &mut self.txn {
                Some(txn) => txn.aborted = true,
                None => self.db.locks.release_transaction(self.session_id),
            }
        }
        outcome
    }

    /// `SET CONSTRAINTS { ALL | name [, ...] } { DEFERRED | IMMEDIATE }`
    /// (PostgreSQL semantics, verified against `sql-set-constraints.html` and
    /// `AfterTriggerSetState` in `src/backend/commands/trigger.c`):
    ///
    /// * Name resolution and `DEFERRABLE`-ness validation (the `42704`/`42809`
    ///   errors below) happen the same way whether or not an explicit
    ///   transaction block is open — verified against a live PostgreSQL 16
    ///   instance: `SET CONSTRAINTS bogus_name IMMEDIATE` with no `BEGIN`
    ///   still raises `42704`, and naming a `NOT DEFERRABLE` constraint with
    ///   `DEFERRED` outside a block still raises `42809`, each alongside
    ///   PostgreSQL's own `WARNING: SET CONSTRAINTS can only be used in
    ///   transaction blocks`.
    /// * What *is* a no-op outside an explicit transaction block is the mode
    ///   change itself, plus the retroactive re-check `IMMEDIATE` would
    ///   otherwise force: there is no transaction-scoped `SET CONSTRAINTS`
    ///   state to change there, and no deferred check could possibly be
    ///   pending yet either (`Exec::constraint_modes` is `None` in
    ///   autocommit, so `Exec::fk_is_deferred` never queues one) — so a
    ///   *validated* request outside a block has nothing left to do, matching
    ///   PostgreSQL's "otherwise has no effect."
    /// * `ALL` sets the blanket mode for every deferrable constraint and
    ///   forgets any earlier per-name overrides from this transaction
    ///   (PostgreSQL does exactly this — see `AfterTriggerSetState`'s `ALL`
    ///   branch) — it never errors, even when some constraint is `NOT
    ///   DEFERRABLE` (such a constraint is simply unaffected either way).
    /// * A named constraint that does not exist anywhere reachable is
    ///   `42704` ("constraint ... does not exist"); a named constraint that
    ///   is `NOT DEFERRABLE` errors only when asked to go `DEFERRED` —
    ///   `42809` ("constraint ... is not deferrable", *not* the `42704`
    ///   "does not exist" a first guess might reach for) — asking for
    ///   `IMMEDIATE` on one is a silent no-op (it is already always
    ///   immediate, matching PostgreSQL's own `AfterTriggerSetState`, which
    ///   only raises that error on the `DEFERRED` branch).
    /// * Setting `IMMEDIATE` (`ALL` or named) retroactively re-validates
    ///   every currently pending deferred check the request covers right
    ///   now, raising the FK violation error immediately if one is still
    ///   unsatisfied (SQL99/PostgreSQL: "the effects of the SET CONSTRAINTS
    ///   command apply retroactively").
    async fn set_constraints_inner(&mut self, stmt: &SetConstraintsStmt) -> Result<ExecResult> {
        // Resolve + validate against a snapshot first, so a `42704`/`42809`
        // never partially applies a mode change -- and so validation happens
        // identically whether or not an explicit transaction block is open
        // (see the doc comment above).
        let catalog = match &self.txn {
            Some(txn) => txn.catalog.clone(),
            None => self.load_catalog().await?,
        };
        let mut identities: Vec<(QualifiedName, String)> = Vec::new();
        if let Some(names) = &stmt.names {
            for (schema, name) in names {
                let matches = catalog.foreign_keys_named(schema.as_deref(), name);
                if matches.is_empty() {
                    return Err(SqlError::UndefinedObject(format!(
                        "constraint \"{name}\" does not exist"
                    )));
                }
                for (table_q, fk) in matches {
                    if stmt.deferred && !fk.deferrable.is_deferrable() {
                        return Err(SqlError::WrongObjectType(format!(
                            "constraint \"{}\" is not deferrable",
                            fk.name
                        )));
                    }
                    identities.push((table_q, fk.name));
                }
            }
        }
        // A validated request outside an explicit transaction block has
        // nothing left to apply (see the doc comment above).
        if self.txn.is_none() {
            return Ok(ExecResult::empty_command("SET CONSTRAINTS"));
        }
        {
            let txn = self.txn.as_mut().unwrap();
            if stmt.names.is_none() {
                txn.constraint_modes.named.clear();
                txn.constraint_modes.all_deferred = Some(stmt.deferred);
            } else {
                for id in &identities {
                    txn.constraint_modes.named.insert(id.clone(), stmt.deferred);
                }
            }
        }
        if !stmt.deferred {
            let (to_run, keep): (Vec<DeferredFkCheck>, Vec<DeferredFkCheck>) = {
                let txn = self.txn.as_mut().unwrap();
                std::mem::take(&mut txn.pending_deferred)
                    .into_iter()
                    .partition(|c| {
                        stmt.names.is_none() || {
                            let (t, n) = c.identity();
                            identities
                                .iter()
                                .any(|(it, iname)| it == t && iname.as_str() == n)
                        }
                    })
            };
            self.txn.as_mut().unwrap().pending_deferred = keep;
            let txn = self.txn.as_ref().unwrap();
            self.check_deferred(&catalog, &txn.overlay, &txn.truncated, to_run)
                .await?;
        }
        Ok(ExecResult::empty_command("SET CONSTRAINTS"))
    }

    /// Persist a modified catalog the way `execute_inner`'s tail does: stage
    /// it on the open transaction, or save it and release autocommit locks.
    async fn persist_catalog(&mut self, catalog: Catalog) -> Result<()> {
        match &mut self.txn {
            Some(txn) => {
                txn.catalog = catalog;
                txn.catalog_dirty = true;
            }
            None => {
                self.save_catalog(&catalog).await?;
                self.db.locks.release_transaction(self.session_id);
            }
        }
        Ok(())
    }

    fn dispatch(&self, exec: &mut Exec, stmt: &Statement) -> Result<ExecResult> {
        match stmt {
            Statement::Query(q) => {
                // The auto-embedding SQL surface (RFC 0005 §6.3):
                // `SELECT guardian_embed(...)` / `guardian_unembed(...)` are
                // catalog-mutating commands disguised as function calls
                // (sqlparser cannot parse string args on CREATE TRIGGER). Any
                // ordinary query returns `None` and runs normally.
                if let Some(result) = exec.try_embedding_command(q) {
                    return result;
                }
                // Row-level locking (FOR UPDATE / FOR SHARE [NOWAIT | SKIP LOCKED]).
                exec.prepare_for_update(q)?;
                let rs = exec.exec_select_query(q, &[])?;
                let fields = rs
                    .schema
                    .fields
                    .iter()
                    .map(|f| crate::sql::result::OutField::new(f.name.clone(), f.ty.clone()))
                    .collect();
                Ok(ExecResult::Rows {
                    fields,
                    rows: rs.rows,
                })
            }
            // `EXPLAIN <select>` (RFC 0005 §6.2): this engine has no
            // cost-based planner producing a static tree, and the ANN scan
            // decision (adaptive growth, measured selectivity) only exists at
            // run time — so EXPLAIN *executes* the query, discards its rows
            // and reports the plan decisions actually taken (EXPLAIN ANALYZE
            // semantics). Restricted to SELECT so EXPLAIN can never run DML
            // side effects.
            #[cfg(feature = "vector-index")]
            Statement::Explain {
                statement: inner, ..
            } => {
                let Statement::Query(q) = inner.as_ref() else {
                    return Err(SqlError::FeatureNotSupported(
                        "EXPLAIN is supported for SELECT statements only".into(),
                    ));
                };
                exec.plan_notes.borrow_mut().clear();
                // Deliberately NOT `prepare_for_update`: EXPLAIN is inspection.
                // Queuing this SELECT's `FOR UPDATE`/`FOR SHARE` row locks (or
                // consuming rows under `SKIP LOCKED`) would be an observable,
                // non-read-only side effect from a statement users expect to be
                // side-effect-free. The plan report needs none of it.
                let _ = exec.exec_select_query(q, &[])?;
                let mut notes = std::mem::take(&mut *exec.plan_notes.borrow_mut());
                if notes.is_empty() {
                    notes.push("Seq Scan (default execution; no indexed path chosen)".into());
                }
                Ok(ExecResult::Rows {
                    fields: vec![crate::sql::result::OutField::new(
                        "QUERY PLAN".to_string(),
                        crate::relational::SqlType::Text,
                    )],
                    rows: notes
                        .into_iter()
                        .map(|n| vec![crate::relational::SqlValue::Text(n)])
                        .collect(),
                })
            }
            Statement::Insert(insert) => exec.exec_insert(insert),
            Statement::Update(update) => exec.exec_update(update),
            Statement::Delete(delete) => exec.exec_delete(delete),
            Statement::CreateTable(ct) => exec.exec_create_table(ct),
            Statement::CreateSchema {
                schema_name,
                if_not_exists,
                ..
            } => {
                let name = schema_name_to_string(schema_name);
                exec.exec_create_schema(&name, *if_not_exists)
            }
            Statement::CreateIndex(ci) => exec.exec_create_index(ci),
            Statement::CreateView(cv) => exec.exec_create_view(cv),
            Statement::AlterTable(alter) => exec.exec_alter_table(&alter.name, &alter.operations),
            Statement::Drop {
                object_type,
                if_exists,
                names,
                cascade,
                ..
            } => exec.exec_drop(object_type, *if_exists, names, *cascade),
            Statement::Truncate(_) => exec.exec_truncate(stmt),
            Statement::Set(_) => Ok(ExecResult::empty_command("SET")),
            Statement::CreatePolicy(cp) => exec.exec_create_policy(cp),
            Statement::DropPolicy(dp) => exec.exec_drop_policy(dp),
            Statement::CreateExtension(ce) => exec.exec_create_extension(ce),
            Statement::DropExtension(de) => exec.exec_drop_extension(de),
            Statement::CreateFunction(cf) => exec.exec_create_function(cf),
            Statement::DropFunction(df) => exec.exec_drop_function(df),
            Statement::ShowVariable { variable } => {
                let name = variable
                    .iter()
                    .map(|i| crate::sql::names::ident_name(i).to_ascii_lowercase())
                    .collect::<Vec<_>>()
                    .join(".");
                let value = exec
                    .vars
                    .borrow()
                    .get(&name)
                    .cloned()
                    .or_else(|| crate::sql::ext::default_guc(&name).map(str::to_string))
                    .or_else(|| builtin_show_default(&name));
                match value {
                    Some(v) => Ok(ExecResult::Rows {
                        fields: vec![crate::sql::result::OutField::new(
                            name.clone(),
                            crate::relational::SqlType::Text,
                        )],
                        rows: vec![vec![crate::relational::SqlValue::Text(v)]],
                    }),
                    None => Err(SqlError::UndefinedObject(format!(
                        "unrecognized configuration parameter \"{name}\""
                    ))),
                }
            }
            // Truthfulness contract: features the engine deliberately does not
            // implement get a *named* stable rejection (0A000) rather than the
            // generic fallback message. These arms serve the extended query
            // protocol; the simple protocol already rejects the same statements
            // by keyword prefix (see [`unsupported_by_prefix`]).
            Statement::CreateProcedure { .. } => Err(SqlError::FeatureNotSupported(
                "CREATE PROCEDURE is not supported".into(),
            )),
            Statement::CreateTrigger(ct) => exec.exec_create_trigger(ct),
            Statement::DropTrigger(dt) => exec.exec_drop_trigger(dt),
            other => self.dispatch_fallback(other),
        }
    }

    /// Handle utility statements (SET/SHOW/RESET/...) by inspecting the text.
    fn dispatch_fallback(&self, stmt: &Statement) -> Result<ExecResult> {
        let text = stmt.to_string();
        let mut words = text.split_whitespace();
        let first = words.next().unwrap_or("").to_ascii_uppercase();
        let second = words.next().unwrap_or("").to_ascii_uppercase();
        // Extension / sequence management is a no-op (sequences are managed
        // implicitly by serial columns; no extensions are required).
        if matches!(
            (first.as_str(), second.as_str()),
            ("CREATE", "EXTENSION")
                | ("DROP", "EXTENSION")
                | ("CREATE", "SEQUENCE")
                | ("ALTER", "SEQUENCE")
                | ("DROP", "SEQUENCE")
        ) {
            return Ok(ExecResult::empty_command(format!("{first} {second}")));
        }
        match first.as_str() {
            "SET" | "RESET" | "DISCARD" | "DEALLOCATE" | "LISTEN" | "UNLISTEN" | "CHECKPOINT"
            | "CLOSE" | "ANALYZE" | "VACUUM" | "COMMENT" | "GRANT" | "REVOKE" | "SAVEPOINT"
            | "RELEASE" | "PREPARE" | "EXECUTE" => Ok(ExecResult::empty_command(first)),
            "SHOW" => {
                let var = text
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("")
                    .trim_end_matches(';')
                    .to_string();
                let value = show_value(&var);
                Ok(ExecResult::Rows {
                    fields: vec![crate::sql::result::OutField::new(
                        if var.is_empty() {
                            "show".to_string()
                        } else {
                            var
                        },
                        crate::relational::SqlType::Text,
                    )],
                    rows: vec![vec![SqlValue::Text(value)]],
                })
            }
            _ => Err(SqlError::FeatureNotSupported(format!(
                "statement not supported: {first}"
            ))),
        }
    }

    // ---- transaction control -------------------------------------------

    async fn begin(&mut self) -> Result<ExecResult> {
        if self.txn.is_none() {
            let catalog = self.load_catalog().await?;
            self.txn = Some(Transaction {
                catalog,
                catalog_dirty: false,
                overlay: HashMap::new(),
                truncated: HashSet::new(),
                aborted: false,
                constraint_modes: ConstraintModes::default(),
                pending_deferred: Vec::new(),
                deferred_triggers: Vec::new(),
            });
        }
        Ok(ExecResult::empty_command("BEGIN"))
    }

    async fn commit(&mut self) -> Result<ExecResult> {
        if let Some(mut txn) = self.txn.take() {
            // Committing an aborted transaction rolls it back (PostgreSQL).
            if txn.aborted {
                self.db.locks.release_transaction(self.session_id);
                return Ok(ExecResult::empty_command("ROLLBACK"));
            }
            // PostgreSQL validates any still-`DEFERRED` constraint checks as
            // the last step before a transaction actually commits; if one is
            // still violated, `COMMIT` itself fails and the transaction rolls
            // back (nothing below has run yet, so nothing needs undoing —
            // `txn`'s writes just disappear with it, and the session has no
            // transaction afterward, matching PostgreSQL: a re-issued
            // `COMMIT`/`ROLLBACK` after a failed one finds none in progress).
            if !txn.pending_deferred.is_empty() {
                let checks = std::mem::take(&mut txn.pending_deferred);
                if let Err(e) = self
                    .check_deferred(&txn.catalog, &txn.overlay, &txn.truncated, checks)
                    .await
                {
                    self.db.locks.release_transaction(self.session_id);
                    return Err(e);
                }
            }
            // Fire any CONSTRAINT TRIGGER firings deferred to COMMIT.
            // Trigger bodies may INSERT/UPDATE/DELETE (e.g. audit log inserts);
            // those mutations are applied directly to storage by
            // fire_deferred_triggers before the transaction's main overlay is
            // written below.
            if !txn.deferred_triggers.is_empty() {
                let firings = std::mem::take(&mut txn.deferred_triggers);
                if let Err(e) = self
                    .fire_deferred_triggers(&txn.catalog, &txn.overlay, &txn.truncated, firings)
                    .await
                {
                    self.db.locks.release_transaction(self.session_id);
                    return Err(e);
                }
            }
            let watch = self.db.has_change_listeners();
            let at = chrono::Utc::now();
            let mut events = Vec::new();
            for c in &txn.truncated {
                self.db.storage.truncate(c).await?;
            }
            for (collection, rows) in &txn.overlay {
                for (rid, val) in rows {
                    let old = if watch {
                        self.db.storage.get(collection, rid).await?
                    } else {
                        None
                    };
                    match val {
                        Some(doc) => {
                            if watch {
                                push_change(&mut events, old.as_ref(), Some(doc), at);
                            }
                            self.db.storage.put(collection, rid, doc).await?
                        }
                        None => {
                            if watch {
                                push_change(&mut events, old.as_ref(), None, at);
                            }
                            self.db.storage.delete(collection, rid).await?
                        }
                    }
                }
            }
            if txn.catalog_dirty {
                self.save_catalog(&txn.catalog).await?;
                // Schema changes are invisible to the storage change counter
                // (see `invalidate_table_cache`).
                self.db.invalidate_table_cache();
            }
            self.db.emit_changes(events);
        }
        self.db.locks.release_transaction(self.session_id);
        Ok(ExecResult::empty_command("COMMIT"))
    }

    async fn rollback(&mut self) -> Result<ExecResult> {
        self.txn = None;
        self.db.locks.release_transaction(self.session_id);
        Ok(ExecResult::empty_command("ROLLBACK"))
    }

    // ---- storage helpers -----------------------------------------------

    async fn load_catalog(&self) -> Result<Catalog> {
        match self.db.storage.load_catalog().await? {
            Some(json) => serde_json::from_value(json)
                .map_err(|e| SqlError::Storage(format!("corrupt catalog: {e}"))),
            None => Ok(Catalog::new(&self.db.name)),
        }
    }

    async fn save_catalog(&self, catalog: &Catalog) -> Result<()> {
        let json = serde_json::to_value(catalog)
            .map_err(|e| SqlError::Storage(format!("serialize catalog: {e}")))?;
        self.db.storage.save_catalog(&json).await
    }

    async fn load_table(
        &self,
        catalog: &Catalog,
        q: &QualifiedName,
    ) -> Result<Option<Arc<LoadedTable>>> {
        let Some(table) = catalog.get_table(q) else {
            return Ok(None);
        };
        let collection = table.storage_collection.clone();
        let index_defs: Vec<_> = catalog
            .indexes_for_table(&q.schema, &q.name)
            .into_iter()
            .cloned()
            .collect();

        // Does this statement's transaction have staged writes for this
        // collection? If so the cached (committed) view is not what the
        // statement must see — fall through to the merge path below.
        let overlay = self.txn.as_ref().and_then(|txn| {
            let truncated = txn.truncated.contains(&collection);
            let ov = txn.overlay.get(&collection);
            (truncated || ov.is_some()).then_some((truncated, ov))
        });

        // Uncommitted schema changes in this transaction (a DDL statement ran
        // since BEGIN) make the shared, committed-meta cache unsafe: a cache
        // hit would return the *pre-DDL* `LoadedTable` meta this transaction
        // must not see (e.g. an added column would be invisible), and caching
        // a table built with this transaction's uncommitted meta would leak it
        // to other sessions at the same storage generation. So a transaction
        // with pending catalog changes builds fresh and uncached — DDL inside
        // a transaction is the uncommon path.
        let txn_schema_dirty = self.txn.as_ref().is_some_and(|t| t.catalog_dirty);

        if overlay.is_none() && !txn_schema_dirty {
            // Hot path: no staged writes, no uncommitted schema. Serve from the
            // decoded-table cache when the backend reports a change counter;
            // otherwise decode fresh (uncacheable backend) but still return an
            // Arc.
            let table = table.clone();
            if let Some(generation) = self.db.storage.generation(&collection) {
                let built = self
                    .db
                    .cached_table(&collection, generation, move |docs| {
                        LoadedTable::build(table, docs, index_defs)
                    })
                    .await?;
                return Ok(Some(built));
            }
            let docs = self.db.storage.scan(&collection).await?;
            return Ok(Some(Arc::new(LoadedTable::build(table, docs, index_defs)?)));
        }

        // Fresh, uncached build: merge the committed scan with the
        // transaction's staged writes (when any), decoded against the current
        // — possibly uncommitted — catalog meta. The result is private to this
        // statement/transaction and never enters the shared cache.
        let truncated = matches!(overlay, Some((true, _)));
        let mut map: std::collections::BTreeMap<String, Json> = if truncated {
            std::collections::BTreeMap::new()
        } else {
            self.db
                .storage
                .scan(&collection)
                .await?
                .into_iter()
                .collect()
        };
        if let Some((_, Some(ov))) = overlay {
            for (rid, val) in ov {
                match val {
                    Some(doc) => {
                        map.insert(rid.clone(), doc.clone());
                    }
                    None => {
                        map.remove(rid);
                    }
                }
            }
        }
        let docs = map.into_iter().collect();
        Ok(Some(Arc::new(LoadedTable::build(
            table.clone(),
            docs,
            index_defs,
        )?)))
    }

    /// Load `q` reflecting explicitly-given `overlay`/`truncated` writes —
    /// the same merge [`Session::load_table`] does via `self.txn`, factored
    /// out so it can also be used once a `Transaction` has already been
    /// taken out of `self.txn` (see [`Session::check_deferred`], called from
    /// `commit` after `self.txn.take()`).
    async fn load_table_with_overlay(
        &self,
        catalog: &Catalog,
        q: &QualifiedName,
        overlay: &HashMap<String, HashMap<String, Option<Json>>>,
        truncated: &HashSet<String>,
    ) -> Result<Option<Arc<LoadedTable>>> {
        let Some(table) = catalog.get_table(q) else {
            return Ok(None);
        };
        let collection = table.storage_collection.clone();
        let mut docs = self.db.storage.scan(&collection).await?;
        let is_truncated = truncated.contains(&collection);
        let collection_overlay = overlay.get(&collection);
        if is_truncated || collection_overlay.is_some() {
            let mut map: std::collections::BTreeMap<String, Json> = if is_truncated {
                std::collections::BTreeMap::new()
            } else {
                docs.into_iter().collect()
            };
            if let Some(ov) = collection_overlay {
                for (rid, val) in ov {
                    match val {
                        Some(doc) => {
                            map.insert(rid.clone(), doc.clone());
                        }
                        None => {
                            map.remove(rid);
                        }
                    }
                }
            }
            docs = map.into_iter().collect();
        }
        let index_defs = catalog
            .indexes_for_table(&q.schema, &q.name)
            .into_iter()
            .cloned()
            .collect();
        Ok(Some(Arc::new(LoadedTable::build(
            table.clone(),
            docs,
            index_defs,
        )?)))
    }

    /// Re-validate every check in `checks` ([`DeferredFkCheck`]) against the
    /// transaction's *current* state — `overlay`/`truncated` are its own
    /// staged writes (not yet applied to storage), so this sees exactly what
    /// the transaction itself would see, whether or not `self.txn` still
    /// holds it. Loads only the tables the checks actually reference. Called
    /// by `COMMIT` ([`Session::commit`], after taking the transaction out of
    /// `self.txn` but before applying its writes) and by `SET CONSTRAINTS
    /// ... IMMEDIATE` ([`Session::exec_set_constraints`], while `self.txn` is
    /// still live).
    async fn check_deferred(
        &self,
        catalog: &Catalog,
        overlay: &HashMap<String, HashMap<String, Option<Json>>>,
        truncated: &HashSet<String>,
        checks: Vec<DeferredFkCheck>,
    ) -> Result<()> {
        if checks.is_empty() {
            return Ok(());
        }
        let mut tables: HashMap<QualifiedName, Arc<LoadedTable>> = HashMap::new();
        for q in deferred_check_tables(&checks, catalog) {
            if let Some(t) = self
                .load_table_with_overlay(catalog, &q, overlay, truncated)
                .await?
            {
                tables.insert(q, t);
            }
        }
        #[allow(unused_mut)]
        let mut exec = Exec::new(
            catalog.clone(),
            tables,
            Vec::new(),
            chrono::Utc::now(),
            self.db.name.clone(),
            self.username.clone(),
            self.db.locks.clone(),
            self.session_id,
        );
        #[cfg(feature = "vector-index")]
        {
            exec.ann = Some(self.db.ann.clone());
        }
        exec.fk_drain_deferred(checks)
    }

    /// Fire every deferred-constraint trigger queued during the transaction.
    /// Called by `COMMIT` after FK deferred checks pass, before writing to
    /// storage. Mutations produced by trigger bodies (e.g. INSERT INTO audit)
    /// are applied directly to storage via [`apply_mutations`].
    async fn fire_deferred_triggers(
        &self,
        catalog: &Catalog,
        overlay: &HashMap<String, HashMap<String, Option<Json>>>,
        truncated: &HashSet<String>,
        firings: Vec<DeferredTriggerFiring>,
    ) -> Result<()> {
        if firings.is_empty() {
            return Ok(());
        }
        // Load all tables so trigger bodies can access any table (they may
        // INSERT into audit tables, SELECT from reference tables, etc.).
        let mut tables: HashMap<QualifiedName, Arc<LoadedTable>> = HashMap::new();
        let all_table_qs: Vec<QualifiedName> = catalog
            .tables()
            .map(|t| QualifiedName {
                schema: t.schema.clone(),
                name: t.name.clone(),
            })
            .collect();
        for q in all_table_qs {
            if let Some(t) = self
                .load_table_with_overlay(catalog, &q, overlay, truncated)
                .await?
            {
                tables.insert(q, t);
            }
        }
        let mut exec = Exec::new(
            catalog.clone(),
            tables,
            Vec::new(),
            chrono::Utc::now(),
            self.db.name.clone(),
            self.username.clone(),
            self.db.locks.clone(),
            self.session_id,
        );
        #[cfg(feature = "vector-index")]
        {
            exec.ann = Some(self.db.ann.clone());
        }
        exec.fire_deferred(firings)?;
        let mutations = std::mem::take(&mut *exec.mutations.lock().unwrap());
        self.apply_mutations(mutations).await
    }

    async fn apply_mutations(&self, mutations: Vec<Mutation>) -> Result<()> {
        let watch = self.db.has_change_listeners();
        let at = chrono::Utc::now();
        let mut events = Vec::new();
        for m in mutations {
            match m {
                Mutation::Put {
                    collection,
                    row_id,
                    doc,
                } => {
                    if watch {
                        let old = self.db.storage.get(&collection, &row_id).await?;
                        push_change(&mut events, old.as_ref(), Some(&doc), at);
                    }
                    self.db.storage.put(&collection, &row_id, &doc).await?
                }
                Mutation::Delete { collection, row_id } => {
                    if watch {
                        let old = self.db.storage.get(&collection, &row_id).await?;
                        push_change(&mut events, old.as_ref(), None, at);
                    }
                    self.db.storage.delete(&collection, &row_id).await?
                }
                Mutation::Truncate { collection } => self.db.storage.truncate(&collection).await?,
            }
        }
        self.db.emit_changes(events);
        Ok(())
    }
}

/// Classify one storage write as a [`ChangeEvent`] and append it to `events`.
/// `old` is the stored document before the write (`None` when absent), `new`
/// the document being written (`None` for a physical delete). Tombstoned rows
/// (`__deleted: true`) count as absent, so a tombstoning put is a `DELETE` and
/// re-inserting over a tombstone is an `INSERT`. Documents that are not table
/// rows (no `__table` marker) produce no event.
fn push_change(
    events: &mut Vec<ChangeEvent>,
    old: Option<&Json>,
    new: Option<&Json>,
    at: chrono::DateTime<chrono::Utc>,
) {
    use crate::sql::store::{F_DELETED, F_ID, F_SCHEMA, F_TABLE};
    // A fn item (not a closure) so the input/output lifetimes elide correctly.
    fn live(doc: Option<&Json>) -> Option<&Json> {
        let doc = doc?;
        let obj = doc.as_object()?;
        obj.get(F_ID)?.as_str()?;
        if obj.get(F_DELETED).and_then(Json::as_bool).unwrap_or(false) {
            return None;
        }
        Some(doc)
    }
    let old_live = live(old);
    let new_live = live(new);
    let (op, source) = match (old_live, new_live) {
        (None, Some(n)) => (ChangeOp::Insert, n),
        (Some(_), Some(n)) => (ChangeOp::Update, n),
        (Some(o), None) => (ChangeOp::Delete, o),
        (None, None) => return,
    };
    let Some(obj) = source.as_object() else {
        return;
    };
    let Some(table) = obj.get(F_TABLE).and_then(Json::as_str) else {
        return;
    };
    let schema = obj
        .get(F_SCHEMA)
        .and_then(Json::as_str)
        .unwrap_or("public")
        .to_string();
    events.push(ChangeEvent {
        schema,
        table: table.to_string(),
        op,
        old: old_live.cloned(),
        new: new_live.cloned(),
        commit_time: at,
    });
}

/// A hand-recognized `SET CONSTRAINTS { ALL | name [, ...] } { DEFERRED |
/// IMMEDIATE }` — sqlparser 0.62 has no AST for it at all (`CONSTRAINTS`
/// isn't even one of its keywords), the same situation `ALTER EXTENSION` is
/// in (see `crate::sql::ext::alter`), so it is recognized by prefix
/// ([`is_set_constraints`]) and hand-parsed ([`parse_set_constraints`])
/// instead of going through the general parser.
struct SetConstraintsStmt {
    /// `None` = `ALL`; `Some(names)` = the explicit, possibly
    /// schema-qualified constraint name list — `(schema, name)`, each
    /// case-folded like any unquoted SQL identifier (quoted names kept
    /// verbatim).
    names: Option<Vec<(Option<String>, String)>>,
    /// `true` = `DEFERRED`, `false` = `IMMEDIATE`.
    deferred: bool,
}

/// Whether a statement's first two keywords are `SET CONSTRAINTS` (skipping
/// leading whitespace and comments), the same by-prefix recognition
/// `crate::sql::ext::alter::is_alter_extension` uses for `ALTER EXTENSION`.
fn is_set_constraints(sql: &str) -> bool {
    let words = leading_keywords(sql, 2);
    words.first().map(String::as_str) == Some("SET")
        && words.get(1).map(String::as_str) == Some("CONSTRAINTS")
}

/// A cursor over a non-whitespace token slice, for [`parse_set_constraints`].
struct SetConstraintsCursor<'a> {
    toks: &'a [sqlparser::tokenizer::Token],
    pos: usize,
}

impl<'a> SetConstraintsCursor<'a> {
    fn peek(&self) -> Option<&'a sqlparser::tokenizer::Token> {
        self.toks.get(self.pos)
    }

    fn word(&self) -> Option<&str> {
        match self.peek() {
            Some(sqlparser::tokenizer::Token::Word(w)) => Some(w.value.as_str()),
            _ => None,
        }
    }

    /// Consume a case-insensitive bare keyword, if next.
    fn eat_kw(&mut self, kw: &str) -> bool {
        if self
            .word()
            .map(|w| w.eq_ignore_ascii_case(kw))
            .unwrap_or(false)
        {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_comma(&mut self) -> bool {
        if matches!(self.peek(), Some(sqlparser::tokenizer::Token::Comma)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn eat_period(&mut self) -> bool {
        if matches!(self.peek(), Some(sqlparser::tokenizer::Token::Period)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// An identifier: unquoted words fold to lower case, quoted stay verbatim.
    fn eat_ident(&mut self, what: &str) -> Result<String> {
        match self.peek() {
            Some(sqlparser::tokenizer::Token::Word(w)) => {
                let v = if w.quote_style.is_some() {
                    w.value.clone()
                } else {
                    w.value.to_ascii_lowercase()
                };
                self.pos += 1;
                Ok(v)
            }
            other => Err(SqlError::Syntax(format!(
                "SET CONSTRAINTS: expected {what}, found {}",
                other
                    .map(|t| format!("{t:?}"))
                    .unwrap_or_else(|| "end of input".into())
            ))),
        }
    }
}

/// Hand-parse one `SET CONSTRAINTS` statement (no trailing `;`), using
/// sqlparser's own tokenizer for quote- and comment-aware identifier
/// handling (the same approach `crate::sql::ext::alter` uses a bespoke
/// lexer for; this borrows sqlparser's instead since the grammar here needs
/// nothing beyond words/commas/periods).
fn parse_set_constraints(sql: &str) -> Result<SetConstraintsStmt> {
    use sqlparser::tokenizer::{Token, Tokenizer};
    let dialect = sqlparser::dialect::PostgreSqlDialect {};
    let body = sql.trim().trim_end_matches(';');
    let tokens: Vec<Token> = Tokenizer::new(&dialect, body)
        .tokenize()
        .map_err(|e| SqlError::Syntax(format!("SET CONSTRAINTS: {e}")))?
        .into_iter()
        .filter(|t| !matches!(t, Token::Whitespace(_)))
        .collect();

    let mut c = SetConstraintsCursor {
        toks: &tokens,
        pos: 0,
    };
    if !c.eat_kw("SET") || !c.eat_kw("CONSTRAINTS") {
        return Err(SqlError::Internal(
            "parse_set_constraints called on non-SET-CONSTRAINTS input".into(),
        ));
    }
    let names = if c.eat_kw("ALL") {
        None
    } else {
        let mut names = Vec::new();
        loop {
            let first = c.eat_ident("a constraint name")?;
            let name = if c.eat_period() {
                let second = c.eat_ident("a constraint name after '.'")?;
                (Some(first), second)
            } else {
                (None, first)
            };
            names.push(name);
            if !c.eat_comma() {
                break;
            }
        }
        Some(names)
    };
    let deferred = if c.eat_kw("DEFERRED") {
        true
    } else if c.eat_kw("IMMEDIATE") {
        false
    } else {
        return Err(SqlError::Syntax(
            "SET CONSTRAINTS: expected DEFERRED or IMMEDIATE".into(),
        ));
    };
    if c.pos != tokens.len() {
        return Err(SqlError::Syntax(format!(
            "SET CONSTRAINTS: unexpected trailing input near {:?}",
            &tokens[c.pos..]
        )));
    }
    Ok(SetConstraintsStmt { names, deferred })
}

/// Recognize statements the engine deliberately does not support by their
/// leading keywords, returning the feature name for the `0A000` message.
///
/// This runs on raw statement segments *before* parsing (the same mechanism
/// that routes `ALTER EXTENSION` to its hand parser), because sqlparser 0.62
/// parses only some spellings of these statements — e.g. it rejects the
/// PostgreSQL form of `CREATE PROCEDURE` with a `42601` — and the truthfulness
/// contract requires one stable rejection code for the whole family.
fn unsupported_by_prefix(segment: &str) -> Option<&'static str> {
    let words = leading_keywords(segment, 4);
    let w = |i: usize| words.get(i).map(String::as_str).unwrap_or("");
    match (w(0), w(1)) {
        // CREATE FUNCTION and CREATE/DROP TRIGGER are implemented (see
        // `crate::sql::udf` and `crate::sql::trigger`) and parse cleanly
        // under sqlparser 0.62's PostgreSQL dialect, so they are not
        // rejected by prefix here.
        ("CREATE", "PROCEDURE") => Some("CREATE PROCEDURE"),
        ("CREATE", "OR") if w(2) == "REPLACE" && w(3) == "PROCEDURE" => Some("CREATE PROCEDURE"),
        _ => None,
    }
}

/// The first `max` bare keywords of a statement, upper-cased — skipping
/// whitespace, `--` line comments and (nested) `/* */` block comments.
/// Scanning stops at the first token that is not a bare word (a quoted
/// identifier, punctuation, ...), so only genuine leading keywords match.
fn leading_keywords(sql: &str, max: usize) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < bytes.len() && out.len() < max {
        match bytes[i] {
            c if c.is_ascii_whitespace() => i += 1,
            b'-' if bytes.get(i + 1) == Some(&b'-') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let mut depth = 1u32;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            c if c.is_ascii_alphabetic() || c == b'_' => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                out.push(sql[start..i].to_ascii_uppercase());
            }
            _ => break,
        }
    }
    out
}

/// Errors eligible for sidecar fallback-forwarding: the statement referenced
/// a function, type or relation the local engine does not have (typically
/// objects provided by a sidecar-routed extension).
fn sidecar_routable(e: &SqlError) -> bool {
    matches!(
        e,
        SqlError::UndefinedFunction(_) | SqlError::UndefinedType(_) | SqlError::UndefinedTable(_)
    )
}

/// Keep the local error (same SQLSTATE, same message) but explain why it was
/// not forwarded to the sidecar.
fn with_sidecar_txn_hint(e: SqlError) -> SqlError {
    SqlError::Sidecar {
        sqlstate: e.sqlstate().to_string(),
        message: format!(
            "{e} — hint: statements are not forwarded to the PostgreSQL sidecar inside a \
             transaction block (sidecar routing is autocommit-only)"
        ),
    }
}

/// The implicit table-level locks a statement takes, deduplicated to the
/// strongest mode per table (mirrors PostgreSQL's automatic locking).
fn table_lock_plan(stmt: &Statement, catalog: &Catalog) -> Vec<(u32, LockMode)> {
    stmt_lock_plan(stmt, catalog, true)
}

fn stmt_lock_plan(
    stmt: &Statement,
    catalog: &Catalog,
    include_function_bodies: bool,
) -> Vec<(u32, LockMode)> {
    use sqlparser::ast::{FromTable, ObjectType, TableFactor, TableObject};
    let resolve = |schema: Option<&str>, name: &str| -> Option<u32> {
        catalog
            .resolve_table_name(schema, name)
            .and_then(|q| catalog.get_table(&q).map(|t| t.oid))
    };
    let resolve_name =
        |out: &mut Vec<(u32, LockMode)>, name: &sqlparser::ast::ObjectName, mode: LockMode| {
            let (s, n) = crate::sql::names::split_schema_table(name);
            if let Some(oid) = resolve(s.as_deref(), &n) {
                out.push((oid, mode));
            }
        };
    let read_names = |out: &mut Vec<(u32, LockMode)>, names: &NameOut, mode: LockMode| {
        for (s, n) in names {
            if let Some(oid) = resolve(s.as_deref(), n) {
                out.push((oid, mode));
            }
        }
    };
    let mut plan = Vec::new();
    match stmt {
        Statement::Query(q) => {
            let mode = if q.locks.is_empty() {
                LockMode::AccessShare
            } else {
                LockMode::RowShare
            };
            let mut names = Vec::new();
            collect_query(q, &mut names);
            read_names(&mut plan, &names, mode);
        }
        Statement::Insert(i) => {
            if let TableObject::TableName(name) = &i.table {
                resolve_name(&mut plan, name, LockMode::RowExclusive);
            }
            if let Some(src) = &i.source {
                let mut names = Vec::new();
                collect_query(src, &mut names);
                read_names(&mut plan, &names, LockMode::AccessShare);
            }
        }
        Statement::Update(u) => {
            if let TableFactor::Table { name, .. } = &u.table.relation {
                resolve_name(&mut plan, name, LockMode::RowExclusive);
            }
            if let Some(sel) = &u.selection {
                let mut names = Vec::new();
                collect_expr(sel, &mut names);
                read_names(&mut plan, &names, LockMode::AccessShare);
            }
        }
        Statement::Delete(d) => {
            let items = match &d.from {
                FromTable::WithFromKeyword(items) | FromTable::WithoutKeyword(items) => items,
            };
            if let Some(twj) = items.first()
                && let TableFactor::Table { name, .. } = &twj.relation
            {
                resolve_name(&mut plan, name, LockMode::RowExclusive);
            }
            if let Some(sel) = &d.selection {
                let mut names = Vec::new();
                collect_expr(sel, &mut names);
                read_names(&mut plan, &names, LockMode::AccessShare);
            }
        }
        Statement::CreateIndex(ci) => resolve_name(&mut plan, &ci.table_name, LockMode::Share),
        Statement::AlterTable(a) => resolve_name(&mut plan, &a.name, LockMode::AccessExclusive),
        Statement::Drop {
            object_type: ObjectType::Table,
            names,
            ..
        } => {
            for name in names {
                resolve_name(&mut plan, name, LockMode::AccessExclusive);
            }
        }
        Statement::Truncate(t) => {
            for target in &t.table_names {
                resolve_name(&mut plan, &target.name, LockMode::AccessExclusive);
            }
        }
        _ => {}
    }
    // Foreign-key ripple around a DML target: referencing tables may be
    // written by referential actions (ROW EXCLUSIVE — the mode an explicit
    // UPDATE/DELETE on them takes) and parents are read for existence checks
    // (ROW SHARE, like PostgreSQL's FOR KEY SHARE probes).
    if let Some((name, include_children)) = fk_dml_target(stmt) {
        let (s, n) = crate::sql::names::split_schema_table(name);
        if let Some(q) = catalog.resolve_table_name(s.as_deref(), &n) {
            let (written, read) = crate::sql::fk::fk_ripple(catalog, &q, include_children);
            for (set, mode) in [
                (written, LockMode::RowExclusive),
                (read, LockMode::RowShare),
            ] {
                for fq in set {
                    if let Some(t) = catalog.get_table(&fq) {
                        plan.push((t.oid, mode));
                    }
                }
            }
        }
    }
    // A DML statement can fire triggers whose bodies write tables the
    // statement's own text never mentions (and PL/pgSQL function bodies can
    // do the same when called mid-statement). Mirror the preload pass's
    // coarseness (see `execute_inner`): fold in the lock plan of every
    // catalog function body's embedded statements, without recursing into
    // *their* function calls (the flat pass already covers every function).
    if include_function_bodies
        && matches!(
            stmt,
            Statement::Insert(_) | Statement::Update(_) | Statement::Delete(_)
        )
    {
        for f in catalog.functions() {
            for body_stmt in crate::sql::udf::body_statements(f) {
                plan.extend(stmt_lock_plan(&body_stmt, catalog, false));
            }
        }
    }
    // Deduplicate to the strongest mode per table (lock in oid order to reduce
    // deadlocks between statements touching the same set of tables).
    let mut by_oid: std::collections::BTreeMap<u32, LockMode> = std::collections::BTreeMap::new();
    for (oid, mode) in plan {
        let entry = by_oid.entry(oid).or_insert(mode);
        if table_mode_rank(mode) > table_mode_rank(*entry) {
            *entry = mode;
        }
    }
    by_oid.into_iter().collect()
}

/// The DML target of `stmt` plus whether foreign-key referencing tables can
/// be *written* (referential actions): UPDATE/DELETE always; INSERT only when
/// an `ON CONFLICT DO UPDATE` can rewrite referenced columns (a plain INSERT
/// only reads parents).
fn fk_dml_target(stmt: &Statement) -> Option<(&sqlparser::ast::ObjectName, bool)> {
    use sqlparser::ast::{FromTable, OnConflictAction, OnInsert, TableFactor, TableObject};
    match stmt {
        Statement::Insert(i) => {
            let upsert = matches!(
                &i.on,
                Some(OnInsert::OnConflict(oc))
                    if matches!(oc.action, OnConflictAction::DoUpdate(_))
            );
            match &i.table {
                TableObject::TableName(name) => Some((name, upsert)),
                _ => None,
            }
        }
        Statement::Update(u) => match &u.table.relation {
            TableFactor::Table { name, .. } => Some((name, true)),
            _ => None,
        },
        Statement::Delete(d) => {
            let items = match &d.from {
                FromTable::WithFromKeyword(items) | FromTable::WithoutKeyword(items) => items,
            };
            match items.first().map(|twj| &twj.relation) {
                Some(TableFactor::Table { name, .. }) => Some((name, true)),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Extra table names foreign-key enforcement may touch for `stmt`, to be
/// preloaded alongside the statement's own references.
fn fk_preload(stmt: &Statement, catalog: &Catalog) -> NameOut {
    let Some((name, include_children)) = fk_dml_target(stmt) else {
        return Vec::new();
    };
    let (schema, n) = crate::sql::names::split_schema_table(name);
    let Some(q) = catalog.resolve_table_name(schema.as_deref(), &n) else {
        return Vec::new();
    };
    let (written, read) = crate::sql::fk::fk_ripple(catalog, &q, include_children);
    written
        .into_iter()
        .chain(read)
        .map(|fq| (Some(fq.schema), fq.name))
        .collect()
}

fn table_mode_rank(mode: LockMode) -> u8 {
    match mode {
        LockMode::AccessShare => 0,
        LockMode::RowShare => 1,
        LockMode::RowExclusive => 2,
        LockMode::ShareUpdateExclusive => 3,
        LockMode::Share => 4,
        LockMode::ShareRowExclusive => 5,
        LockMode::Exclusive => 6,
        LockMode::AccessExclusive => 7,
        _ => 0,
    }
}

fn map_lock_table_mode(mode: Option<sqlparser::ast::LockTableMode>) -> LockMode {
    use sqlparser::ast::LockTableMode as M;
    match mode {
        Some(M::AccessShare) => LockMode::AccessShare,
        Some(M::RowShare) => LockMode::RowShare,
        Some(M::RowExclusive) => LockMode::RowExclusive,
        Some(M::ShareUpdateExclusive) => LockMode::ShareUpdateExclusive,
        Some(M::Share) => LockMode::Share,
        Some(M::ShareRowExclusive) => LockMode::ShareRowExclusive,
        Some(M::Exclusive) => LockMode::Exclusive,
        // PostgreSQL's default for LOCK TABLE with no mode is ACCESS EXCLUSIVE.
        Some(M::AccessExclusive) | None => LockMode::AccessExclusive,
    }
}

/// Values `SHOW` reports for standard PostgreSQL parameters we do not track
/// as session variables. Mirrors what the pgwire startup already advertises.
fn builtin_show_default(name: &str) -> Option<String> {
    match name {
        "server_version" => Some("16.0 (GuardianDB)".to_string()),
        "server_encoding" | "client_encoding" => Some("UTF8".to_string()),
        "datestyle" => Some("ISO, MDY".to_string()),
        "timezone" | "time_zone" => Some("UTC".to_string()),
        "transaction_isolation" => Some("read committed".to_string()),
        "standard_conforming_strings" => Some("on".to_string()),
        "lock_timeout" => Some("0".to_string()),
        "search_path" => Some("\"$user\", public".to_string()),
        _ => None,
    }
}

fn parse_timeout_ms(raw: &str) -> u64 {
    let raw = raw.trim();
    if let Some(num) = raw.strip_suffix("ms") {
        num.trim().parse().unwrap_or(0)
    } else if let Some(num) = raw.strip_suffix('s') {
        num.trim().parse::<u64>().map(|n| n * 1000).unwrap_or(0)
    } else {
        raw.parse().unwrap_or(0)
    }
}

/// Every table a batch of [`DeferredFkCheck`]s references — both the child
/// (declaring) table and its resolved parent — for preloading before
/// [`Session::check_deferred`].
fn deferred_check_tables(checks: &[DeferredFkCheck], catalog: &Catalog) -> Vec<QualifiedName> {
    let mut set: std::collections::BTreeSet<QualifiedName> = std::collections::BTreeSet::new();
    for c in checks {
        match c {
            DeferredFkCheck::Child { child, fk, .. } => {
                set.insert(child.clone());
                if let Some(p) = catalog.resolve_table_name(Some(&fk.ref_schema), &fk.ref_table) {
                    set.insert(p);
                }
            }
            DeferredFkCheck::Referenced { parent, child, .. } => {
                set.insert(parent.clone());
                set.insert(child.clone());
            }
            DeferredFkCheck::MatchFullNullMix { child, .. } => {
                set.insert(child.clone());
            }
        }
    }
    set.into_iter().collect()
}

fn stage_mutations(txn: &mut Transaction, mutations: Vec<Mutation>) {
    for m in mutations {
        match m {
            Mutation::Put {
                collection,
                row_id,
                doc,
            } => {
                txn.overlay
                    .entry(collection)
                    .or_default()
                    .insert(row_id, Some(doc));
            }
            Mutation::Delete { collection, row_id } => {
                txn.overlay
                    .entry(collection)
                    .or_default()
                    .insert(row_id, None);
            }
            Mutation::Truncate { collection } => {
                txn.truncated.insert(collection.clone());
                txn.overlay.remove(&collection);
            }
        }
    }
}

fn show_value(var: &str) -> String {
    match var.to_ascii_lowercase().as_str() {
        "server_version" => "15.0".into(),
        "server_version_num" => "150000".into(),
        "server_encoding" | "client_encoding" => "UTF8".into(),
        "standard_conforming_strings" | "transaction_read_only" => "on".into(),
        "search_path" => "\"$user\", public".into(),
        "timezone" | "time zone" => "UTC".into(),
        "integer_datetimes" => "on".into(),
        _ => String::new(),
    }
}

fn schema_name_to_string(name: &sqlparser::ast::SchemaName) -> String {
    use sqlparser::ast::SchemaName;
    match name {
        SchemaName::Simple(n) => crate::sql::names::split_schema_table(n).1,
        SchemaName::NamedAuthorization(n, _) => crate::sql::names::split_schema_table(n).1,
        SchemaName::UnnamedAuthorization(ident) => crate::sql::names::ident_name(ident),
    }
}

fn empty_query() -> Query {
    // A harmless `SELECT NULL WHERE false`-style placeholder is overkill; reuse a
    // parsed empty SELECT.
    let stmts = crate::sql::parser::parse_sql("SELECT 1 WHERE 1=0").unwrap();
    match stmts.into_iter().next() {
        Some(Statement::Query(q)) => *q,
        _ => unreachable!(),
    }
}

/// Count `$n` placeholders in a SQL string (ignoring those inside string literals).
fn count_placeholders(sql: &str) -> usize {
    let bytes = sql.as_bytes();
    let mut max = 0usize;
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if c == b'\'' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'\'' => in_string = true,
            b'$' => {
                let mut j = i + 1;
                let mut num = 0usize;
                let mut found = false;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    num = num * 10 + (bytes[j] - b'0') as usize;
                    j += 1;
                    found = true;
                }
                if found {
                    max = max.max(num);
                    i = j;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    max
}

// ---------------------------------------------------------------------------
// Table-reference collection (for preloading)
// ---------------------------------------------------------------------------

type NameOut = Vec<(Option<String>, String)>;

fn collect_stmt(stmt: &Statement, out: &mut NameOut) {
    match stmt {
        Statement::Query(q) => collect_query(q, out),
        Statement::Insert(i) => {
            if let sqlparser::ast::TableObject::TableName(name) = &i.table {
                push_name(name, out);
            }
            if let Some(src) = &i.source {
                collect_query(src, out);
            }
            if let Some(sqlparser::ast::OnInsert::OnConflict(oc)) = &i.on
                && let sqlparser::ast::OnConflictAction::DoUpdate(du) = &oc.action
                && let Some(sel) = &du.selection
            {
                collect_expr(sel, out);
            }
        }
        Statement::Update(u) => {
            collect_tf(&u.table.relation, out);
            for j in &u.table.joins {
                collect_tf(&j.relation, out);
            }
            for a in &u.assignments {
                collect_expr(&a.value, out);
            }
            if let Some(sel) = &u.selection {
                collect_expr(sel, out);
            }
        }
        Statement::Delete(d) => {
            match &d.from {
                sqlparser::ast::FromTable::WithFromKeyword(items)
                | sqlparser::ast::FromTable::WithoutKeyword(items) => {
                    for twj in items {
                        collect_twj(twj, out);
                    }
                }
            }
            if let Some(using) = &d.using {
                for twj in using {
                    collect_twj(twj, out);
                }
            }
            if let Some(sel) = &d.selection {
                collect_expr(sel, out);
            }
        }
        Statement::AlterTable(alter) => push_name(&alter.name, out),
        Statement::CreateIndex(ci) => push_name(&ci.table_name, out),
        // EXPLAIN executes its inner statement (SELECT-only, see dispatch);
        // preload whatever that statement touches.
        Statement::Explain { statement, .. } => collect_stmt(statement, out),
        Statement::CreateView(cv) => collect_query(&cv.query, out),
        Statement::Truncate(t) => {
            for target in &t.table_names {
                push_name(&target.name, out);
            }
        }
        _ => {}
    }
}

fn collect_query(q: &Query, out: &mut NameOut) {
    if let Some(with) = &q.with {
        for cte in &with.cte_tables {
            collect_query(&cte.query, out);
        }
    }
    collect_setexpr(&q.body, out);
}

fn collect_setexpr(s: &sqlparser::ast::SetExpr, out: &mut NameOut) {
    use sqlparser::ast::SetExpr;
    match s {
        SetExpr::Select(sel) => collect_select(sel, out),
        SetExpr::Query(q) => collect_query(q, out),
        SetExpr::SetOperation { left, right, .. } => {
            collect_setexpr(left, out);
            collect_setexpr(right, out);
        }
        SetExpr::Values(v) => {
            for row in &v.rows {
                for e in &row.content {
                    collect_expr(e, out);
                }
            }
        }
        _ => {}
    }
}

fn collect_select(sel: &sqlparser::ast::Select, out: &mut NameOut) {
    for twj in &sel.from {
        collect_twj(twj, out);
    }
    if let Some(w) = &sel.selection {
        collect_expr(w, out);
    }
    if let Some(h) = &sel.having {
        collect_expr(h, out);
    }
    for item in &sel.projection {
        if let sqlparser::ast::SelectItem::UnnamedExpr(e)
        | sqlparser::ast::SelectItem::ExprWithAlias { expr: e, .. } = item
        {
            collect_expr(e, out);
        }
    }
}

fn collect_twj(twj: &sqlparser::ast::TableWithJoins, out: &mut NameOut) {
    collect_tf(&twj.relation, out);
    for j in &twj.joins {
        collect_tf(&j.relation, out);
        if let sqlparser::ast::JoinOperator::Inner(sqlparser::ast::JoinConstraint::On(e))
        | sqlparser::ast::JoinOperator::Left(sqlparser::ast::JoinConstraint::On(e))
        | sqlparser::ast::JoinOperator::Right(sqlparser::ast::JoinConstraint::On(e))
        | sqlparser::ast::JoinOperator::FullOuter(sqlparser::ast::JoinConstraint::On(e)) =
            &j.join_operator
        {
            collect_expr(e, out);
        }
    }
}

fn collect_tf(tf: &sqlparser::ast::TableFactor, out: &mut NameOut) {
    use sqlparser::ast::TableFactor;
    match tf {
        TableFactor::Table { name, .. } => push_name(name, out),
        TableFactor::Derived { subquery, .. } => collect_query(subquery, out),
        _ => {}
    }
}

fn collect_expr(e: &sqlparser::ast::Expr, out: &mut NameOut) {
    use sqlparser::ast::Expr;
    match e {
        Expr::Subquery(q)
        | Expr::Exists { subquery: q, .. }
        | Expr::InSubquery { subquery: q, .. } => collect_query(q, out),
        Expr::BinaryOp { left, right, .. } => {
            collect_expr(left, out);
            collect_expr(right, out);
        }
        Expr::UnaryOp { expr, .. }
        | Expr::Nested(expr)
        | Expr::IsNull(expr)
        | Expr::IsNotNull(expr)
        | Expr::Cast { expr, .. } => collect_expr(expr, out),
        Expr::Between {
            expr, low, high, ..
        } => {
            collect_expr(expr, out);
            collect_expr(low, out);
            collect_expr(high, out);
        }
        Expr::InList { expr, list, .. } => {
            collect_expr(expr, out);
            for e in list {
                collect_expr(e, out);
            }
        }
        Expr::Case {
            conditions,
            else_result,
            operand,
            ..
        } => {
            if let Some(o) = operand {
                collect_expr(o, out);
            }
            for w in conditions {
                collect_expr(&w.condition, out);
                collect_expr(&w.result, out);
            }
            if let Some(e) = else_result {
                collect_expr(e, out);
            }
        }
        _ => {}
    }
}

fn push_name(name: &sqlparser::ast::ObjectName, out: &mut NameOut) {
    out.push(crate::sql::names::split_schema_table(name));
}

// Maintenance note 1: documents compatibility expectations without changing runtime behavior.

// Maintenance note 13: documents compatibility expectations without changing runtime behavior.

// Maintenance note 25: documents compatibility expectations without changing runtime behavior.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// Maintenance note: keeps SQL compatibility behavior explicit for future updates.

// SQL compatibility note 3: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 19: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 3: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.

// SQL compatibility note 19: preserves documented behavior for window functions, recursive CTE validation, SQLSTATE mapping, and aggregate correctness without changing runtime semantics.
