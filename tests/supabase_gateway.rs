//! In-process integration tests for the Supabase-compatible gateway.
//!
//! These drive the axum `Router` directly with `tower::ServiceExt::oneshot`
//! over a `MemoryStorage`-backed `Database` — no real ports are bound.

#![cfg(feature = "supabase")]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

use guardian_db::sql::MemoryStorage;
use guardian_db::sql::engine::{Database, Session};
use guardian_db::supabase::project::ProjectKeys;
use guardian_db::supabase::{AppState, ServiceConfig, SupabaseCompatProject, build_router};

const TEST_SECRET: &str = "integration-test-jwt-secret-value-0123456789";
const IAT: i64 = 1_700_000_000;

struct Harness {
    app: Router,
    anon: String,
    service: String,
    db: Arc<Database<MemoryStorage>>,
}

async fn harness() -> Harness {
    let db = Arc::new(Database::new(Arc::new(MemoryStorage::new()), "app"));
    let keys = ProjectKeys::from_secret(TEST_SECRET, IAT).unwrap();
    let anon = keys.anon_key.clone();
    let service = keys.service_role_key.clone();
    let project =
        SupabaseCompatProject::shell("app", "http://127.0.0.1:54321", keys, chrono::Utc::now());
    let state = AppState::new(db.clone(), project, ServiceConfig::default());
    let app = build_router(state);
    Harness {
        app,
        anon,
        service,
        db,
    }
}

/// Send a request and return (status, headers, JSON body).
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    apikey: Option<&str>,
    bearer: Option<&str>,
    prefer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(k) = apikey {
        builder = builder.header("apikey", k);
    }
    if let Some(b) = bearer {
        builder = builder.header("authorization", format!("Bearer {b}"));
    }
    if let Some(p) = prefer {
        builder = builder.header("prefer", p);
    }
    let req = match body {
        Some(v) => builder
            .header("content-type", "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
    };
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, json)
}

