//! # PostgreSQL wire-protocol server
//!
//! A PostgreSQL wire-protocol server in front of the [`crate::sql`] engine
//! (enabled by the `pgwire` feature).
//!
//! It implements startup (no-auth by default), the simple and extended query
//! protocols, prepared statements, parameter binding, row descriptions, command
//! tags and SQLSTATE-tagged errors. Result columns are emitted in **text** format
//! (what node-postgres, `psql`, DBeaver and TypeORM's `pg` driver use), so a wide
//! range of standard clients connect with no custom code.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

use crate::sql::engine::{Database, Session};
use crate::sql::{ExecResult, OutField, RelationalStorage, SqlType, SqlValue};

use pgwire::api::auth::StartupHandler;
use pgwire::api::portal::Portal;
use pgwire::api::query::{ExtendedQueryHandler, SimpleQueryHandler};
use pgwire::api::results::{
    DataRowEncoder, DescribePortalResponse, DescribeStatementResponse, FieldFormat, FieldInfo,
    QueryResponse, Response, Tag,
};
use pgwire::api::stmt::{NoopQueryParser, StoredStatement};
use pgwire::api::{ClientInfo, NoopHandler, PgWireServerHandlers, Type};
use pgwire::error::{ErrorInfo, PgWireError, PgWireResult};
use pgwire::messages::data::DataRow;
use pgwire::tokio::process_socket;

/// Default bind address for the GuardianDB PostgreSQL gateway.
pub const DEFAULT_ADDR: &str = "127.0.0.1:15432";

/// Per-connection handler. Owns the connection's [`Session`] (transaction state)
/// and a small cache so a `Describe(portal)` followed by `Execute` runs the
/// statement only once.
pub struct GuardianHandler<S: RelationalStorage> {
    session: Mutex<Session<S>>,
    parser: Arc<NoopQueryParser>,
    /// Cache of statement results keyed by `sql|params`, populated by
    /// `do_describe_portal` and consumed by `do_query`.
    describe_cache: Mutex<HashMap<String, ExecResult>>,
}

impl<S: RelationalStorage> GuardianHandler<S> {
    pub fn new(db: Arc<Database<S>>, username: impl Into<String>) -> Self {
        Self {
            session: Mutex::new(Session::new(db, username)),
            parser: Arc::new(NoopQueryParser::new()),
            describe_cache: Mutex::new(HashMap::new()),
        }
    }

    async fn run_one(&self, sql: &str, params: &[SqlValue]) -> PgWireResult<ExecResult> {
        let mut session = self.session.lock().await;
        let prepared = session.prepare(sql).map_err(pg_error)?;
        session
            .execute_one(&prepared.statement, params)
            .await
            .map_err(pg_error)
    }
}

#[async_trait]
impl<S: RelationalStorage + 'static> SimpleQueryHandler for GuardianHandler<S> {
    async fn do_query<C>(&self, _client: &mut C, query: &str) -> PgWireResult<Vec<Response>>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        let mut session = self.session.lock().await;
        let results = session.execute(query).await.map_err(pg_error)?;
        Ok(results.into_iter().map(result_to_response).collect())
    }
}

#[async_trait]
impl<S: RelationalStorage + 'static> ExtendedQueryHandler for GuardianHandler<S> {
    type Statement = String;
    type QueryParser = NoopQueryParser;

    fn query_parser(&self) -> Arc<Self::QueryParser> {
        self.parser.clone()
    }

    async fn do_query<C>(
        &self,
        _client: &mut C,
        portal: &Portal<Self::Statement>,
        _max_rows: usize,
    ) -> PgWireResult<Response>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        let sql = &portal.statement.statement;
        let params = decode_params(portal)?;
        let key = cache_key(sql, &params);
        // Reuse the result computed during Describe(portal), if any.
        if let Some(result) = self.describe_cache.lock().await.remove(&key) {
            return Ok(result_to_response(result));
        }
        let result = self.run_one(sql, &params).await?;
        Ok(result_to_response(result))
    }

    async fn do_describe_statement<C>(
        &self,
        _client: &mut C,
        stmt: &StoredStatement<Self::Statement>,
    ) -> PgWireResult<DescribeStatementResponse>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        let param_types: Vec<Type> = stmt
            .parameter_types
            .iter()
            .map(|t| t.clone().unwrap_or(Type::UNKNOWN))
            .collect();
        // Parameter types are reported; result fields are described per-portal.
        Ok(DescribeStatementResponse::new(param_types, Vec::new()))
    }

    async fn do_describe_portal<C>(
        &self,
        _client: &mut C,
        portal: &Portal<Self::Statement>,
    ) -> PgWireResult<DescribePortalResponse>
    where
        C: ClientInfo + Unpin + Send + Sync,
    {
        let sql = &portal.statement.statement;
        let params = decode_params(portal)?;
        let key = cache_key(sql, &params);
        let result = self.run_one(sql, &params).await?;
        let response = match &result {
            ExecResult::Rows { fields, .. } => DescribePortalResponse::new(field_infos(fields)),
            ExecResult::Command { .. } => DescribePortalResponse::new(Vec::new()),
        };
        self.describe_cache.lock().await.insert(key, result);
        Ok(response)
    }
}

