//! Integration tests for Supabase-compat stage 3: Storage, postgres-meta and
//! Realtime.
//!
//! Storage and pg-meta are driven in-process with `tower::ServiceExt::oneshot`
//! (like `tests/supabase_gateway.rs`). Realtime tests bind an ephemeral
//! 127.0.0.1 port and speak the Phoenix protocol over a real websocket with
//! `tokio-tungstenite`.

#![cfg(feature = "supabase")]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tower::ServiceExt;

use guardian_db::sql::MemoryStorage;
use guardian_db::sql::engine::{Database, Session};
use guardian_db::supabase::project::ProjectKeys;
use guardian_db::supabase::{AppState, ServiceConfig, SupabaseCompatProject, build_router};

const TEST_SECRET: &str = "integration-test-jwt-secret-value-0123456789";
const IAT: i64 = 1_700_000_000;

const UID_A: &str = "0b9fbc1e-6a34-4bff-8df5-6b9f7c4e3d21";
const UID_B: &str = "7f3a1d52-9c1b-4e8e-b0a4-2c5d9e8f7a61";

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

/// Mint a real user access token (`role: authenticated`, `sub: <uuid>`).
fn user_token(sub: &str) -> String {
    let now = chrono::Utc::now().timestamp();
    let mut claims = guardian_db::supabase::Claims::api_key("authenticated", now, now + 3600);
    claims.sub = Some(sub.to_string());
    claims.aud = Some("authenticated".to_string());
    guardian_db::supabase::jwt::sign(&claims, TEST_SECRET).unwrap()
}

/// Send a request with arbitrary headers and a raw body; return
/// `(status, headers, body bytes)`.
async fn call_raw(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> (StatusCode, HeaderMap, Vec<u8>) {
    let mut builder = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        builder = builder.header(*k, *v);
    }
    let req = builder
        .body(body.map(Body::from).unwrap_or_else(Body::empty))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let resp_headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, resp_headers, bytes.to_vec())
}

/// JSON-bodied convenience wrapper around [`call_raw`].
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    apikey: Option<&str>,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut headers: Vec<(String, String)> = Vec::new();
    if let Some(k) = apikey {
        headers.push(("apikey".into(), k.to_string()));
    }
    if let Some(b) = bearer {
        headers.push(("authorization".into(), format!("Bearer {b}")));
    }
    if body.is_some() {
        headers.push(("content-type".into(), "application/json".into()));
    }
    let header_refs: Vec<(&str, &str)> = headers
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let (status, _h, bytes) = call_raw(
        app,
        method,
        uri,
        &header_refs,
        body.map(|v| v.to_string().into_bytes()),
    )
    .await;
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

