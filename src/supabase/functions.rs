//! Supabase Edge Functions-compatible service on top of Guardian Compute.
//!
//! Speaks the two halves of Supabase's Functions surface:
//!
//! * `/functions/v1/{slug}` — the **invocation** endpoint every Supabase client
//!   library posts to. Verifies the caller (default `verify_jwt=true`, matching
//!   the CLI), fetches the deployed WebAssembly body for `slug`, and runs it
//!   inside the Guardian Compute WASM sandbox with the caller's request
//!   folded into an opaque input envelope. The guest returns an output
//!   envelope (status + headers + body) which is rendered as the HTTP
//!   response. Supports `GET`, `POST`, `PUT`, `PATCH`, `DELETE` and the CORS
//!   `OPTIONS` preflight (204).
//!
//! * `/functions/v1/_admin/*` — the **management** endpoints Supabase's CLI
//!   (`supabase functions deploy|list|delete`) and Studio talk to. All admin
//!   routes require `service_role`; every mutation goes through parameterised
//!   SQL against the `functions` schema (bootstrapped on first use).
//!
//! ## Runtime substrate
//!
//! Function bodies are WebAssembly modules using the Guardian Compute ABI
//! (`gdb_alloc` + a `handler` export with signature `(ptr, len) -> i64` where
//! the return packs `(out_ptr << 32) | out_len`). Input is a CBOR-encoded
//! [`InvocationInput`]; output is a CBOR-encoded [`InvocationOutput`]. Every
//! run gets a fresh `Store` and a hard resource ceiling — the same isolation
//! [`crate::compute::WasmRuntime`] enforces for delegated tasks. The only
//! host capability linked in by default is `gdb.log`, so a function can
//! `console.log`-equivalently emit into GuardianDB's tracing without any
//! filesystem, network, or environment access.
//!
//! When a project sets `verify_jwt=false` (the CLI's `--no-verify-jwt`), the
//! bearer/apikey is still required for admission but a missing `Authorization`
//! is allowed. When it is `true` (the default), the caller MUST supply an
//! `apikey` header AND a matching `Authorization: Bearer` token, and both
//! must verify against the project keys — a mismatch renders the same
//! `SUPA_COMPAT_INVALID_JWT` shape as `/rest/v1`.
//!
//! ## Storage model
//!
//! The `functions` schema (see [`BOOTSTRAP_SQL`]) holds three tables:
//!
//! * `functions.functions` — one row per deployed function (slug, name,
//!   verify_jwt, version, timestamps).
//! * `functions._code` — the WASM body, keyed by function id (`bytea`).
//! * `functions.secrets` — per-function environment variables (name/value),
//!   exposed to the guest through the input envelope's `env` map.
//!
//! All three tables have RLS enabled with **no default policies** — only
//! `service_role` reaches them, exactly like `storage.buckets`.
//!
//! ## Error shape
//!
//! Errors match Supabase's Functions runtime shape:
//! `{"error": <code>, "msg": <message>}`. The full taxonomy lives in
//! [`functions_error`] / [`FunctionsErrorCode`]. Gateway-level errors from
//! outside this module (missing apikey, invalid JWT) still render in the
//! shared `{code, message}` shape — that is the caller's contract with the
//! auth layer, unchanged here.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, RawQuery, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, delete, get};
use axum::{Extension, Json as AxumJson};
use base64::Engine as _;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};
use uuid::Uuid;

use crate::sql::{ExecResult, RelationalStorage, SqlValue};
use crate::supabase::error::{SupaError, status_for_sqlstate};
use crate::supabase::gateway::{AppState, AuthContext, run_batch, run_sql_as};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum accepted request body for a function invocation (bytes).
/// Deliberately conservative — function inputs travel through the SQL layer's
/// JSON envelope, and huge payloads should go through Storage instead.
pub const MAX_INVOKE_BODY_BYTES: usize = 6 * 1024 * 1024;

/// Maximum accepted WebAssembly bundle size when deploying (bytes).
pub const MAX_WASM_BUNDLE_BYTES: usize = 50 * 1024 * 1024;

/// The exported guest function every deployed WASM module must implement.
pub const HANDLER_EXPORT: &str = "handler";

/// Wall-clock ceiling for a single invocation, in milliseconds. Matches
/// Supabase's default Edge Functions timeout.
pub const DEFAULT_TIMEOUT_MS: u64 = 60_000;

/// Ceiling on the wasm module's linear memory for a single invocation.
pub const DEFAULT_MEMORY_BYTES: u64 = 128 * 1024 * 1024;

/// Ceiling on wasm fuel for a single invocation.
pub const DEFAULT_FUEL: u64 = 5_000_000_000;

// ---------------------------------------------------------------------------
// Schema bootstrap
// ---------------------------------------------------------------------------

/// The `functions` schema bootstrap. RLS is enabled with no default policies
/// so only `service_role` bypasses.
pub const BOOTSTRAP_SQL: &str = "
CREATE SCHEMA IF NOT EXISTS functions;