/// A per-connection factory required by `process_socket`.
struct GuardianFactory<S: RelationalStorage> {
    handler: Arc<GuardianHandler<S>>,
}

impl<S: RelationalStorage + 'static> PgWireServerHandlers for GuardianFactory<S> {
    fn simple_query_handler(&self) -> Arc<impl SimpleQueryHandler> {
        self.handler.clone()
    }

    fn extended_query_handler(&self) -> Arc<impl ExtendedQueryHandler> {
        self.handler.clone()
    }

    fn startup_handler(&self) -> Arc<impl StartupHandler> {
        Arc::new(NoopHandler)
    }
}

/// Serve the PostgreSQL wire protocol on `addr` over `db` until cancelled.
pub async fn serve<S: RelationalStorage + 'static>(
    addr: &str,
    db: Arc<Database<S>>,
    username: impl Into<String> + Clone + Send + 'static,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    serve_on(listener, db, username).await
}

/// Serve on an already-bound listener (lets the caller learn the chosen port).
pub async fn serve_on<S: RelationalStorage + 'static>(
    listener: TcpListener,
    db: Arc<Database<S>>,
    username: impl Into<String> + Clone + Send + 'static,
) -> std::io::Result<()> {
    loop {
        let (socket, _) = listener.accept().await?;
        let db = db.clone();
        let username = username.clone();
        tokio::spawn(async move {
            let handler = Arc::new(GuardianHandler::new(db, username));
            let factory = Arc::new(GuardianFactory { handler });
            if let Err(e) = process_socket(socket, None, factory).await {
                tracing::warn!("connection error: {e}");
            }
        });
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn pg_error(e: crate::sql::SqlError) -> PgWireError {
    PgWireError::UserError(Box::new(ErrorInfo::new(
        "ERROR".to_string(),
        e.sqlstate().to_string(),
        e.to_string(),
    )))
}

/// Map a [`SqlType`] to a wire-protocol [`Type`] via its OID.
pub fn pg_type(ty: &SqlType) -> Type {
    Type::from_oid(ty.oid()).unwrap_or(Type::TEXT)
}

fn field_infos(fields: &[OutField]) -> Vec<FieldInfo> {
    fields
        .iter()
        .map(|f| {
            FieldInfo::new(
                f.name.clone(),
                None,
                None,
                pg_type(&f.ty),
                FieldFormat::Text,
            )
        })
        .collect()
}

fn result_to_response(result: ExecResult) -> Response {
    match result {
        ExecResult::Rows { fields, rows } => {
            let header = Arc::new(field_infos(&fields));
            let stream_header = header.clone();
            let encoded: Vec<PgWireResult<DataRow>> = rows
                .into_iter()
                .map(move |row| -> PgWireResult<DataRow> {
                    let mut encoder = DataRowEncoder::new(stream_header.clone());
                    for value in &row {
                        encoder.encode_field(&value.to_text())?;
                    }
                    Ok(encoder.take_row())
                })
                .collect();
            Response::Query(QueryResponse::new(header, stream::iter(encoded)))
        }
        ExecResult::Command { tag } => Response::Execution(make_tag(&tag)),
    }
}

/// Build a pgwire [`Tag`] that renders exactly like a PostgreSQL command tag.
fn make_tag(tag: &str) -> Tag {
    let parts: Vec<&str> = tag.split_whitespace().collect();
    match parts.as_slice() {
        ["INSERT", oid, n] => Tag::new("INSERT")
            .with_oid(oid.parse().unwrap_or(0))
            .with_rows(n.parse().unwrap_or(0)),
        [cmd, n] if n.parse::<usize>().is_ok() => {
            Tag::new(cmd).with_rows(n.parse().unwrap_or(0))
        }
        _ => Tag::new(tag),
    }
}

fn cache_key(sql: &str, params: &[SqlValue]) -> String {
    format!("{sql}\u{1}{params:?}")
}

/// Decode bound parameters into [`SqlValue`]s. Non-primitive types arrive as text
/// and are coerced to the column type by the engine.
fn decode_params(portal: &Portal<String>) -> PgWireResult<Vec<SqlValue>> {
    let mut out = Vec::with_capacity(portal.parameter_len());
    for i in 0..portal.parameter_len() {
        let ty = portal
            .statement
            .parameter_types
            .get(i)
            .and_then(|t| t.clone())
            .unwrap_or(Type::UNKNOWN);
        let value = match ty.oid() {
            16 => portal.parameter::<bool>(i, &ty)?.map(SqlValue::Bool),
            21 => portal.parameter::<i16>(i, &ty)?.map(SqlValue::Int2),
            23 => portal.parameter::<i32>(i, &ty)?.map(SqlValue::Int4),
            20 => portal.parameter::<i64>(i, &ty)?.map(SqlValue::Int8),
            700 => portal.parameter::<f32>(i, &ty)?.map(SqlValue::Float4),
            701 => portal.parameter::<f64>(i, &ty)?.map(SqlValue::Float8),
            _ => portal.parameter::<String>(i, &ty)?.map(SqlValue::Text),
        };
        out.push(value.unwrap_or(SqlValue::Null));
    }
    Ok(out)
}