async fn seed_todos(db: &Arc<Database<MemoryStorage>>) {
    let mut s = Session::new(db.clone(), "postgres");
    s.execute("CREATE TABLE todos (id int PRIMARY KEY, title text, done boolean)")
        .await
        .unwrap();
    s.execute("INSERT INTO todos VALUES (1, 'buy milk', false), (2, 'walk dog', true)")
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// Gateway
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_apikey_is_401_typed() {
    let h = harness().await;
    let (status, _headers, body) = call(
        &h.app,
        "GET",
        "/rest/v1/todos?select=*",
        Some("not-a-valid-key"),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "SUPA_COMPAT_INVALID_API_KEY");
}

#[tokio::test]
async fn missing_apikey_is_401_typed() {
    let h = harness().await;
    let (status, _h, body) = call(&h.app, "GET", "/rest/v1/todos", None, None, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "SUPA_COMPAT_MISSING_API_KEY");
}

#[tokio::test]
async fn functions_service_is_wired() {
    // Storage / realtime / pg-meta are implemented since stage 3 (see
    // tests/supabase_storage_realtime.rs), graphql since stage 4 (see
    // tests/supabase_graphql.rs), and functions since stage 5 (see
    // tests/supabase_functions.rs). Invoking a slug that isn't deployed now
    // returns the typed 404 shape, never a bare 404 or 501.
    let h = harness().await;
    let (status, _h, body) = call(
        &h.app,
        "POST",
        "/functions/v1/hello-world",
        Some(&h.anon),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_NOT_FOUND");

    // A missing storage object is now a typed storage-api error, not a 501.
    let (status, _h, body) = call(
        &h.app,
        "GET",
        "/storage/v1/object/bucket/file.png",
        Some(&h.anon),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["statusCode"], "404");
}

#[tokio::test]
async fn request_id_is_propagated() {
    let h = harness().await;
    let (_status, headers, _body) = call(&h.app, "GET", "/health", None, None, None, None).await;
    assert!(headers.get("x-request-id").is_some());
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rest_select_with_eq_filter() {
    let h = harness().await;
    seed_todos(&h.db).await;
    let (status, _h, body) = call(
        &h.app,
        "GET",
        "/rest/v1/todos?select=*&id=eq.1",
        Some(&h.anon),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], 1);
    assert_eq!(arr[0]["title"], "buy milk");
    assert_eq!(arr[0]["done"], false);
}

#[tokio::test]
async fn rest_select_all_ordered() {
    let h = harness().await;
    seed_todos(&h.db).await;
    let (status, _h, body) = call(
        &h.app,
        "GET",
        "/rest/v1/todos?select=id,title&order=id.desc",
        Some(&h.anon),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["id"], 2);
    // Only selected columns are present.
    assert!(arr[0].get("done").is_none());
}

#[tokio::test]
async fn rest_insert_return_representation() {
    let h = harness().await;
    seed_todos(&h.db).await;
    let (status, _h, body) = call(
        &h.app,
        "POST",
        "/rest/v1/todos",
        Some(&h.service),
        None,
        Some("return=representation"),
        Some(json!({"id": 3, "title": "cook dinner", "done": false})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["id"], 3);
    assert_eq!(arr[0]["title"], "cook dinner");
}

#[tokio::test]
async fn rest_insert_array_and_patch_and_delete() {
    let h = harness().await;
    seed_todos(&h.db).await;

    // Insert two rows at once.
    let (status, _h, _b) = call(
        &h.app,
        "POST",
        "/rest/v1/todos",
        Some(&h.service),
        None,
        Some("return=minimal"),
        Some(json!([
            {"id": 10, "title": "ten", "done": false},
            {"id": 11, "title": "eleven", "done": false}
        ])),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // PATCH id=10.
    let (status, _h, body) = call(
        &h.app,
        "PATCH",
        "/rest/v1/todos?id=eq.10",
        Some(&h.service),
        None,
        Some("return=representation"),
        Some(json!({"done": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap()[0]["done"], true);

    // DELETE id=11.
    let (status, _h, body) = call(
        &h.app,
        "DELETE",
        "/rest/v1/todos?id=eq.11",
        Some(&h.service),
        None,
        Some("return=representation"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap()[0]["id"], 11);
}

#[tokio::test]
async fn rest_count_exact_content_range() {
    let h = harness().await;
    seed_todos(&h.db).await;
    let (status, headers, _body) = call(
        &h.app,
        "GET",
        "/rest/v1/todos?select=*&order=id",
        Some(&h.anon),
        None,
        Some("count=exact"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    let cr = headers.get("content-range").unwrap().to_str().unwrap();
    assert_eq!(cr, "0-1/2");
}

#[tokio::test]
async fn rest_single_object_accept() {
    let h = harness().await;
    seed_todos(&h.db).await;
    let req = Request::builder()
        .method("GET")
        .uri("/rest/v1/todos?id=eq.2")
        .header("apikey", &h.anon)
        .header("accept", "application/vnd.pgrst.object+json")
        .body(Body::empty())
        .unwrap();
    let resp = h.app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body.is_object());
    assert_eq!(body["id"], 2);
}

#[tokio::test]
async fn rest_unsupported_filter_is_400() {
    let h = harness().await;
    seed_todos(&h.db).await;
    let (status, _h, body) = call(
        &h.app,
        "GET",
        "/rest/v1/todos?title=cs.foo",
        Some(&h.anon),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "SUPA_COMPAT_REST_UNSUPPORTED_FILTER");
}

#[tokio::test]
async fn rest_missing_table_is_pgrst_404() {
    let h = harness().await;
    let (status, _h, body) = call(
        &h.app,
        "GET",
        "/rest/v1/nope?select=*",
        Some(&h.anon),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "42P01");
}

#[tokio::test]
async fn rest_rpc_scalar_function() {
    let h = harness().await;
    let (status, _h, body) = call(
        &h.app,
        "POST",
        "/rest/v1/rpc/upper",
        Some(&h.anon),
        None,
        None,
        Some(json!({"a": "hello"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.to_string().contains("HELLO"),
        "rpc upper should uppercase: {body}"
    );
}

/// `/rpc/{name}` transparently reaches user-defined functions too: it just
/// compiles to `SELECT name(args...)` over the same SQL engine that resolves
/// `CREATE FUNCTION`-registered functions (see `guardian_db::sql::udf`).
#[tokio::test]
async fn rest_rpc_calls_user_defined_function() {
    let h = harness().await;
    let mut s = Session::new(h.db.clone(), "postgres");
    s.execute("CREATE FUNCTION add_one(n int) RETURNS int LANGUAGE SQL AS $$ SELECT $1 + 1 $$")
        .await
        .unwrap();

    let (status, _h, body) = call(
        &h.app,
        "POST",
        "/rest/v1/rpc/add_one",
        Some(&h.anon),
        None,
        None,
        Some(json!({"n": 41})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.to_string().contains("42"),
        "rpc add_one(41) should return 42: {body}"
    );
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_signup_then_login_then_user() {
    let h = harness().await;

    // signup
    let (status, _h, body) = call(
        &h.app,
        "POST",
        "/auth/v1/signup",
        Some(&h.anon),
        None,
        None,
        Some(json!({"email": "alice@example.com", "password": "hunter2pass"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "signup body: {body}");
    assert!(body["access_token"].is_string());
    assert_eq!(body["token_type"], "bearer");
    assert!(body["refresh_token"].is_string());
    assert_eq!(body["user"]["email"], "alice@example.com");
    assert_eq!(body["user"]["role"], "authenticated");

    // token grant_type=password
    let (status, _h, tok) = call(
        &h.app,
        "POST",
        "/auth/v1/token?grant_type=password",
        Some(&h.anon),
        None,
        None,
        Some(json!({"email": "alice@example.com", "password": "hunter2pass"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "token body: {tok}");
    let access = tok["access_token"].as_str().unwrap().to_string();
    let refresh = tok["refresh_token"].as_str().unwrap().to_string();

    // GET /user with the access token
    let (status, _h, user) = call(
        &h.app,
        "GET",
        "/auth/v1/user",
        Some(&h.anon),
        Some(&access),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "user body: {user}");
    assert_eq!(user["email"], "alice@example.com");

    // refresh_token rotation
    let (status, _h, refreshed) = call(
        &h.app,
        "POST",
        "/auth/v1/token?grant_type=refresh_token",
        Some(&h.anon),
        None,
        None,
        Some(json!({"refresh_token": refresh})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "refresh body: {refreshed}");
    assert!(refreshed["access_token"].is_string());
    assert_ne!(refreshed["refresh_token"].as_str().unwrap(), "");
}

#[tokio::test]
async fn auth_bad_password_is_400() {
    let h = harness().await;
    call(
        &h.app,
        "POST",
        "/auth/v1/signup",
        Some(&h.anon),
        None,
        None,
        Some(json!({"email": "bob@example.com", "password": "correct-horse"})),
    )
    .await;
    let (status, _h, body) = call(
        &h.app,
        "POST",
        "/auth/v1/token?grant_type=password",
        Some(&h.anon),
        None,
        None,
        Some(json!({"email": "bob@example.com", "password": "wrong"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_grant");
}

#[tokio::test]
async fn auth_duplicate_signup_is_422() {
    let h = harness().await;
    let signup = || {
        call(
            &h.app,
            "POST",
            "/auth/v1/signup",
            Some(&h.anon),
            None,
            None,
            Some(json!({"email": "dup@example.com", "password": "passwordpassword"})),
        )
    };
    let (status, _h, _b) = signup().await;
    assert_eq!(status, StatusCode::OK);
    let (status, _h, body) = signup().await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error_code"], "user_already_exists");
}

#[tokio::test]
async fn auth_admin_requires_service_role() {
    let h = harness().await;

    // Create a user via signup so the list is non-empty.
    call(
        &h.app,
        "POST",
        "/auth/v1/signup",
        Some(&h.anon),
        None,
        None,
        Some(json!({"email": "carol@example.com", "password": "passwordpassword"})),
    )
    .await;

    // anon key → 403 typed.
    let (status, _h, body) = call(
        &h.app,
        "GET",
        "/auth/v1/admin/users",
        Some(&h.anon),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "SUPA_COMPAT_FORBIDDEN");

    // service_role key → works.
    let (status, _h, body) = call(
        &h.app,
        "GET",
        "/auth/v1/admin/users",
        Some(&h.service),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin list body: {body}");
    let users = body["users"].as_array().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0]["email"], "carol@example.com");
}

#[tokio::test]
async fn auth_oauth_provider_is_typed_unsupported() {
    let h = harness().await;
    let (status, _h, body) = call(
        &h.app,
        "POST",
        "/auth/v1/token?grant_type=id_token",
        Some(&h.anon),
        None,
        None,
        Some(json!({"provider": "google", "id_token": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "SUPA_COMPAT_AUTH_PROVIDER_UNSUPPORTED");
}

// ---------------------------------------------------------------------------
// Row-Level Security through the gateway
// ---------------------------------------------------------------------------

const UID_A: &str = "0b9fbc1e-6a34-4bff-8df5-6b9f7c4e3d21";
const UID_B: &str = "7f3a1d52-9c1b-4e8e-b0a4-2c5d9e8f7a61";

/// `notes` owned per user: RLS enabled, owner-scoped SELECT/INSERT policies
/// for `authenticated` (so `anon` is default-denied).
async fn seed_rls_notes(db: &Arc<Database<MemoryStorage>>) {
    let mut s = Session::new(db.clone(), "postgres");
    s.execute("CREATE TABLE notes (id int PRIMARY KEY, user_id text, body text, secret int)")
        .await
        .unwrap();
    s.execute(&format!(
        "INSERT INTO notes VALUES (1, '{UID_A}', 'a note', 1), (2, '{UID_B}', 'b note', 2)"
    ))
    .await
    .unwrap();
    s.execute("ALTER TABLE notes ENABLE ROW LEVEL SECURITY")
        .await
        .unwrap();
    s.execute(
        "CREATE POLICY notes_select ON notes FOR SELECT TO authenticated \
         USING (user_id = auth.uid()::text)",
    )
    .await
    .unwrap();
    s.execute(
        "CREATE POLICY notes_insert ON notes FOR INSERT TO authenticated \
         WITH CHECK (user_id = auth.uid()::text AND secret < 100)",
    )
    .await
    .unwrap();
}

/// Mint a real user access token (`role: authenticated`, `sub: <uuid>`) signed
/// with the project secret, like GoTrue would.
fn user_token(sub: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let mut claims = guardian_db::supabase::Claims::api_key("authenticated", now, now + 3600);
    claims.sub = Some(sub.to_string());
    claims.aud = Some("authenticated".to_string());
    guardian_db::supabase::jwt::sign(&claims, TEST_SECRET).unwrap()
}

#[tokio::test]
async fn rls_anon_sees_nothing() {
    let h = harness().await;
    seed_rls_notes(&h.db).await;
    let (status, _h, body) = call(
        &h.app,
        "GET",
        "/rest/v1/notes?select=*",
        Some(&h.anon),
        None,
        None,
        None,
    )
    .await;
    // Policies target `authenticated`: anon is default-denied, not an error.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));
}

#[tokio::test]
async fn rls_authenticated_user_sees_only_their_rows() {
    let h = harness().await;
    seed_rls_notes(&h.db).await;
    let token = user_token(UID_A);
    let (status, _h, body) = call(
        &h.app,
        "GET",
        "/rest/v1/notes?select=*&order=id.asc",
        Some(&h.anon),
        Some(&token),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1, "user A must see exactly their row: {body}");
    assert_eq!(arr[0]["user_id"], UID_A);
    assert_eq!(arr[0]["body"], "a note");
}

#[tokio::test]
async fn rls_service_role_bypasses_policies() {
    let h = harness().await;
    seed_rls_notes(&h.db).await;
    let (status, _h, body) = call(
        &h.app,
        "GET",
        "/rest/v1/notes?select=*",
        Some(&h.service),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn rls_insert_passing_with_check_succeeds() {
    let h = harness().await;
    seed_rls_notes(&h.db).await;
    let token = user_token(UID_A);
    let (status, _h, body) = call(
        &h.app,
        "POST",
        "/rest/v1/notes",
        Some(&h.anon),
        Some(&token),
        Some("return=representation"),
        Some(json!({"id": 3, "user_id": UID_A, "body": "mine", "secret": 5})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body: {body}");
    assert_eq!(body.as_array().unwrap()[0]["body"], "mine");
}

#[tokio::test]
async fn rls_insert_violating_with_check_is_pgrst_42501() {
    let h = harness().await;
    seed_rls_notes(&h.db).await;
    let token = user_token(UID_A);
    // secret >= 100 violates the policy's WITH CHECK.
    let (status, _h, body) = call(
        &h.app,
        "POST",
        "/rest/v1/notes",
        Some(&h.anon),
        Some(&token),
        None,
        Some(json!({"id": 4, "user_id": UID_A, "body": "x", "secret": 500})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "42501");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("row-level security policy for table \"notes\""),
        "message: {body}"
    );
    // Writing a row for someone else fails the same way.
    let (status, _h, body) = call(
        &h.app,
        "POST",
        "/rest/v1/notes",
        Some(&h.anon),
        Some(&token),
        None,
        Some(json!({"id": 5, "user_id": UID_B, "body": "forged", "secret": 1})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "42501");
    // Nothing leaked into the table.
    let (_s, _h2, all) = call(
        &h.app,
        "GET",
        "/rest/v1/notes?select=id",
        Some(&h.service),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(all.as_array().unwrap().len(), 2);
}