CREATE TABLE IF NOT EXISTS functions.functions (
    id uuid PRIMARY KEY,
    slug text NOT NULL UNIQUE,
    name text NOT NULL,
    verify_jwt boolean NOT NULL,
    version bigint NOT NULL,
    status text NOT NULL,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE TABLE IF NOT EXISTS functions._code (
    function_id uuid PRIMARY KEY,
    body bytea NOT NULL,
    content_sha256 text NOT NULL,
    size_bytes bigint NOT NULL,
    updated_at timestamptz NOT NULL
);

CREATE TABLE IF NOT EXISTS functions.secrets (
    function_id uuid NOT NULL,
    name text NOT NULL,
    value text NOT NULL,
    updated_at timestamptz NOT NULL,
    PRIMARY KEY (function_id, name)
);

ALTER TABLE functions.functions ENABLE ROW LEVEL SECURITY;
ALTER TABLE functions._code ENABLE ROW LEVEL SECURITY;
ALTER TABLE functions.secrets ENABLE ROW LEVEL SECURITY;
";

/// The columns projected for every function metadata read.
const FUNCTION_COLUMNS: &str =
    "id, slug, name, verify_jwt, version, status, created_at, updated_at";

/// Bootstrap the `functions` schema exactly once per gateway instance.
pub async fn ensure_schema<S: RelationalStorage + 'static>(
    state: &AppState<S>,
) -> Result<(), SupaError> {
    state
        .functions_ready
        .get_or_try_init(|| async {
            run_batch(&state.db, "service_role", BOOTSTRAP_SQL)
                .await
                .map_err(SupaError::Sql)?;
            Ok::<(), SupaError>(())
        })
        .await
        .map(|_| ())
}

// ---------------------------------------------------------------------------
// Error rendering
// ---------------------------------------------------------------------------

/// The stable error codes the functions surface can surface. The wire body
/// is always `{"error": <code>, "msg": <message>}` at the associated HTTP
/// status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionsErrorCode {
    /// The function slug is not deployed.
    NotFound,
    /// The function's WASM body failed to compile or instantiate.
    BootError,
    /// The function trapped, exceeded fuel, memory, or deadline, or violated
    /// the ABI at runtime.
    RuntimeError,
    /// The invocation input was too large or malformed.
    BadRequest,
    /// The service role is required (admin routes).
    Forbidden,
    /// The deployed body was not a valid WebAssembly module.
    InvalidWasm,
    /// An internal gateway failure (never contains a secret).
    Internal,
}

impl FunctionsErrorCode {
    fn status(self) -> StatusCode {
        match self {
            FunctionsErrorCode::NotFound => StatusCode::NOT_FOUND,
            FunctionsErrorCode::BootError => StatusCode::INTERNAL_SERVER_ERROR,
            FunctionsErrorCode::RuntimeError => StatusCode::INTERNAL_SERVER_ERROR,
            FunctionsErrorCode::BadRequest => StatusCode::BAD_REQUEST,
            FunctionsErrorCode::Forbidden => StatusCode::FORBIDDEN,
            FunctionsErrorCode::InvalidWasm => StatusCode::BAD_REQUEST,
            FunctionsErrorCode::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(self) -> &'static str {
        match self {
            FunctionsErrorCode::NotFound => "SUPA_COMPAT_FUNCTION_NOT_FOUND",
            FunctionsErrorCode::BootError => "SUPA_COMPAT_FUNCTION_BOOT_ERROR",
            FunctionsErrorCode::RuntimeError => "SUPA_COMPAT_FUNCTION_RUNTIME_ERROR",
            FunctionsErrorCode::BadRequest => "SUPA_COMPAT_FUNCTION_BAD_REQUEST",
            FunctionsErrorCode::Forbidden => "SUPA_COMPAT_FUNCTION_FORBIDDEN",
            FunctionsErrorCode::InvalidWasm => "SUPA_COMPAT_FUNCTION_INVALID_WASM",
            FunctionsErrorCode::Internal => "SUPA_COMPAT_FUNCTION_INTERNAL",
        }
    }
}

/// Render a functions-shaped error: `{"error": <code>, "msg": <message>}`.
pub fn functions_error(code: FunctionsErrorCode, msg: impl Into<String>) -> Response {
    let msg = msg.into();
    (
        code.status(),
        AxumJson(json!({
            "error": code.code(),
            "msg": msg,
        })),
    )
        .into_response()
}

/// Convert an infrastructure error to the functions error shape.
fn functions_error_from(e: SupaError) -> Response {
    if let SupaError::Sql(err) = &e {
        let state = err.sqlstate();
        return functions_error(
            match status_for_sqlstate(state) {
                StatusCode::NOT_FOUND => FunctionsErrorCode::NotFound,
                StatusCode::FORBIDDEN => FunctionsErrorCode::Forbidden,
                StatusCode::BAD_REQUEST => FunctionsErrorCode::BadRequest,
                _ => FunctionsErrorCode::Internal,
            },
            err.to_string(),
        );
    }
    match e {
        SupaError::Forbidden(what) => functions_error(
            FunctionsErrorCode::Forbidden,
            format!("{what} requires the service_role key"),
        ),
        SupaError::BadRequest(m) => functions_error(FunctionsErrorCode::BadRequest, m),
        SupaError::Internal(m) => {
            if let Some(rest) = m.strip_prefix("__functions_boot::") {
                functions_error(FunctionsErrorCode::BootError, rest)
            } else if let Some(rest) = m.strip_prefix("__functions_runtime::") {
                functions_error(FunctionsErrorCode::RuntimeError, rest)
            } else {
                functions_error(FunctionsErrorCode::Internal, m)
            }
        }
        other => other.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

/// The invocation router mounted at `/functions/v1` **behind** the apikey
/// middleware. Both admin (`/_admin/...`) and invocation (`/{slug}`) routes
/// live here so a single apikey layer covers the whole namespace.
pub fn router<S: RelationalStorage + 'static>() -> Router<AppState<S>> {
    Router::new()
        // Admin: service_role-only CRUD.
        .route(
            "/_admin/functions",
            get(list_functions::<S>).post(create_function::<S>),
        )
        .route(
            "/_admin/functions/{slug}",
            get(get_function::<S>)
                .patch(update_function::<S>)
                .delete(delete_function::<S>),
        )
        .route(
            "/_admin/functions/{slug}/body",
            get(get_function_body::<S>).put(put_function_body::<S>),
        )
        .route(
            "/_admin/functions/{slug}/secrets",
            get(list_secrets::<S>)
                .post(upsert_secret::<S>)
                .delete(delete_all_secrets::<S>),
        )
        .route(
            "/_admin/functions/{slug}/secrets/{name}",
            delete(delete_secret::<S>),
        )
        // Invocation: OPTIONS is unauthenticated CORS preflight, all others
        // are authenticated per verify_jwt.
        .route("/{slug}", any(invoke_function::<S>))
        .layer(DefaultBodyLimit::max(MAX_INVOKE_BODY_BYTES))
}

// ---------------------------------------------------------------------------
// Admin: CRUD on functions.functions
// ---------------------------------------------------------------------------

/// The JSON shape for creating a function.
#[derive(Debug, Clone, Deserialize)]
struct CreateFunctionBody {
    slug: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default = "default_verify_jwt")]
    verify_jwt: bool,
    /// Base64-encoded `.wasm` bytes. Optional at create time so a slug can
    /// be reserved and the body PUT separately (matches Supabase's CLI flow).
    #[serde(default)]
    body: Option<String>,
}