async fn create_bucket(h: &Harness, key: &str, name: &str, body: Value) -> (StatusCode, Value) {
    let mut b = body;
    b["name"] = json!(name);
    call(
        &h.app,
        "POST",
        "/storage/v1/bucket",
        Some(key),
        None,
        Some(b),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn upload(
    h: &Harness,
    key: &str,
    bearer: Option<&str>,
    bucket: &str,
    path: &str,
    content_type: &str,
    bytes: &[u8],
    upsert: bool,
) -> (StatusCode, Value) {
    let auth_header;
    let mut headers: Vec<(&str, &str)> = vec![("apikey", key), ("content-type", content_type)];
    if let Some(b) = bearer {
        auth_header = format!("Bearer {b}");
        headers.push(("authorization", auth_header.as_str()));
    }
    if upsert {
        headers.push(("x-upsert", "true"));
    }
    let (status, _h, body) = call_raw(
        &h.app,
        "POST",
        &format!("/storage/v1/object/{bucket}/{path}"),
        &headers,
        Some(bytes.to_vec()),
    )
    .await;
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

// ===========================================================================
// Storage: buckets
// ===========================================================================

#[tokio::test]
async fn storage_bucket_crud() {
    let h = harness().await;

    // Create.
    let (status, body) = create_bucket(&h, &h.service, "avatars", json!({"public": false})).await;
    assert_eq!(status, StatusCode::OK, "create: {body}");
    assert_eq!(body["name"], "avatars");

    // Duplicate → 409 in storage shape.
    let (status, body) = create_bucket(&h, &h.service, "avatars", json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(body["statusCode"], "409");
    assert_eq!(body["error"], "Duplicate");

    // Get.
    let (status, body) = call(
        &h.app,
        "GET",
        "/storage/v1/bucket/avatars",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "get: {body}");
    assert_eq!(body["id"], "avatars");
    assert_eq!(body["public"], false);

    // List.
    let (status, body) = call(
        &h.app,
        "GET",
        "/storage/v1/bucket",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);

    // Update.
    let (status, body) = call(
        &h.app,
        "PUT",
        "/storage/v1/bucket/avatars",
        Some(&h.service),
        None,
        Some(json!({"public": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update: {body}");
    let (_s, body) = call(
        &h.app,
        "GET",
        "/storage/v1/bucket/avatars",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(body["public"], true);

    // Missing bucket → storage-shaped 404 (not a bare 404).
    let (status, body) = call(
        &h.app,
        "GET",
        "/storage/v1/bucket/nope",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["statusCode"], "404");

    // Delete-nonempty is rejected.
    let (status, _b) = upload(
        &h,
        &h.service,
        None,
        "avatars",
        "a.txt",
        "text/plain",
        b"hello",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &h.app,
        "DELETE",
        "/storage/v1/bucket/avatars",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    // Empty it, then delete works.
    let (status, _b) = call(
        &h.app,
        "POST",
        "/storage/v1/bucket/avatars/empty",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = call(
        &h.app,
        "DELETE",
        "/storage/v1/bucket/avatars",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["message"], "Successfully deleted");
}

#[tokio::test]
async fn storage_anon_cannot_create_bucket_by_default() {
    let h = harness().await;
    // RLS is enabled on storage.buckets with no policies: anon is denied.
    let (status, body) = create_bucket(&h, &h.anon, "hacked", json!({})).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["statusCode"], "403");
    assert_eq!(body["error"], "42501");
}

// ===========================================================================
// Storage: upload / download
// ===========================================================================

#[tokio::test]
async fn storage_upload_download_roundtrip_and_public_rules() {
    let h = harness().await;
    create_bucket(&h, &h.service, "private", json!({"public": false})).await;
    create_bucket(&h, &h.service, "pub", json!({"public": true})).await;

    let payload = b"GuardianDB stores bytes!";
    let (status, body) = upload(
        &h,
        &h.service,
        None,
        "private",
        "docs/readme.txt",
        "text/plain",
        payload,
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "upload: {body}");
    assert_eq!(body["Key"], "private/docs/readme.txt");
    assert!(body["Id"].is_string());

    // Authed download (service key) returns the exact bytes + content type.
    let (status, headers, bytes) = call_raw(
        &h.app,
        "GET",
        "/storage/v1/object/private/docs/readme.txt",
        &[("apikey", h.service.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, payload);
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "text/plain"
    );

    // anon download of a private object: RLS hides the row → 404, not bytes.
    let (status, _h2, bytes) = call_raw(
        &h.app,
        "GET",
        "/storage/v1/object/private/docs/readme.txt",
        &[("apikey", h.anon.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["statusCode"], "404");

    // Public URL on a private bucket is refused.
    let (status, _h2, _b) = call_raw(
        &h.app,
        "GET",
        "/storage/v1/object/public/private/docs/readme.txt",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Public bucket: no credentials at all needed.
    upload(
        &h,
        &h.service,
        None,
        "pub",
        "logo.png",
        "image/png",
        b"PNG",
        false,
    )
    .await;
    let (status, headers, bytes) = call_raw(
        &h.app,
        "GET",
        "/storage/v1/object/public/pub/logo.png",
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"PNG");
    assert_eq!(
        headers.get("content-type").unwrap().to_str().unwrap(),
        "image/png"
    );

    // Duplicate upload without upsert → 409; with x-upsert → replaced.
    let (status, _b) = upload(
        &h,
        &h.service,
        None,
        "pub",
        "logo.png",
        "image/png",
        b"PNG2",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _b) = upload(
        &h,
        &h.service,
        None,
        "pub",
        "logo.png",
        "image/png",
        b"PNG2",
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_s, _h2, bytes) = call_raw(
        &h.app,
        "GET",
        "/storage/v1/object/public/pub/logo.png",
        &[],
        None,
    )
    .await;
    assert_eq!(bytes, b"PNG2");
}

#[tokio::test]
async fn storage_bucket_limits_enforced() {
    let h = harness().await;
    create_bucket(
        &h,
        &h.service,
        "strict",
        json!({"file_size_limit": 8, "allowed_mime_types": ["text/plain"]}),
    )
    .await;

    // Over the size limit → 413.
    let (status, body) = upload(
        &h,
        &h.service,
        None,
        "strict",
        "big.txt",
        "text/plain",
        b"way more than eight bytes",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{body}");
    assert_eq!(body["statusCode"], "413");

    // Disallowed mime type → 415.
    let (status, body) = upload(
        &h,
        &h.service,
        None,
        "strict",
        "x.json",
        "application/json",
        b"{}",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "{body}");
    assert_eq!(body["error"], "invalid_mime_type");

    // Within both limits → accepted.
    let (status, _b) = upload(
        &h,
        &h.service,
        None,
        "strict",
        "ok.txt",
        "text/plain",
        b"tiny",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Multipart bodies are a typed unsupported error, not silence.
    let (status, _h2, bytes) = call_raw(
        &h.app,
        "POST",
        "/storage/v1/object/strict/m.txt",
        &[
            ("apikey", h.service.as_str()),
            ("content-type", "multipart/form-data; boundary=xyz"),
        ],
        Some(b"--xyz--".to_vec()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "SUPA_COMPAT_STORAGE_MULTIPART_UNSUPPORTED");
}

#[tokio::test]
async fn storage_unknown_route_is_typed_not_bare_404() {
    let h = harness().await;
    let (status, body) = call(
        &h.app,
        "GET",
        "/storage/v1/render/image/whatever",
        Some(&h.anon),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "SUPA_COMPAT_STORAGE_UNSUPPORTED_ROUTE");
}

// ===========================================================================
// Storage: signed URLs
// ===========================================================================

#[tokio::test]
async fn storage_signed_url_accept_expire_tamper() {
    let h = harness().await;
    create_bucket(&h, &h.service, "vault", json!({"public": false})).await;
    upload(
        &h,
        &h.service,
        None,
        "vault",
        "secret.txt",
        "text/plain",
        b"ssh",
        false,
    )
    .await;

    // Create a signed URL (service key).
    let (status, body) = call(
        &h.app,
        "POST",
        "/storage/v1/object/sign/vault/secret.txt",
        Some(&h.service),
        None,
        Some(json!({"expiresIn": 3600})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "sign: {body}");
    let signed = body["signedURL"].as_str().unwrap().to_string();
    assert!(signed.starts_with("/object/sign/vault/secret.txt?token="));

    // Redeem with no credentials at all.
    let (status, _h2, bytes) =
        call_raw(&h.app, "GET", &format!("/storage/v1{signed}"), &[], None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"ssh");

    // Tampered token → 400 typed.
    let tampered = {
        let mut s = signed.clone();
        let last = s.pop().unwrap();
        s.push(if last == 'A' { 'B' } else { 'A' });
        s
    };
    let (status, _h2, bytes) =
        call_raw(&h.app, "GET", &format!("/storage/v1{tampered}"), &[], None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "invalid_signature");

    // Token signed for a different object is refused.
    let (status, _h2, _b) = call_raw(
        &h.app,
        "GET",
        &format!(
            "/storage/v1/object/sign/vault/other.txt?token={}",
            signed.split("token=").nth(1).unwrap()
        ),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Expired token → 400 typed. Craft one with exp in the past.
    let now = chrono::Utc::now().timestamp();
    let mut claims = guardian_db::supabase::Claims::api_key("anon", now - 100, now - 10);
    claims.iss = None;
    claims.extra.insert("url".into(), json!("vault/secret.txt"));
    let expired = guardian_db::supabase::jwt::sign(&claims, TEST_SECRET).unwrap();
    let (status, _h2, bytes) = call_raw(
        &h.app,
        "GET",
        &format!("/storage/v1/object/sign/vault/secret.txt?token={expired}"),
        &[],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["message"].as_str().unwrap().contains("expired"),
        "{body}"
    );

    // Signing requires visibility: anon cannot sign a private object.
    let (status, _b) = call(
        &h.app,
        "POST",
        "/storage/v1/object/sign/vault/secret.txt",
        Some(&h.anon),
        None,
        Some(json!({"expiresIn": 60})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ===========================================================================
// Storage: list / move / copy / delete
// ===========================================================================

#[tokio::test]
async fn storage_list_with_prefix_limit_offset_sort() {
    let h = harness().await;
    create_bucket(&h, &h.service, "files", json!({})).await;
    for name in ["a/1.txt", "a/2.txt", "b/3.txt"] {
        let (status, _b) = upload(
            &h,
            &h.service,
            None,
            "files",
            name,
            "text/plain",
            b"x",
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, body) = call(
        &h.app,
        "POST",
        "/storage/v1/object/list/files",
        Some(&h.service),
        None,
        Some(json!({"prefix": "a/", "sortBy": {"column": "name", "order": "desc"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "list: {body}");
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["name"], "a/2.txt");
    assert_eq!(arr[1]["name"], "a/1.txt");
    assert!(arr[0]["metadata"]["mimetype"].is_string());

    // limit + offset window the filtered set.
    let (_s, body) = call(
        &h.app,
        "POST",
        "/storage/v1/object/list/files",
        Some(&h.service),
        None,
        Some(json!({"prefix": "a/", "limit": 1, "offset": 1})),
    )
    .await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "a/2.txt");
}

#[tokio::test]
async fn storage_move_copy_delete() {
    let h = harness().await;
    create_bucket(&h, &h.service, "ops", json!({})).await;
    upload(
        &h,
        &h.service,
        None,
        "ops",
        "one.txt",
        "text/plain",
        b"1",
        false,
    )
    .await;

    // Move.
    let (status, body) = call(
        &h.app,
        "POST",
        "/storage/v1/object/move",
        Some(&h.service),
        None,
        Some(json!({"bucketId": "ops", "sourceKey": "one.txt", "destinationKey": "moved.txt"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, _h2, _b) = call_raw(
        &h.app,
        "GET",
        "/storage/v1/object/ops/one.txt",
        &[("apikey", h.service.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Copy: both exist afterwards with the same bytes.
    let (status, body) = call(
        &h.app,
        "POST",
        "/storage/v1/object/copy",
        Some(&h.service),
        None,
        Some(json!({"bucketId": "ops", "sourceKey": "moved.txt", "destinationKey": "copy.txt"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["Key"], "ops/copy.txt");
    for name in ["moved.txt", "copy.txt"] {
        let (status, _h2, bytes) = call_raw(
            &h.app,
            "GET",
            &format!("/storage/v1/object/ops/{name}"),
            &[("apikey", h.service.as_str())],
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(bytes, b"1");
    }

    // Single delete, then bulk delete.
    let (status, body) = call(
        &h.app,
        "DELETE",
        "/storage/v1/object/ops/copy.txt",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = call(
        &h.app,
        "DELETE",
        "/storage/v1/object/ops",
        Some(&h.service),
        None,
        Some(json!({"prefixes": ["moved.txt"]})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body.as_array().unwrap().len(), 1);
    assert_eq!(body[0]["name"], "moved.txt");
}

// ===========================================================================
// Storage: RLS
// ===========================================================================

/// Owner-scoped policies on storage.objects for `authenticated`.
async fn seed_owner_policies(h: &Harness) {
    // Trigger the storage bootstrap first, then add policies directly.
    create_bucket(h, &h.service, "mine", json!({})).await;
    let mut s = Session::new(h.db.clone(), "postgres");
    s.execute(
        "CREATE POLICY obj_owner_select ON storage.objects FOR SELECT TO authenticated \
             USING (owner = auth.uid());
         CREATE POLICY obj_owner_insert ON storage.objects FOR INSERT TO authenticated \
             WITH CHECK (owner = auth.uid());
         CREATE POLICY obj_owner_delete ON storage.objects FOR DELETE TO authenticated \
             USING (owner = auth.uid())",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn storage_rls_anon_denied_owner_scoped_service_sees_all() {
    let h = harness().await;
    seed_owner_policies(&h).await;
    let token_a = user_token(UID_A);
    let token_b = user_token(UID_B);

    // anon (no policies for anon) cannot upload.
    let (status, body) = upload(
        &h,
        &h.anon,
        None,
        "mine",
        "anon.txt",
        "text/plain",
        b"nope",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "42501");

    // User A uploads; the owner column is set from auth.uid().
    let (status, body) = upload(
        &h,
        &h.anon,
        Some(&token_a),
        "mine",
        "a.txt",
        "text/plain",
        b"A's file",
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "user A upload: {body}");

    // A can download their own object.
    let auth_a = format!("Bearer {token_a}");
    let (status, _h2, bytes) = call_raw(
        &h.app,
        "GET",
        "/storage/v1/object/mine/a.txt",
        &[
            ("apikey", h.anon.as_str()),
            ("authorization", auth_a.as_str()),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes, b"A's file");

    // B cannot see A's object (404, no leak), and their list is empty.
    let auth_b = format!("Bearer {token_b}");
    let (status, _h2, _b) = call_raw(
        &h.app,
        "GET",
        "/storage/v1/object/mine/a.txt",
        &[
            ("apikey", h.anon.as_str()),
            ("authorization", auth_b.as_str()),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_s, body) = call(
        &h.app,
        "POST",
        "/storage/v1/object/list/mine",
        Some(&h.anon),
        Some(&token_b),
        Some(json!({"prefix": ""})),
    )
    .await;
    assert_eq!(body, json!([]));

    // anon list is empty too (default deny), not an error.
    let (status, body) = call(
        &h.app,
        "POST",
        "/storage/v1/object/list/mine",
        Some(&h.anon),
        None,
        Some(json!({"prefix": ""})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));

    // service_role sees everything.
    let (_s, body) = call(
        &h.app,
        "POST",
        "/storage/v1/object/list/mine",
        Some(&h.service),
        None,
        Some(json!({"prefix": ""})),
    )
    .await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "a.txt");
}

// ===========================================================================
// pg-meta
// ===========================================================================

async fn seed_pg_meta(db: &Arc<Database<MemoryStorage>>) {
    let mut s = Session::new(db.clone(), "postgres");
    s.execute(
        "CREATE TABLE authors (id int PRIMARY KEY, name text NOT NULL, email text UNIQUE);
         CREATE TABLE books (id int PRIMARY KEY, author_id int REFERENCES authors(id), \
             title text);
         ALTER TABLE books ENABLE ROW LEVEL SECURITY;
         CREATE POLICY books_read ON books FOR SELECT TO authenticated USING (true)",
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn pg_meta_requires_service_role() {
    let h = harness().await;
    let (status, body) = call(&h.app, "GET", "/pg-meta/tables", Some(&h.anon), None, None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "SUPA_COMPAT_FORBIDDEN");
    // And still requires a valid apikey at all.
    let (status, _b) = call(&h.app, "GET", "/pg-meta/tables", None, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn pg_meta_tables_columns_constraints() {
    let h = harness().await;
    seed_pg_meta(&h.db).await;

    let (status, body) = call(
        &h.app,
        "GET",
        "/pg-meta/tables?included_schemas=public",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let tables = body.as_array().unwrap();
    assert_eq!(tables.len(), 2);
    let books = tables
        .iter()
        .find(|t| t["name"] == "books")
        .expect("books table");
    assert_eq!(books["schema"], "public");
    assert_eq!(books["rls_enabled"], true);
    assert!(books["id"].is_number());
    // Columns are embedded with postgres-meta keys.
    let cols = books["columns"].as_array().unwrap();
    let author_id = cols.iter().find(|c| c["name"] == "author_id").unwrap();
    assert_eq!(author_id["data_type"], "integer");
    assert_eq!(author_id["format"], "int4");
    // Primary keys and FK relationships.
    assert_eq!(books["primary_keys"][0]["name"], "id");
    let rels = books["relationships"].as_array().unwrap();
    assert!(
        rels.iter().any(|r| r["source_table_name"] == "books"
            && r["target_table_name"] == "authors"
            && r["source_column_name"] == "author_id"),
        "relationships: {rels:?}"
    );

    // The alias prefix works too.
    let (status, alias_body) = call(
        &h.app,
        "GET",
        "/platform/pg-meta/tables?included_schemas=public",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(alias_body.as_array().unwrap().len(), 2);

    // /columns flat endpoint.
    let (status, body) = call(
        &h.app,
        "GET",
        "/pg-meta/columns?included_schemas=public",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cols = body.as_array().unwrap();
    assert!(
        cols.iter()
            .any(|c| c["table"] == "authors" && c["name"] == "email")
    );
    let name_col = cols
        .iter()
        .find(|c| c["table"] == "authors" && c["name"] == "name")
        .unwrap();
    assert_eq!(name_col["is_nullable"], false);

    // /constraints includes p / u / f rows.
    let (status, body) = call(
        &h.app,
        "GET",
        "/pg-meta/constraints?included_schemas=public",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cons = body.as_array().unwrap();
    assert!(
        cons.iter()
            .any(|c| c["type"] == "p" && c["table"] == "authors")
    );
    assert!(
        cons.iter()
            .any(|c| c["type"] == "f" && c["table"] == "books")
    );
}

#[tokio::test]
async fn pg_meta_policies_extensions_roles_schemas() {
    let h = harness().await;
    seed_pg_meta(&h.db).await;

    let (status, body) = call(
        &h.app,
        "GET",
        "/pg-meta/policies",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let policies = body.as_array().unwrap();
    let p = policies
        .iter()
        .find(|p| p["name"] == "books_read")
        .expect("books_read policy");
    assert_eq!(p["table"], "books");
    assert_eq!(p["command"], "SELECT");
    assert_eq!(p["action"], "PERMISSIVE");
    assert_eq!(p["roles"][0], "authenticated");
    assert_eq!(p["definition"], "true");

    let (status, body) = call(
        &h.app,
        "GET",
        "/pg-meta/extensions",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let exts = body.as_array().unwrap();
    let plpgsql = exts.iter().find(|e| e["name"] == "plpgsql").unwrap();
    assert!(plpgsql["installed_version"].is_string());
    assert!(exts.iter().all(|e| e["runtime"].is_string()));

    let (status, body) = call(
        &h.app,
        "GET",
        "/pg-meta/roles",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let roles: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["name"].as_str())
        .collect();
    for expected in ["guardian", "anon", "authenticated", "service_role"] {
        assert!(
            roles.contains(&expected),
            "missing role {expected}: {roles:?}"
        );
    }

    let (status, body) = call(
        &h.app,
        "GET",
        "/pg-meta/schemas",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let schemas: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(schemas.contains(&"public"));

    // /types comes from the pg_type catalog view.
    let (status, body) = call(
        &h.app,
        "GET",
        "/pg-meta/types",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().iter().any(|t| t["name"] == "uuid"));

    // functions/triggers are honestly empty.
    for ep in ["/pg-meta/functions", "/pg-meta/triggers"] {
        let (status, body) = call(&h.app, "GET", ep, Some(&h.service), None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, json!([]));
    }
}

#[tokio::test]
async fn pg_meta_query_endpoint() {
    let h = harness().await;
    seed_pg_meta(&h.db).await;

    // SELECT rows come back as array-of-objects (Studio's SQL editor path).
    let (status, body) = call(
        &h.app,
        "POST",
        "/pg-meta/query",
        Some(&h.service),
        None,
        Some(
            json!({"query": "INSERT INTO authors VALUES (1, 'Ada', 'ada@x.io'); \
                    SELECT id, name FROM authors"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body, json!([{"id": 1, "name": "Ada"}]));

    // DDL yields an empty array.
    let (status, body) = call(
        &h.app,
        "POST",
        "/pg-meta/query",
        Some(&h.service),
        None,
        Some(json!({"query": "CREATE TABLE tmp_q (id int PRIMARY KEY)"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, json!([]));

    // Errors are {"error": {"message","code"}} with the SQLSTATE.
    let (status, body) = call(
        &h.app,
        "POST",
        "/pg-meta/query",
        Some(&h.service),
        None,
        Some(json!({"query": "SELECT * FROM does_not_exist"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "42P01");
    assert!(body["error"]["message"].is_string());

    // anon may not use the SQL editor.
    let (status, _b) = call(
        &h.app,
        "POST",
        "/pg-meta/query",
        Some(&h.anon),
        None,
        Some(json!({"query": "SELECT 1"})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ===========================================================================
// Realtime (real websocket over an ephemeral port)
// ===========================================================================

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn spawn_server(h: &Harness) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = h.app.clone();
    tokio::spawn(async move {
        axum::serve(listener, app.into_make_service())
            .await
            .unwrap();
    });
    addr
}

async fn ws_connect(addr: std::net::SocketAddr, apikey: &str) -> WsStream {
    let url = format!("ws://{addr}/realtime/v1/websocket?apikey={apikey}&vsn=1.0.0");
    let (ws, _resp) = tokio_tungstenite::connect_async(url).await.unwrap();
    ws
}

async fn ws_send(ws: &mut WsStream, v: Value) {
    ws.send(WsMessage::Text(v.to_string().into()))
        .await
        .unwrap();
}

/// Receive the next JSON text frame within `secs` seconds.
async fn ws_recv(ws: &mut WsStream, secs: u64) -> Option<Value> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(secs), ws.next())
            .await
            .ok()??
            .ok()?;
        match msg {
            WsMessage::Text(t) => return serde_json::from_str(t.as_str()).ok(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            _ => return None,
        }
    }
}

/// Read frames until one with `event` arrives (matching both the object form
/// `{"event": ...}` and the array form `[.., .., topic, event, payload]`), or
/// time out.
async fn ws_recv_event(ws: &mut WsStream, event: &str, secs: u64) -> Option<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        let frame = ws_recv(ws, secs).await?;
        let ev = frame
            .get("event")
            .or_else(|| frame.get(3))
            .and_then(Value::as_str)
            .unwrap_or("");
        if ev == event {
            return Some(frame);
        }
    }
    None
}

fn join_frame(topic: &str, reference: &str, config: Value, access_token: Option<&str>) -> Value {
    let mut payload = json!({ "config": config });
    if let Some(tok) = access_token {
        payload["access_token"] = json!(tok);
    }
    json!({
        "topic": topic,
        "event": "phx_join",
        "payload": payload,
        "ref": reference,
        "join_ref": reference,
    })
}

async fn seed_realtime_todos(db: &Arc<Database<MemoryStorage>>) {
    let mut s = Session::new(db.clone(), "postgres");
    s.execute("CREATE TABLE todos (id int PRIMARY KEY, title text, done boolean)")
        .await
        .unwrap();
}

#[tokio::test]
async fn realtime_connect_join_heartbeat_and_bad_key() {
    let h = harness().await;
    seed_realtime_todos(&h.db).await;
    let addr = spawn_server(&h).await;

    // A bad apikey is refused before the upgrade (typed 401).
    let url = format!("ws://{addr}/realtime/v1/websocket?apikey=bogus&vsn=1.0.0");
    let err = tokio_tungstenite::connect_async(url).await.err().unwrap();
    match err {
        tokio_tungstenite::tungstenite::Error::Http(resp) => {
            assert_eq!(resp.status().as_u16(), 401);
        }
        other => panic!("expected HTTP 401 error, got {other:?}"),
    }

    let mut ws = ws_connect(addr, &h.anon).await;

    // Join with a postgres_changes binding: reply carries server-assigned ids.
    ws_send(
        &mut ws,
        join_frame(
            "realtime:public:todos",
            "1",
            json!({"postgres_changes": [{"event": "*", "schema": "public", "table": "todos"}]}),
            None,
        ),
    )
    .await;
    let reply = ws_recv_event(&mut ws, "phx_reply", 5).await.unwrap();
    assert_eq!(reply["payload"]["status"], "ok", "{reply}");
    let changes = &reply["payload"]["response"]["postgres_changes"];
    assert!(changes[0]["id"].is_number(), "{reply}");
    assert_eq!(changes[0]["table"], "todos");

    // Heartbeat on the phoenix topic.
    ws_send(
        &mut ws,
        json!({"topic": "phoenix", "event": "heartbeat", "payload": {}, "ref": "2"}),
    )
    .await;
    let reply = ws_recv_event(&mut ws, "phx_reply", 5).await.unwrap();
    assert_eq!(reply["topic"], "phoenix");
    assert_eq!(reply["payload"]["status"], "ok");

    // Unsupported filter operators are typed join errors.
    ws_send(
        &mut ws,
        join_frame(
            "realtime:bad",
            "3",
            json!({"postgres_changes": [{"event": "*", "schema": "public", "table": "todos",
                    "filter": "id=gt.5"}]}),
            None,
        ),
    )
    .await;
    let reply = ws_recv_event(&mut ws, "phx_reply", 5).await.unwrap();
    assert_eq!(reply["payload"]["status"], "error");
    assert_eq!(
        reply["payload"]["response"]["code"],
        "SUPA_COMPAT_REALTIME_UNSUPPORTED_FILTER"
    );

    // The Phoenix array form works too; replies come back array-shaped.
    ws_send(&mut ws, json!(["4", "4", "phoenix", "heartbeat", {}])).await;
    let reply = ws_recv_event(&mut ws, "phx_reply", 5).await.unwrap();
    assert_eq!(reply[2], "phoenix");
    assert_eq!(reply[3], "phx_reply");
    assert_eq!(reply[4]["status"], "ok");
}

#[tokio::test]
async fn realtime_insert_update_delete_delivery_and_filters() {
    let h = harness().await;
    seed_realtime_todos(&h.db).await;
    let addr = spawn_server(&h).await;

    // Subscriber 1: everything on todos. Subscriber 2: only id=eq.2.
    let mut all = ws_connect(addr, &h.anon).await;
    ws_send(
        &mut all,
        join_frame(
            "realtime:all",
            "1",
            json!({"postgres_changes": [{"event": "*", "schema": "public", "table": "todos"}]}),
            None,
        ),
    )
    .await;
    assert_eq!(
        ws_recv_event(&mut all, "phx_reply", 5).await.unwrap()["payload"]["status"],
        "ok"
    );

    let mut filtered = ws_connect(addr, &h.anon).await;
    ws_send(
        &mut filtered,
        join_frame(
            "realtime:filtered",
            "1",
            json!({"postgres_changes": [{"event": "INSERT", "schema": "public",
                    "table": "todos", "filter": "id=eq.2"}]}),
            None,
        ),
    )
    .await;
    assert_eq!(
        ws_recv_event(&mut filtered, "phx_reply", 5).await.unwrap()["payload"]["status"],
        "ok"
    );

    // INSERT id=1: the unfiltered subscriber sees it; the filtered one must not.
    let mut s = Session::new(h.db.clone(), "postgres");
    s.execute("INSERT INTO todos VALUES (1, 'first', false)")
        .await
        .unwrap();
    let ev = ws_recv_event(&mut all, "postgres_changes", 5)
        .await
        .unwrap();
    let data = &ev["payload"]["data"];
    assert_eq!(data["eventType"], "INSERT");
    assert_eq!(data["type"], "INSERT");
    assert_eq!(data["schema"], "public");
    assert_eq!(data["table"], "todos");
    assert_eq!(data["new"]["id"], 1);
    assert_eq!(data["record"]["title"], "first");
    assert!(data["commit_timestamp"].is_string());
    assert!(ev["payload"]["ids"][0].is_number());
    assert!(
        ws_recv_event(&mut filtered, "postgres_changes", 1)
            .await
            .is_none(),
        "filter id=eq.2 must not match id=1"
    );

    // INSERT id=2: both see it.
    s.execute("INSERT INTO todos VALUES (2, 'second', false)")
        .await
        .unwrap();
    let ev = ws_recv_event(&mut filtered, "postgres_changes", 5)
        .await
        .unwrap();
    assert_eq!(ev["payload"]["data"]["new"]["id"], 2);
    let ev = ws_recv_event(&mut all, "postgres_changes", 5)
        .await
        .unwrap();
    assert_eq!(ev["payload"]["data"]["new"]["id"], 2);

    // UPDATE carries old and new.
    s.execute("UPDATE todos SET done = true WHERE id = 1")
        .await
        .unwrap();
    let ev = ws_recv_event(&mut all, "postgres_changes", 5)
        .await
        .unwrap();
    let data = &ev["payload"]["data"];
    assert_eq!(data["eventType"], "UPDATE");
    assert_eq!(data["new"]["done"], true);
    assert_eq!(data["old"]["done"], false);
    assert_eq!(data["old_record"]["id"], 1);

    // DELETE carries the old row (non-RLS table: delivered to anon).
    s.execute("DELETE FROM todos WHERE id = 2").await.unwrap();
    let ev = ws_recv_event(&mut all, "postgres_changes", 5)
        .await
        .unwrap();
    let data = &ev["payload"]["data"];
    assert_eq!(data["eventType"], "DELETE");
    assert_eq!(data["old"]["id"], 2);
}

#[tokio::test]
async fn realtime_rls_gates_delivery() {
    let h = harness().await;
    {
        let mut s = Session::new(h.db.clone(), "postgres");
        s.execute("CREATE TABLE notes (id int PRIMARY KEY, user_id text, body text)")
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
    }
    let addr = spawn_server(&h).await;
    let binding =
        json!({"postgres_changes": [{"event": "*", "schema": "public", "table": "notes"}]});

    // anon subscriber: RLS-enabled table, no anon policy → nothing delivered.
    let mut anon_ws = ws_connect(addr, &h.anon).await;
    ws_send(
        &mut anon_ws,
        join_frame("realtime:anon", "1", binding.clone(), None),
    )
    .await;
    ws_recv_event(&mut anon_ws, "phx_reply", 5).await.unwrap();

    // user A subscriber (join-payload access_token upgrades the connection).
    let token_a = user_token(UID_A);
    let mut a_ws = ws_connect(addr, &h.anon).await;
    ws_send(
        &mut a_ws,
        join_frame("realtime:a", "1", binding.clone(), Some(&token_a)),
    )
    .await;
    ws_recv_event(&mut a_ws, "phx_reply", 5).await.unwrap();

    // user B subscriber.
    let token_b = user_token(UID_B);
    let mut b_ws = ws_connect(addr, &h.anon).await;
    ws_send(
        &mut b_ws,
        join_frame("realtime:b", "1", binding.clone(), Some(&token_b)),
    )
    .await;
    ws_recv_event(&mut b_ws, "phx_reply", 5).await.unwrap();

    // service subscriber (bypass).
    let mut service_ws = ws_connect(addr, &h.service).await;
    ws_send(
        &mut service_ws,
        join_frame("realtime:svc", "1", binding, None),
    )
    .await;
    ws_recv_event(&mut service_ws, "phx_reply", 5)
        .await
        .unwrap();

    // Insert a row owned by A.
    let mut s = Session::new(h.db.clone(), "postgres");
    s.execute(&format!(
        "INSERT INTO notes VALUES (1, '{UID_A}', 'a private note')"
    ))
    .await
    .unwrap();

    // A receives their row; service receives it; anon and B receive nothing.
    let ev = ws_recv_event(&mut a_ws, "postgres_changes", 5)
        .await
        .unwrap();
    assert_eq!(ev["payload"]["data"]["new"]["user_id"], UID_A);
    let ev = ws_recv_event(&mut service_ws, "postgres_changes", 5)
        .await
        .unwrap();
    assert_eq!(ev["payload"]["data"]["new"]["id"], 1);
    assert!(
        ws_recv_event(&mut anon_ws, "postgres_changes", 1)
            .await
            .is_none(),
        "anon must not receive RLS-protected rows"
    );
    assert!(
        ws_recv_event(&mut b_ws, "postgres_changes", 1)
            .await
            .is_none(),
        "user B must not receive user A's rows"
    );
}

#[tokio::test]
async fn realtime_broadcast_between_subscribers() {
    let h = harness().await;
    let addr = spawn_server(&h).await;

    let mut a = ws_connect(addr, &h.anon).await;
    let mut b = ws_connect(addr, &h.anon).await;
    for (ws, r) in [(&mut a, "1"), (&mut b, "2")] {
        ws_send(ws, join_frame("realtime:room1", r, json!({}), None)).await;
        let reply = ws_recv_event(ws, "phx_reply", 5).await.unwrap();
        assert_eq!(reply["payload"]["status"], "ok");
    }

    // A broadcasts; B receives; A (self=false default) does not.
    ws_send(
        &mut a,
        json!({
            "topic": "realtime:room1",
            "event": "broadcast",
            "payload": {"type": "broadcast", "event": "cursor", "payload": {"x": 42}},
            "ref": "3",
        }),
    )
    .await;
    let ev = ws_recv_event(&mut b, "broadcast", 5).await.unwrap();
    assert_eq!(ev["topic"], "realtime:room1");
    assert_eq!(ev["payload"]["event"], "cursor");
    assert_eq!(ev["payload"]["payload"]["x"], 42);
    assert!(
        ws_recv_event(&mut a, "broadcast", 1).await.is_none(),
        "sender must not receive its own broadcast without self:true"
    );

    // Broadcasting to a topic that was never joined is a typed error.
    ws_send(
        &mut a,
        json!({
            "topic": "realtime:never-joined",
            "event": "broadcast",
            "payload": {},
            "ref": "4",
        }),
    )
    .await;
    let reply = ws_recv_event(&mut a, "phx_reply", 5).await.unwrap();
    assert_eq!(reply["payload"]["status"], "error");
    assert_eq!(
        reply["payload"]["response"]["code"],
        "SUPA_COMPAT_REALTIME_NOT_JOINED"
    );
}

// ===========================================================================
// Gateway: functions is live (typed 404 for undeployed slugs); graphql is live
// ===========================================================================

#[tokio::test]
async fn functions_returns_typed_not_found_and_graphql_is_live() {
    let h = harness().await;
    let (status, body) = call(
        &h.app,
        "POST",
        "/functions/v1/hello",
        Some(&h.anon),
        None,
        None,
    )
    .await;
    // Functions layer is now wired (see tests/supabase_functions.rs). An
    // undeployed slug renders the typed 404 shape, not the old 501 stub.
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_NOT_FOUND");

    // GraphQL is implemented (see tests/supabase_graphql.rs): a real request
    // executes and returns GraphQL-shaped JSON with HTTP 200.
    let (status, body) = call(
        &h.app,
        "POST",
        "/graphql/v1",
        Some(&h.anon),
        None,
        Some(json!({"query": "{ __typename }"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["__typename"], "Query");
}
