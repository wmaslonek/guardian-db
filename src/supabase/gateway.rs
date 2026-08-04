//! The axum gateway: routing, middleware, and the shared execution helpers.
//!
//! [`build_router`] assembles the Kong-shaped routing table over an
//! [`AppState`]. Two middleware layers run: [`request_id`] ensures every
//! request/response carries an `x-request-id`, and [`require_apikey`] verifies
//! the `apikey` (and optional `Authorization: Bearer`) against the project keys,
//! resolves the effective Postgres role, and attaches an [`AuthContext`]. REST
//! and Auth handlers then open a per-request [`Session`] bound to that role.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use chrono::Utc;

use crate::relational::Catalog;
use crate::sql::engine::{Database, Session};
use crate::sql::{ExecResult, RelationalStorage, SqlError, SqlValue};
use crate::supabase::error::SupaError;
use crate::supabase::jwt::Claims;
use crate::supabase::project::{ServiceConfig, SupabaseCompatProject};

/// Shared, cheaply-cloneable gateway state (all `Arc`s).
pub struct AppState<S: RelationalStorage> {
    /// The relational database backing every request.
    pub db: Arc<Database<S>>,
    /// The single project this gateway serves.
    pub project: Arc<SupabaseCompatProject>,
    /// GoTrue-shaped service configuration.
    pub config: Arc<ServiceConfig>,
    /// One-shot guard so the `auth` schema is bootstrapped on first use.
    pub schema_ready: Arc<tokio::sync::OnceCell<()>>,
    /// One-shot guard so the `storage` schema is bootstrapped on first use.
    pub storage_ready: Arc<tokio::sync::OnceCell<()>>,
    /// One-shot guard so the `functions` schema is bootstrapped on first use.
    pub functions_ready: Arc<tokio::sync::OnceCell<()>>,
    /// Shared realtime state (the broadcast bus between websocket subscribers
    /// and the id generator for connections / bindings).
    pub realtime: Arc<crate::supabase::realtime::RealtimeShared>,
}

impl<S: RelationalStorage> Clone for AppState<S> {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            project: self.project.clone(),
            config: self.config.clone(),
            schema_ready: self.schema_ready.clone(),
            storage_ready: self.storage_ready.clone(),
            functions_ready: self.functions_ready.clone(),
            realtime: self.realtime.clone(),
        }
    }
}

impl<S: RelationalStorage> AppState<S> {
    pub fn new(
        db: Arc<Database<S>>,
        project: SupabaseCompatProject,
        config: ServiceConfig,
    ) -> Self {
        Self {
            db,
            project: Arc::new(project),
            config: Arc::new(config),
            schema_ready: Arc::new(tokio::sync::OnceCell::new()),
            storage_ready: Arc::new(tokio::sync::OnceCell::new()),
            functions_ready: Arc::new(tokio::sync::OnceCell::new()),
            realtime: Arc::new(crate::supabase::realtime::RealtimeShared::new()),
        }
    }
}

/// The resolved authentication context for a request, attached as an extension
/// by [`require_apikey`]. This is the seam an RLS-enforcement slice hooks into.
#[derive(Clone, Debug)]
pub struct AuthContext {
    /// The effective Postgres role the request runs as
    /// (`anon` / `authenticated` / `service_role`).
    pub role: String,
    /// The role carried by the `apikey` header.
    pub api_key_role: String,
    /// Verified claims from `Authorization: Bearer`, if one was supplied.
    pub claims: Option<Claims>,
    /// The request's `x-request-id`.
    pub request_id: String,
}

impl AuthContext {
    pub fn is_service_role(&self) -> bool {
        self.role == "service_role"
    }

    /// The authenticated end-user id (the bearer token's `sub`), if any.
    pub fn user_id(&self) -> Option<&str> {
        self.claims
            .as_ref()
            .and_then(|c| c.sub.as_deref())
            .filter(|s| !s.is_empty())
    }