fn default_verify_jwt() -> bool {
    true
}

/// The JSON shape for updating a function.
#[derive(Debug, Clone, Deserialize)]
struct UpdateFunctionBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    verify_jwt: Option<bool>,
    #[serde(default)]
    body: Option<String>,
}

async fn list_functions<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        let result = run_sql_as(
            &state.db,
            &auth,
            &format!("SELECT {FUNCTION_COLUMNS} FROM functions.functions ORDER BY slug"),
            vec![],
        )
        .await
        .map_err(SupaError::Sql)?;
        Ok(AxumJson(Json::Array(result_objects(result)?)).into_response())
    })
    .await
}

async fn create_function<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    body: Bytes,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        let req: CreateFunctionBody = serde_json::from_slice(&body)
            .map_err(|e| SupaError::BadRequest(format!("invalid create body: {e}")))?;
        validate_slug(&req.slug)?;

        let wasm = match req.body.as_deref() {
            Some(b64) => Some(decode_wasm(b64)?),
            None => None,
        };

        let id = Uuid::new_v4();
        let now = Utc::now();
        let name = req.name.unwrap_or_else(|| req.slug.clone());
        let status = if wasm.is_some() { "ACTIVE" } else { "PENDING" };

        let insert = run_sql_as(
            &state.db,
            &auth,
            "INSERT INTO functions.functions \
             (id, slug, name, verify_jwt, version, status, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, 1, $5, $6, $6)",
            vec![
                SqlValue::Uuid(id),
                SqlValue::Text(req.slug.clone()),
                SqlValue::Text(name),
                SqlValue::Bool(req.verify_jwt),
                SqlValue::Text(status.to_string()),
                SqlValue::Timestamptz(now),
            ],
        )
        .await;
        if let Err(e) = insert {
            if e.sqlstate() == "23505" {
                return Ok(functions_error(
                    FunctionsErrorCode::BadRequest,
                    format!("function slug \"{}\" already exists", req.slug),
                ));
            }
            return Err(SupaError::Sql(e));
        }

        if let Some(wasm) = wasm {
            insert_code(&state, &auth, id, &wasm, now).await?;
        }

        let row = fetch_one_function(&state, &auth, &req.slug).await?;
        Ok((StatusCode::CREATED, AxumJson(row)).into_response())
    })
    .await
}

async fn get_function<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    Path(slug): Path<String>,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        match load_function_row(&state, &auth, &slug).await? {
            Some(row) => Ok(AxumJson(row).into_response()),
            None => Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" not found"),
            )),
        }
    })
    .await
}

async fn update_function<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    Path(slug): Path<String>,
    body: Bytes,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        let req: UpdateFunctionBody = serde_json::from_slice(&body)
            .map_err(|e| SupaError::BadRequest(format!("invalid update body: {e}")))?;

        let Some(existing) = load_function_row(&state, &auth, &slug).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" not found"),
            ));
        };
        let id = extract_id(&existing)?;
        let now = Utc::now();
        let mut sets = vec!["updated_at = $1".to_string()];
        let mut params: Vec<SqlValue> = vec![SqlValue::Timestamptz(now)];
        if let Some(name) = req.name {
            params.push(SqlValue::Text(name));
            sets.push(format!("name = ${}", params.len()));
        }
        if let Some(verify) = req.verify_jwt {
            params.push(SqlValue::Bool(verify));
            sets.push(format!("verify_jwt = ${}", params.len()));
        }
        let bumps_version = req.body.is_some();
        if bumps_version {
            sets.push("version = version + 1".to_string());
            sets.push("status = 'ACTIVE'".to_string());
        }
        params.push(SqlValue::Text(slug.clone()));
        let sql = format!(
            "UPDATE functions.functions SET {} WHERE slug = ${}",
            sets.join(", "),
            params.len()
        );
        run_sql_as(&state.db, &auth, &sql, params)
            .await
            .map_err(SupaError::Sql)?;

        if let Some(b64) = req.body {
            let wasm = decode_wasm(&b64)?;
            replace_code(&state, &auth, id, &wasm, now).await?;
        }

        let row = fetch_one_function(&state, &auth, &slug).await?;
        Ok(AxumJson(row).into_response())
    })
    .await
}

async fn delete_function<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    Path(slug): Path<String>,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        let Some(row) = load_function_row(&state, &auth, &slug).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" not found"),
            ));
        };
        let id = extract_id(&row)?;
        // Cascade by hand — the engine's foreign keys are optional and the
        // storage.rs pattern (delete code + secrets + metadata) is what we
        // mirror here.
        run_sql_as(
            &state.db,
            &auth,
            "DELETE FROM functions._code WHERE function_id = $1",
            vec![SqlValue::Uuid(id)],
        )
        .await
        .map_err(SupaError::Sql)?;
        run_sql_as(
            &state.db,
            &auth,
            "DELETE FROM functions.secrets WHERE function_id = $1",
            vec![SqlValue::Uuid(id)],
        )
        .await
        .map_err(SupaError::Sql)?;
        run_sql_as(
            &state.db,
            &auth,
            "DELETE FROM functions.functions WHERE slug = $1",
            vec![SqlValue::Text(slug)],
        )
        .await
        .map_err(SupaError::Sql)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

async fn get_function_body<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    Path(slug): Path<String>,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        let Some(row) = load_function_row(&state, &auth, &slug).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" not found"),
            ));
        };
        let id = extract_id(&row)?;
        let Some(bytes) = load_code(&state, &auth, id).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" has no deployed body"),
            ));
        };
        Ok((
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/wasm")],
            bytes,
        )
            .into_response())
    })
    .await
}

async fn put_function_body<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    Path(slug): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        let Some(row) = load_function_row(&state, &auth, &slug).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" not found"),
            ));
        };
        let id = extract_id(&row)?;
        // Accept either raw application/wasm bytes or base64 in a text body.
        let content_type = headers
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let wasm = if content_type.starts_with("application/wasm")
            || content_type.starts_with("application/octet-stream")
        {
            body.to_vec()
        } else {
            let text = std::str::from_utf8(&body).map_err(|_| {
                SupaError::BadRequest("body must be application/wasm or base64 text".into())
            })?;
            decode_wasm(text.trim())?
        };
        if wasm.len() > MAX_WASM_BUNDLE_BYTES {
            return Ok(functions_error(
                FunctionsErrorCode::BadRequest,
                format!(
                    "wasm body exceeds {MAX_WASM_BUNDLE_BYTES} bytes ({} bytes)",
                    wasm.len()
                ),
            ));
        }
        validate_wasm_magic(&wasm)?;
        let now = Utc::now();
        replace_code(&state, &auth, id, &wasm, now).await?;
        run_sql_as(
            &state.db,
            &auth,
            "UPDATE functions.functions \
             SET version = version + 1, status = 'ACTIVE', updated_at = $1 \
             WHERE slug = $2",
            vec![SqlValue::Timestamptz(now), SqlValue::Text(slug.clone())],
        )
        .await
        .map_err(SupaError::Sql)?;
        let row = fetch_one_function(&state, &auth, &slug).await?;
        Ok(AxumJson(row).into_response())
    })
    .await
}

// ---------------------------------------------------------------------------
// Admin: secrets
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct UpsertSecretBody {
    name: String,
    value: String,
}

async fn list_secrets<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    Path(slug): Path<String>,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        let Some(row) = load_function_row(&state, &auth, &slug).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" not found"),
            ));
        };
        let id = extract_id(&row)?;
        // Never surface the value; return only names + timestamps.
        let result = run_sql_as(
            &state.db,
            &auth,
            "SELECT name, updated_at FROM functions.secrets \
             WHERE function_id = $1 ORDER BY name",
            vec![SqlValue::Uuid(id)],
        )
        .await
        .map_err(SupaError::Sql)?;
        Ok(AxumJson(Json::Array(result_objects(result)?)).into_response())
    })
    .await
}

async fn upsert_secret<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    Path(slug): Path<String>,
    body: Bytes,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        let req: UpsertSecretBody = serde_json::from_slice(&body)
            .map_err(|e| SupaError::BadRequest(format!("invalid secret body: {e}")))?;
        validate_secret_name(&req.name)?;
        let Some(row) = load_function_row(&state, &auth, &slug).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" not found"),
            ));
        };
        let id = extract_id(&row)?;
        let now = Utc::now();
        // Upsert without ON CONFLICT (portable across engine flavours): DELETE
        // then INSERT, both parameterised.
        run_sql_as(
            &state.db,
            &auth,
            "DELETE FROM functions.secrets WHERE function_id = $1 AND name = $2",
            vec![SqlValue::Uuid(id), SqlValue::Text(req.name.clone())],
        )
        .await
        .map_err(SupaError::Sql)?;
        run_sql_as(
            &state.db,
            &auth,
            "INSERT INTO functions.secrets (function_id, name, value, updated_at) \
             VALUES ($1, $2, $3, $4)",
            vec![
                SqlValue::Uuid(id),
                SqlValue::Text(req.name.clone()),
                SqlValue::Text(req.value),
                SqlValue::Timestamptz(now),
            ],
        )
        .await
        .map_err(SupaError::Sql)?;
        Ok(AxumJson(json!({ "name": req.name, "updated_at": now.to_rfc3339() })).into_response())
    })
    .await
}

async fn delete_secret<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    Path((slug, name)): Path<(String, String)>,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        let Some(row) = load_function_row(&state, &auth, &slug).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" not found"),
            ));
        };
        let id = extract_id(&row)?;
        run_sql_as(
            &state.db,
            &auth,
            "DELETE FROM functions.secrets WHERE function_id = $1 AND name = $2",
            vec![SqlValue::Uuid(id), SqlValue::Text(name)],
        )
        .await
        .map_err(SupaError::Sql)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

async fn delete_all_secrets<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    Path(slug): Path<String>,
) -> Response {
    run(async {
        require_service_role(&auth)?;
        ensure_schema(&state).await?;
        let Some(row) = load_function_row(&state, &auth, &slug).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" not found"),
            ));
        };
        let id = extract_id(&row)?;
        run_sql_as(
            &state.db,
            &auth,
            "DELETE FROM functions.secrets WHERE function_id = $1",
            vec![SqlValue::Uuid(id)],
        )
        .await
        .map_err(SupaError::Sql)?;
        Ok(StatusCode::NO_CONTENT.into_response())
    })
    .await
}