    /// The claims document injected into the per-request session as
    /// `request.jwt.claims` (what `auth.uid()` / `auth.jwt()` and
    /// `current_setting('request.jwt.claims')` read, PostgREST-style). When no
    /// bearer token was supplied, a minimal `{"role": ...}` document is
    /// synthesized so `auth.role()` still reflects the effective role.
    pub fn claims_json(&self) -> String {
        self.claims
            .as_ref()
            .and_then(|c| serde_json::to_string(c).ok())
            .unwrap_or_else(|| serde_json::json!({ "role": self.role }).to_string())
    }
}

/// A request/response correlation id extension.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

// ---------------------------------------------------------------------------
// Router assembly
// ---------------------------------------------------------------------------

/// Build the full Kong-shaped router over `state`.
pub fn build_router<S: RelationalStorage + 'static>(state: AppState<S>) -> Router {
    let apikey_layer = axum::middleware::from_fn_with_state(state.clone(), require_apikey::<S>);

    let protected = Router::new()
        .nest("/rest/v1", crate::supabase::rest::router::<S>())
        .nest("/auth/v1", crate::supabase::auth::router::<S>())
        // pg_graphql-compatible GraphQL: apikey-verified like /rest/v1; the
        // resolved role + JWT claims govern row security inside.
        .nest("/graphql/v1", crate::supabase::graphql::router::<S>())
        // postgres-meta (Studio): apikey-verified here, service_role-gated in
        // the handlers.
        .nest("/pg-meta", crate::supabase::pg_meta::router::<S>())
        .nest("/platform/pg-meta", crate::supabase::pg_meta::router::<S>())
        // Functions: invocation + admin CRUD, both apikey-verified. Admin
        // handlers additionally require service_role.
        .nest("/functions/v1", crate::supabase::functions::router::<S>())
        .layer(apikey_layer.clone());

    // Storage: authenticated routes behind the apikey layer, plus the
    // credential-less public/signed download routes, plus a typed catch-all so
    // an unimplemented storage path never yields a bare 404.
    let storage = Router::new()
        .merge(crate::supabase::storage::protected_router::<S>().layer(apikey_layer.clone()))
        .merge(crate::supabase::storage::public_router::<S>())
        .fallback(crate::supabase::storage::unsupported_route);

    Router::new()
        .merge(protected)
        .nest("/storage/v1", storage)
        // Realtime: browsers cannot set headers on websocket connects, so the
        // apikey arrives as a query parameter, verified inside the handler.
        .nest("/realtime/v1", crate::supabase::realtime::router::<S>())
        .route("/health", any(health))
        .layer(axum::middleware::from_fn(request_id))
        .with_state(state)
}

async fn health() -> Response {
    axum::Json(serde_json::json!({"status": "ok", "service": "guardian-supabase"})).into_response()
}

// ---------------------------------------------------------------------------
// Middleware
// ---------------------------------------------------------------------------

/// Ensure an `x-request-id` exists on the request (generating one if absent) and
/// echo it on the response.
pub async fn request_id(mut req: Request, next: Next) -> Response {
    let rid = header_str(req.headers(), "x-request-id")
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    req.extensions_mut().insert(RequestId(rid.clone()));
    let mut resp = next.run(req).await;
    if let Ok(value) = rid.parse() {
        resp.headers_mut().insert("x-request-id", value);
    }
    resp
}