// ---------------------------------------------------------------------------
// Invocation
// ---------------------------------------------------------------------------

/// The JSON envelope handed to the guest as CBOR input.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationInput {
    pub method: String,
    pub url: String,
    pub path: String,
    pub query: String,
    pub headers: Vec<(String, String)>,
    /// Base64-encoded request body (may be empty).
    #[serde(default)]
    pub body_b64: String,
    /// Per-function env (name/value pairs from `functions.secrets`).
    #[serde(default)]
    pub env: Vec<(String, String)>,
    /// The caller's resolved role (`anon` / `authenticated` / `service_role`).
    pub role: String,
    /// The caller's `x-request-id`.
    pub request_id: String,
    /// The raw bearer JWT the caller supplied (when verify_jwt=true this is
    /// verified). May be empty when verify_jwt=false and no bearer was sent.
    #[serde(default)]
    pub jwt: String,
}

/// The JSON envelope the guest returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationOutput {
    #[serde(default = "default_status")]
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Base64-encoded response body.
    #[serde(default)]
    pub body_b64: String,
}

fn default_status() -> u16 {
    200
}

async fn invoke_function<S: RelationalStorage + 'static>(
    State(state): State<AppState<S>>,
    Extension(auth): Extension<AuthContext>,
    Path(slug): Path<String>,
    method: Method,
    headers: HeaderMap,
    RawQuery(query): RawQuery,
    body: Bytes,
) -> Response {
    // CORS preflight is answered without touching the DB.
    if method == Method::OPTIONS {
        return cors_preflight_response(&headers);
    }
    run(async {
        ensure_schema(&state).await?;

        // Routing metadata lookup runs as service_role: an anon caller cannot
        // read `functions.functions` under the schema's default-deny RLS, and
        // whether a slug is deployed is not a user-visible fact — it is a
        // routing decision equivalent to the storage-api's `_blobs` fetch.
        let admin_auth = as_service_role(&auth);
        let Some(row) = load_function_row(&state, &admin_auth, &slug).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" not found"),
            ));
        };
        let verify_jwt = extract_bool(&row, "verify_jwt").unwrap_or(true);
        if verify_jwt && auth.claims.is_none() {
            return Ok(functions_error(
                FunctionsErrorCode::Forbidden,
                "this function requires a bearer token (verify_jwt=true)",
            ));
        }
        let id = extract_id(&row)?;
        let Some(wasm) = load_code(&state, &admin_auth, id).await? else {
            return Ok(functions_error(
                FunctionsErrorCode::NotFound,
                format!("function \"{slug}\" has no deployed body"),
            ));
        };
        let secrets = load_secrets(&state, &admin_auth, id).await?;

        let query_str = query.unwrap_or_default();
        let path = format!("/functions/v1/{slug}");
        let url = format!(
            "{}{}{}",
            state.project.api_url.trim_end_matches('/'),
            path,
            if query_str.is_empty() {
                String::new()
            } else {
                format!("?{query_str}")
            },
        );

        let mut header_pairs = Vec::with_capacity(headers.len());
        for (k, v) in headers.iter() {
            if let Ok(s) = v.to_str() {
                header_pairs.push((k.as_str().to_string(), s.to_string()));
            }
        }

        let jwt = auth
            .claims
            .as_ref()
            .and_then(|_| bearer_from(&headers))
            .unwrap_or_default();

        let input = InvocationInput {
            method: method.to_string(),
            url,
            path,
            query: query_str,
            headers: header_pairs,
            body_b64: base64::engine::general_purpose::STANDARD.encode(&body),
            env: secrets,
            role: auth.role.clone(),
            request_id: auth.request_id.clone(),
            jwt,
        };

        let output = run_guest(&wasm, &input).await?;

        render_output(output)
    })
    .await
}

/// The result of running a guest, before it becomes an HTTP response.
async fn run_guest(
    wasm: &[u8],
    input: &InvocationInput,
) -> Result<InvocationOutput, SupaError> {
    let input_bytes = serde_cbor_to_bytes(input)?;
    let wasm = wasm.to_vec();
    // The guardian compute runtime is synchronous and CPU-bound; run it in
    // spawn_blocking so we do not stall the tokio scheduler.
    let joined = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, GuestError> {
        run_guest_blocking(&wasm, HANDLER_EXPORT, &input_bytes)
    })
    .await
    .map_err(|e| SupaError::Internal(format!("guest runner join error: {e}")))?;

    let bytes = match joined {
        Ok(bytes) => bytes,
        Err(GuestError::Boot(msg)) => {
            return Err(SupaError::Internal(format!("__functions_boot::{msg}")));
        }
        Err(GuestError::Runtime(msg)) => {
            return Err(SupaError::Internal(format!("__functions_runtime::{msg}")));
        }
    };

    let out: InvocationOutput = serde_cbor_from_bytes(&bytes).map_err(|e| {
        SupaError::Internal(format!("__functions_runtime::invalid output: {e}"))
    })?;
    Ok(out)
}

/// Errors distinguishing boot-time (compile/instantiate/missing export) from
/// runtime (trap/deadline/fuel/etc.), so the invocation surface can render
/// the correct `SUPA_COMPAT_FUNCTION_*` code.
#[derive(Debug)]
#[allow(dead_code)]
enum GuestError {
    Boot(String),
    Runtime(String),
}

#[cfg(feature = "compute")]
fn run_guest_blocking(
    wasm: &[u8],
    entrypoint: &str,
    input: &[u8],
) -> Result<Vec<u8>, GuestError> {
    use crate::compute::runtime::{HostGrants, WasmRuntime};
    use crate::compute::ResourceLimits;
    use crate::compute::runtime::TaskError;

    let runtime = WasmRuntime::new().map_err(|e| GuestError::Boot(e.to_string()))?;
    let task = runtime
        .compile(wasm)
        .map_err(|e| GuestError::Boot(e.to_string()))?;
    let limits = ResourceLimits {
        max_memory_bytes: DEFAULT_MEMORY_BYTES,
        fuel: DEFAULT_FUEL,
        timeout_ms: DEFAULT_TIMEOUT_MS,
    };
    let mut grants = HostGrants::default();
    grants.log = true;
    let result = runtime
        .execute_with_host(&task, entrypoint, input, &limits, &grants)
        .map_err(|e| match e {
            TaskError::InvalidModule(m) => GuestError::Boot(format!("invalid module: {m}")),
            TaskError::MissingExport(m) => GuestError::Boot(format!("missing export: {m}")),
            TaskError::HostCapabilityDenied(m) => {
                GuestError::Boot(format!("host capability denied: {m}"))
            }
            TaskError::FuelExhausted => GuestError::Runtime("fuel exhausted".into()),
            TaskError::DeadlineExceeded => GuestError::Runtime("wall-clock deadline exceeded".into()),
            TaskError::MemoryLimitExceeded => GuestError::Runtime("memory limit exceeded".into()),
            TaskError::AbiViolation(m) => GuestError::Runtime(format!("abi violation: {m}")),
            TaskError::Trapped(m) => GuestError::Runtime(format!("trap: {m}")),
            TaskError::WasmUnavailable(m) => GuestError::Boot(format!("wasm unavailable: {m}")),
            TaskError::Runtime(m) => GuestError::Runtime(m),
        })?;
    let _ = &result.metrics;
    Ok(result.output)
}

/// Fallback runner when the `compute` feature is not enabled: no WASM sandbox
/// is available. This preserves the module's compile-shape while making it
/// clear the runtime substrate is off.
#[cfg(not(feature = "compute"))]
fn run_guest_blocking(
    _wasm: &[u8],
    _entrypoint: &str,
    _input: &[u8],
) -> Result<Vec<u8>, GuestError> {
    Err(GuestError::Boot(
        "the `compute` feature must be enabled to invoke functions".into(),
    ))
}

fn render_output(output: InvocationOutput) -> Result<Response, SupaError> {
    let status = StatusCode::from_u16(output.status)
        .map_err(|_| SupaError::Internal(format!("_functions_runtime::bad status {}", output.status)))?;
    let body_bytes = if output.body_b64.is_empty() {
        Vec::new()
    } else {
        base64::engine::general_purpose::STANDARD
            .decode(output.body_b64.as_bytes())
            .map_err(|e| SupaError::Internal(format!("_functions_runtime::bad body base64: {e}")))?
    };
    let mut response = (status, body_bytes).into_response();
    for (k, v) in output.headers {
        let (Ok(name), Ok(value)) = (HeaderName::try_from(k), HeaderValue::try_from(v)) else {
            continue;
        };
        response.headers_mut().insert(name, value);
    }
    Ok(response)
}

/// The standard Supabase Functions CORS preflight response — 204 with the
/// echo of the caller's request headers. Matches how the deno-runtime CLI
/// answers OPTIONS before dispatching to user code.
fn cors_preflight_response(req_headers: &HeaderMap) -> Response {
    let allow_headers = req_headers
        .get("access-control-request-headers")
        .and_then(|v| v.to_str().ok())
        .unwrap_or(
            "authorization, x-client-info, apikey, content-type, x-supabase-api-version",
        )
        .to_string();
    let mut resp = StatusCode::NO_CONTENT.into_response();
    let h = resp.headers_mut();
    h.insert(
        "access-control-allow-origin",
        HeaderValue::from_static("*"),
    );
    h.insert(
        "access-control-allow-methods",
        HeaderValue::from_static("POST, GET, PUT, PATCH, DELETE, OPTIONS"),
    );
    if let Ok(v) = HeaderValue::try_from(allow_headers) {
        h.insert("access-control-allow-headers", v);
    }
    h.insert(
        "access-control-max-age",
        HeaderValue::from_static("86400"),
    );
    resp
}

// ---------------------------------------------------------------------------
// Data-access helpers
// ---------------------------------------------------------------------------

async fn insert_code<S: RelationalStorage + 'static>(
    state: &AppState<S>,
    auth: &AuthContext,
    id: Uuid,
    wasm: &[u8],
    now: chrono::DateTime<Utc>,
) -> Result<(), SupaError> {
    let sha = hex_sha256(wasm);
    run_sql_as(
        &state.db,
        auth,
        "INSERT INTO functions._code \
         (function_id, body, content_sha256, size_bytes, updated_at) \
         VALUES ($1, $2, $3, $4, $5)",
        vec![
            SqlValue::Uuid(id),
            SqlValue::Bytea(wasm.to_vec()),
            SqlValue::Text(sha),
            SqlValue::Int8(wasm.len() as i64),
            SqlValue::Timestamptz(now),
        ],
    )
    .await
    .map_err(SupaError::Sql)?;
    Ok(())
}

async fn replace_code<S: RelationalStorage + 'static>(
    state: &AppState<S>,
    auth: &AuthContext,
    id: Uuid,
    wasm: &[u8],
    now: chrono::DateTime<Utc>,
) -> Result<(), SupaError> {
    run_sql_as(
        &state.db,
        auth,
        "DELETE FROM functions._code WHERE function_id = $1",
        vec![SqlValue::Uuid(id)],
    )
    .await
    .map_err(SupaError::Sql)?;
    insert_code(state, auth, id, wasm, now).await
}