/// Verify the `apikey` (and optional bearer) against the project keys, resolve
/// the effective role, and attach an [`AuthContext`].
pub async fn require_apikey<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    mut req: Request,
    next: Next,
) -> Response {
    let now = Utc::now().timestamp();
    let headers = req.headers();

    // The apikey header is required; Supabase clients also accept the apikey in
    // Authorization, so fall back to a bearer token when the header is absent.
    let apikey = header_str(headers, "apikey")
        .map(str::to_string)
        .or_else(|| bearer_token(headers).map(str::to_string));
    let Some(apikey) = apikey else {
        return SupaError::MissingApiKey.into_response();
    };
    let api_claims = match state.project.keys.verify_api_key(&apikey, now) {
        Ok(c) => c,
        Err(_) => return SupaError::InvalidApiKey.into_response(),
    };
    let api_key_role = api_claims.pg_role().to_string();

    // A distinct Authorization bearer identifies the caller (anon token or a
    // real user access token). If present it must verify.
    let claims = match bearer_token(headers) {
        Some(tok) => match state.project.keys.verify_api_key(tok, now) {
            Ok(c) => Some(c),
            Err(e) => return SupaError::InvalidJwt(e).into_response(),
        },
        None => None,
    };

    // Effective role: the bearer's role wins (PostgREST semantics); otherwise the
    // apikey's role.
    let role = claims
        .as_ref()
        .map(|c| c.pg_role().to_string())
        .unwrap_or_else(|| api_key_role.clone());

    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map(|r| r.0.clone())
        .or_else(|| header_str(req.headers(), "x-request-id").map(str::to_string))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    req.extensions_mut().insert(AuthContext {
        role,
        api_key_role,
        claims,
        request_id,
    });
    next.run(req).await
}

// ---------------------------------------------------------------------------
// Header helpers
// ---------------------------------------------------------------------------

pub(crate) fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    header_str(headers, "authorization").and_then(|v| {
        v.strip_prefix("Bearer ")
            .or_else(|| {
                v.strip_prefix("bearer ")
                    .or_else(|| v.strip_prefix("BEARER "))
            })
            .map(str::trim)
    })
}

// ---------------------------------------------------------------------------
// Shared engine execution helpers
// ---------------------------------------------------------------------------

/// Load and deserialize the persisted catalog, if any. Used to coerce REST
/// filter/body values to their declared column types.
pub(crate) async fn load_catalog<S: RelationalStorage>(
    db: &Database<S>,
) -> Result<Option<Catalog>, SupaError> {
    match db.storage().load_catalog().await {
        Ok(Some(json)) => serde_json::from_value(json)
            .map(Some)
            .map_err(|e| SupaError::Internal(format!("corrupt catalog: {e}"))),
        Ok(None) => Ok(None),
        Err(e) => Err(SupaError::Internal(format!("catalog load failed: {e}"))),
    }
}

/// Run a single parameterised statement as `role`, returning its result.
/// No JWT claims are injected — used for internal (service_role) work, which
/// bypasses row security anyway.
pub(crate) async fn run_sql<S: RelationalStorage + 'static>(
    db: &Arc<Database<S>>,
    role: &str,
    sql: &str,
    params: Vec<SqlValue>,
) -> Result<ExecResult, SqlError> {
    let mut session = Session::new(db.clone(), role.to_string());
    let prepared = session.prepare(sql)?;
    session.execute_one(&prepared.statement, &params).await
}

/// Run a single parameterised statement in a session bound to the request's
/// resolved role, with the request's JWT claims installed as
/// `request.jwt.claims` — the seam that makes row-security policies
/// (`auth.uid()`, `current_setting('request.jwt.claims')`) see the caller.
pub(crate) async fn run_sql_as<S: RelationalStorage + 'static>(
    db: &Arc<Database<S>>,
    auth: &AuthContext,
    sql: &str,
    params: Vec<SqlValue>,
) -> Result<ExecResult, SqlError> {
    let mut session = Session::new(db.clone(), auth.role.clone());
    session.set_var("request.jwt.claims", &auth.claims_json());
    let prepared = session.prepare(sql)?;
    session.execute_one(&prepared.statement, &params).await
}

/// Run a multi-statement SQL batch as `role` (no params). Used for DDL
/// bootstrap where several statements execute in order.
pub(crate) async fn run_batch<S: RelationalStorage + 'static>(
    db: &Arc<Database<S>>,
    role: &str,
    sql: &str,
) -> Result<Vec<ExecResult>, SqlError> {
    let mut session = Session::new(db.clone(), role.to_string());
    session.execute(sql).await
}