async fn load_code<S: RelationalStorage + 'static>(
    state: &AppState<S>,
    auth: &AuthContext,
    id: Uuid,
) -> Result<Option<Vec<u8>>, SupaError> {
    let result = run_sql_as(
        &state.db,
        auth,
        "SELECT body FROM functions._code WHERE function_id = $1",
        vec![SqlValue::Uuid(id)],
    )
    .await
    .map_err(SupaError::Sql)?;
    let ExecResult::Rows { rows, .. } = result else {
        return Ok(None);
    };
    let Some(row) = rows.into_iter().next() else {
        return Ok(None);
    };
    match row.into_iter().next() {
        Some(SqlValue::Bytea(bytes)) => Ok(Some(bytes)),
        Some(SqlValue::Null) => Ok(None),
        _ => Err(SupaError::Internal(
            "function body column is not bytea".into(),
        )),
    }
}

async fn load_secrets<S: RelationalStorage + 'static>(
    state: &AppState<S>,
    auth: &AuthContext,
    id: Uuid,
) -> Result<Vec<(String, String)>, SupaError> {
    let result = run_sql_as(
        &state.db,
        auth,
        "SELECT name, value FROM functions.secrets WHERE function_id = $1 ORDER BY name",
        vec![SqlValue::Uuid(id)],
    )
    .await
    .map_err(SupaError::Sql)?;
    let ExecResult::Rows { rows, .. } = result else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut it = row.into_iter();
        let name = match it.next() {
            Some(SqlValue::Text(s)) => s,
            _ => continue,
        };
        let value = match it.next() {
            Some(SqlValue::Text(s)) => s,
            _ => continue,
        };
        out.push((name, value));
    }
    Ok(out)
}

async fn load_function_row<S: RelationalStorage + 'static>(
    state: &AppState<S>,
    auth: &AuthContext,
    slug: &str,
) -> Result<Option<Json>, SupaError> {
    let result = run_sql_as(
        &state.db,
        auth,
        &format!("SELECT {FUNCTION_COLUMNS} FROM functions.functions WHERE slug = $1"),
        vec![SqlValue::Text(slug.to_string())],
    )
    .await
    .map_err(SupaError::Sql)?;
    let objs = result_objects(result)?;
    Ok(objs.into_iter().next())
}

async fn fetch_one_function<S: RelationalStorage + 'static>(
    state: &AppState<S>,
    auth: &AuthContext,
    slug: &str,
) -> Result<Json, SupaError> {
    load_function_row(state, auth, slug)
        .await?
        .ok_or_else(|| SupaError::Internal(format!("function \"{slug}\" vanished after write")))
}

fn extract_id(row: &Json) -> Result<Uuid, SupaError> {
    row.get("id")
        .and_then(Json::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| SupaError::Internal("function row missing id".into()))
}

fn extract_bool(row: &Json, key: &str) -> Option<bool> {
    row.get(key).and_then(Json::as_bool)
}

// ---------------------------------------------------------------------------
// Small utilities
// ---------------------------------------------------------------------------

fn require_service_role(auth: &AuthContext) -> Result<(), SupaError> {
    if auth.is_service_role() {
        Ok(())
    } else {
        Err(SupaError::Forbidden("this admin route"))
    }
}

/// Build a service-role-shaped copy of the caller's `AuthContext`, preserving
/// their claims and request-id — used for internal metadata lookups on the
/// invocation path that must bypass the RLS-default-deny on `functions.*`.
fn as_service_role(auth: &AuthContext) -> AuthContext {
    let mut clone = auth.clone();
    clone.role = "service_role".into();
    clone
}

fn validate_slug(slug: &str) -> Result<(), SupaError> {
    let ok = !slug.is_empty()
        && slug.len() <= 128
        && slug
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(())
    } else {
        Err(SupaError::BadRequest(format!(
            "invalid function slug: \"{slug}\" (allowed: [A-Za-z0-9_-], max 128)"
        )))
    }
}

fn validate_secret_name(name: &str) -> Result<(), SupaError> {
    let ok = !name.is_empty()
        && name.len() <= 128
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        Ok(())
    } else {
        Err(SupaError::BadRequest(format!(
            "invalid secret name: \"{name}\" (must match [A-Za-z_][A-Za-z0-9_]*, max 128)"
        )))
    }
}

fn decode_wasm(b64: &str) -> Result<Vec<u8>, SupaError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| SupaError::BadRequest(format!("wasm body is not valid base64: {e}")))?;
    if bytes.len() > MAX_WASM_BUNDLE_BYTES {
        return Err(SupaError::BadRequest(format!(
            "wasm body exceeds {MAX_WASM_BUNDLE_BYTES} bytes ({} bytes)",
            bytes.len()
        )));
    }
    validate_wasm_magic(&bytes)?;
    Ok(bytes)
}

fn validate_wasm_magic(bytes: &[u8]) -> Result<(), SupaError> {
    const WASM_MAGIC: &[u8] = b"\0asm";
    if bytes.len() < 8 || &bytes[..4] != WASM_MAGIC {
        return Err(SupaError::BadRequest(
            "body is not a WebAssembly module (missing \\0asm magic)".into(),
        ));
    }
    Ok(())
}

fn hex_sha256(bytes: &[u8]) -> String {
    #[cfg(feature = "sql")]
    {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(bytes);
        return hex::encode(digest);
    }
    #[cfg(not(feature = "sql"))]
    {
        // The `supabase` feature implies `sql`, which pulls in sha2, but keep
        // the fallback so a hypothetical feature-shuffled build still builds.
        format!("blake3-{}", blake3::hash(bytes).to_hex())
    }
}

fn bearer_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
                .or_else(|| v.strip_prefix("BEARER "))
                .map(str::trim)
                .map(String::from)
        })
}

/// Encode a value as CBOR — bytes-safe transport for guest input.
fn serde_cbor_to_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, SupaError> {
    let mut out = Vec::with_capacity(256);
    ciborium::ser::into_writer(value, &mut out)
        .map_err(|e| SupaError::Internal(format!("_functions_runtime::cbor encode: {e}")))?;
    Ok(out)
}

fn serde_cbor_from_bytes<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, SupaError> {
    ciborium::de::from_reader(bytes)
        .map_err(|e| SupaError::Internal(format!("_functions_runtime::cbor decode: {e}")))
}

// ---------------------------------------------------------------------------
// ExecResult → JSON (mirrors the storage.rs helper; kept local to avoid
// pulling private helpers across modules)
// ---------------------------------------------------------------------------

/// Convert an `ExecResult` into an array of JSON objects.
fn result_objects(result: ExecResult) -> Result<Vec<Json>, SupaError> {
    match result {
        ExecResult::Rows { fields, rows } => {
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                let mut obj = Map::with_capacity(fields.len());
                for (field, value) in fields.iter().zip(row.into_iter()) {
                    obj.insert(field.name.clone(), value_to_json(value));
                }
                out.push(Json::Object(obj));
            }
            Ok(out)
        }
        ExecResult::Command { .. } => Ok(Vec::new()),
    }
}

/// Local, self-contained SqlValue → JSON rendering. Mirrors the shape of the
/// REST module's helper without depending on it, since this feature-slice
/// keeps its module surface minimal.
fn value_to_json(v: SqlValue) -> Json {
    match v {
        SqlValue::Null => Json::Null,
        SqlValue::Bool(b) => Json::Bool(b),
        SqlValue::Int2(n) => Json::from(n),
        SqlValue::Int4(n) => Json::from(n),
        SqlValue::Int8(n) => Json::from(n),
        SqlValue::Float4(f) => serde_json::Number::from_f64(f as f64)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        SqlValue::Float8(f) => serde_json::Number::from_f64(f)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        SqlValue::Numeric(d) => Json::String(d.to_string()),
        SqlValue::Text(s) => Json::String(s),
        SqlValue::Citext(s) => Json::String(s),
        SqlValue::Bytea(b) => {
            Json::String(base64::engine::general_purpose::STANDARD.encode(b))
        }
        SqlValue::Uuid(u) => Json::String(u.to_string()),
        SqlValue::Date(d) => Json::String(d.to_string()),
        SqlValue::Time(t) => Json::String(t.to_string()),
        SqlValue::Timestamp(t) => Json::String(t.to_string()),
        SqlValue::Timestamptz(t) => Json::String(t.to_rfc3339()),
        SqlValue::Json(j) => j,
        SqlValue::Array(items) => Json::Array(items.into_iter().map(value_to_json).collect()),
        SqlValue::Vector(v) => Json::Array(
            v.into_iter()
                .filter_map(|f| serde_json::Number::from_f64(f as f64).map(Json::Number))
                .collect(),
        ),
        SqlValue::HStore(m) => {
            let mut obj = Map::with_capacity(m.len());
            for (k, v) in m {
                obj.insert(k, v.map(Json::String).unwrap_or(Json::Null));
            }
            Json::Object(obj)
        }
        SqlValue::Ltree(s) => Json::String(s),
        SqlValue::Cube { ll, ur } => json!({ "ll": ll, "ur": ur }),
        SqlValue::TsVector(_) | SqlValue::TsQuery(_) => Json::Null,
    }
}

// ---------------------------------------------------------------------------
// Error-driven response helper (mirrors storage.rs's `run` helper)
// ---------------------------------------------------------------------------

async fn run<F>(fut: F) -> Response
where
    F: std::future::Future<Output = Result<Response, SupaError>>,
{
    fut.await.unwrap_or_else(functions_error_from)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_validation() {
        assert!(validate_slug("hello-world").is_ok());
        assert!(validate_slug("hello_world_2").is_ok());
        assert!(validate_slug("Hello123").is_ok());
        assert!(validate_slug("").is_err());
        assert!(validate_slug("has space").is_err());
        assert!(validate_slug("has/slash").is_err());
        assert!(validate_slug(&"a".repeat(129)).is_err());
    }

    #[test]
    fn secret_name_validation() {
        assert!(validate_secret_name("MY_ENV").is_ok());
        assert!(validate_secret_name("_leading").is_ok());
        assert!(validate_secret_name("").is_err());
        assert!(validate_secret_name("1leading").is_err());
        assert!(validate_secret_name("has-dash").is_err());
    }

    #[test]
    fn wasm_magic_check() {
        assert!(validate_wasm_magic(b"\0asm\x01\x00\x00\x00").is_ok());
        assert!(validate_wasm_magic(b"not-wasm").is_err());
        assert!(validate_wasm_magic(b"").is_err());
    }

    #[test]
    fn error_shape_stable() {
        let resp = functions_error(FunctionsErrorCode::NotFound, "no such fn");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn cbor_round_trip() {
        let input = InvocationInput {
            method: "POST".into(),
            url: "http://x/functions/v1/hi".into(),
            path: "/functions/v1/hi".into(),
            query: String::new(),
            headers: vec![("x".into(), "1".into())],
            body_b64: base64::engine::general_purpose::STANDARD.encode(b"hello"),
            env: vec![("SECRET".into(), "s3cret".into())],
            role: "anon".into(),
            request_id: "req-1".into(),
            jwt: String::new(),
        };
        let bytes = serde_cbor_to_bytes(&input).unwrap();
        let back: InvocationInput = serde_cbor_from_bytes(&bytes).unwrap();
        assert_eq!(back.method, "POST");
        assert_eq!(back.headers, vec![("x".to_string(), "1".to_string())]);
    }

    #[test]
    fn default_status_is_200() {
        let out: InvocationOutput = serde_json::from_value(json!({})).unwrap();
        assert_eq!(out.status, 200);
        assert!(out.body_b64.is_empty());
    }
}
