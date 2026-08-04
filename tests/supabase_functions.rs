//! Integration tests for the Supabase Functions compatibility layer.
//!
//! Drives the axum `Router` directly with `tower::ServiceExt::oneshot` over a
//! `MemoryStorage`-backed `Database` — no ports are bound and no WASM is
//! executed here. WASM-invocation tests belong under `--features
//! "supabase,compute"` and are gated separately.

#![cfg(feature = "supabase")]

use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request, StatusCode};
use base64::Engine as _;
use serde_json::{Value, json};
use tower::ServiceExt;

use guardian_db::sql::MemoryStorage;
use guardian_db::sql::engine::Database;
use guardian_db::supabase::project::ProjectKeys;
use guardian_db::supabase::{AppState, ServiceConfig, SupabaseCompatProject, build_router};

const TEST_SECRET: &str = "integration-test-jwt-secret-value-0123456789";
const IAT: i64 = 1_700_000_000;

struct Harness {
    app: Router,
    anon: String,
    service: String,
    #[allow(dead_code)]
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

async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    apikey: Option<&str>,
    bearer: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(k) = apikey {
        builder = builder.header("apikey", k);
    }
    if let Some(b) = bearer {
        builder = builder.header("authorization", format!("Bearer {b}"));
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
    let headers = resp.headers().clone();
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    (status, headers, bytes.to_vec())
}

/// A minimal valid WASM module: magic + version + one empty type section.
/// It has no exports so instantiation succeeds but invoking `handler` fails
/// with `MissingExport` — enough to prove the deploy path stores/retrieves
/// the bundle and boot-time validation runs.
fn minimal_wasm() -> Vec<u8> {
    // \0asm, version 1
    b"\0asm\x01\x00\x00\x00".to_vec()
}

// ---------------------------------------------------------------------------
// Admin CRUD
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_requires_service_role() {
    let h = harness().await;
    let (status, _, body) = call(
        &h.app,
        "GET",
        "/functions/v1/_admin/functions",
        Some(&h.anon),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_FORBIDDEN");
}

#[tokio::test]
async fn admin_list_empty() {
    let h = harness().await;
    let (status, _, body) = call(
        &h.app,
        "GET",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn admin_create_and_get() {
    let h = harness().await;
    let (status, _, body) = call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "hello", "name": "Hello world", "verify_jwt": false })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["slug"], "hello");
    assert_eq!(body["verify_jwt"], false);
    assert_eq!(body["status"], "PENDING");
    assert_eq!(body["version"], 1);

    let (status, _, body) = call(
        &h.app,
        "GET",
        "/functions/v1/_admin/functions/hello",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["slug"], "hello");

    let (status, _, list) = call(
        &h.app,
        "GET",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let arr = list.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["slug"], "hello");
}

#[tokio::test]
async fn admin_create_duplicate_slug_conflict() {
    let h = harness().await;
    let (status, _, _) = call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "dup" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let (status, _, body) = call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "dup" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_BAD_REQUEST");
}

#[tokio::test]
async fn admin_bad_slug_is_rejected() {
    let h = harness().await;
    let (status, _, body) = call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "has space" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_BAD_REQUEST");
}

#[tokio::test]
async fn admin_bad_wasm_body_is_rejected() {
    let h = harness().await;
    let b64_junk = base64::engine::general_purpose::STANDARD.encode(b"not wasm");
    let (status, _, body) = call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "bad", "body": b64_junk })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_BAD_REQUEST");
}

#[tokio::test]
async fn admin_deploy_body_via_put() {
    let h = harness().await;
    call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "hello" })),
    )
    .await;

    let wasm = minimal_wasm();
    let (status, _, body) = call_raw(
        &h.app,
        "PUT",
        "/functions/v1/_admin/functions/hello/body",
        &[
            ("apikey", h.service.as_str()),
            ("content-type", "application/wasm"),
        ],
        Some(wasm.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let parsed: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(parsed["status"], "ACTIVE");
    assert_eq!(parsed["version"], 2);

    let (status, _, got) = call_raw(
        &h.app,
        "GET",
        "/functions/v1/_admin/functions/hello/body",
        &[("apikey", h.service.as_str())],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(got, wasm);
}

#[tokio::test]
async fn admin_update_metadata() {
    let h = harness().await;
    call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "hello", "verify_jwt": true })),
    )
    .await;
    let (status, _, body) = call(
        &h.app,
        "PATCH",
        "/functions/v1/_admin/functions/hello",
        Some(&h.service),
        None,
        Some(json!({ "name": "Renamed", "verify_jwt": false })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "Renamed");
    assert_eq!(body["verify_jwt"], false);
    assert_eq!(body["version"], 1);
}

#[tokio::test]
async fn admin_delete_function() {
    let h = harness().await;
    call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "gone" })),
    )
    .await;
    let (status, _, _) = call(
        &h.app,
        "DELETE",
        "/functions/v1/_admin/functions/gone",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, body) = call(
        &h.app,
        "GET",
        "/functions/v1/_admin/functions/gone",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_NOT_FOUND");
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn secret_lifecycle() {
    let h = harness().await;
    call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "envy" })),
    )
    .await;

    // Empty list first.
    let (status, _, body) = call(
        &h.app,
        "GET",
        "/functions/v1/_admin/functions/envy/secrets",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());

    // Upsert.
    let (status, _, body) = call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions/envy/secrets",
        Some(&h.service),
        None,
        Some(json!({ "name": "OPENAI_KEY", "value": "sk-abc123" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "OPENAI_KEY");

    // List (values never surfaced).
    let (status, _, body) = call(
        &h.app,
        "GET",
        "/functions/v1/_admin/functions/envy/secrets",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["name"], "OPENAI_KEY");
    assert!(arr[0].get("value").is_none(), "secret value must not leak");

    // Delete single.
    let (status, _, _) = call(
        &h.app,
        "DELETE",
        "/functions/v1/_admin/functions/envy/secrets/OPENAI_KEY",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _, body) = call(
        &h.app,
        "GET",
        "/functions/v1/_admin/functions/envy/secrets",
        Some(&h.service),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn secret_bad_name_rejected() {
    let h = harness().await;
    call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "envy2" })),
    )
    .await;
    let (status, _, body) = call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions/envy2/secrets",
        Some(&h.service),
        None,
        Some(json!({ "name": "1BADNAME", "value": "x" })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_BAD_REQUEST");
}

// ---------------------------------------------------------------------------
// Invocation (metadata / gating; WASM execution is gated behind `compute`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn invoke_unknown_slug_is_404_typed() {
    let h = harness().await;
    let (status, _, body) = call(
        &h.app,
        "POST",
        "/functions/v1/never-deployed",
        Some(&h.anon),
        None,
        Some(json!({ "hi": "there" })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_NOT_FOUND");
}

#[tokio::test]
async fn invoke_verify_jwt_requires_bearer() {
    let h = harness().await;
    // Deploy with verify_jwt=true and no body (so it never runs; we only
    // exercise the JWT gate).
    call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({ "slug": "locked", "verify_jwt": true })),
    )
    .await;

    let (status, _, body) = call(
        &h.app,
        "POST",
        "/functions/v1/locked",
        Some(&h.anon),
        None, // no bearer
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_FORBIDDEN");
}

#[tokio::test]
async fn invoke_options_is_cors_preflight() {
    let h = harness().await;
    // No apikey needed for preflight? Apikey middleware still gates the
    // whole nested router, so include it. Browsers do skip auth on preflight
    // via the OPTIONS-without-credentials shape, and Supabase-hosted platform
    // sits behind Kong which passes OPTIONS through — our layer is stricter
    // by design (documented) and still returns the correct preflight body
    // once authenticated.
    let (status, headers, _) = call_raw(
        &h.app,
        "OPTIONS",
        "/functions/v1/hello",
        &[
            ("apikey", h.anon.as_str()),
            ("access-control-request-method", "POST"),
            ("access-control-request-headers", "authorization, apikey"),
        ],
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(headers.get("access-control-allow-methods").is_some());
    assert!(headers.get("access-control-allow-headers").is_some());
}

#[tokio::test]
async fn invoke_no_body_is_500_boot_error_without_compute() {
    // With no `compute` feature the runtime substrate is off, and
    // `run_guest_blocking` returns a boot error. But because the function
    // has no deployed body, we hit the 404 path first — which is the
    // correct behavior. Exercise both: deploy with an empty stub body,
    // then invoke.
    let h = harness().await;
    call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({
            "slug": "noop",
            "verify_jwt": false,
            "body": base64::engine::general_purpose::STANDARD.encode(minimal_wasm())
        })),
    )
    .await;
    let (status, _, body) = call(
        &h.app,
        "POST",
        "/functions/v1/noop",
        Some(&h.anon),
        None,
        Some(json!({})),
    )
    .await;
    // Without `compute` this returns a boot error; with `compute` the
    // minimal module lacks the `handler` export and gets a boot error too.
    // Either way, the wire shape is `SUPA_COMPAT_FUNCTION_BOOT_ERROR`.
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_BOOT_ERROR");
}

// ---------------------------------------------------------------------------
// End-to-end WASM execution (requires `compute` feature)
// ---------------------------------------------------------------------------

/// A hand-crafted WASM guest that implements the full Functions ABI: `memory`,
/// `gdb_alloc(len) -> i32`, and `handler(ptr, len) -> i64` returning the
/// packed `(out_ptr << 32) | out_len` for a pre-baked CBOR-encoded
/// `InvocationOutput { status: 200, headers: [], body_b64: <base64 of "hi
/// from wasm"> }`.
///
/// The output CBOR is generated at test-authoring time with `ciborium` and
/// embedded as raw bytes in a `data` section — the guest just returns its
/// stable offset and length. Kept minimal on purpose: a real deployed
/// function would parse the input envelope, do work, and marshal its own
/// output; this proves the wire glue works end-to-end without pulling in a
/// full Rust WASM build for the test.
#[cfg(feature = "compute")]
fn echo_wasm() -> Vec<u8> {
    use serde::Serialize;

    #[derive(Serialize)]
    struct Out<'a> {
        status: u16,
        headers: &'a [(String, String)],
        body_b64: String,
    }

    let body_b64 = base64::engine::general_purpose::STANDARD.encode(b"hi from wasm");
    let out = Out {
        status: 200,
        headers: &[],
        body_b64,
    };
    let mut cbor = Vec::new();
    ciborium::ser::into_writer(&out, &mut cbor).unwrap();
    // The output CBOR sits at a fixed offset (2048); alloc bumps from 4096
    // so nothing overwrites it. `handler` returns (2048 << 32) | len.
    let offset: u32 = 2048;
    let len: u32 = cbor.len() as u32;
    let packed: u64 = ((offset as u64) << 32) | (len as u64);
    // Encode `packed` as a WAT `i64.const` literal (decimal is fine).
    let hex_bytes: String = cbor
        .iter()
        .map(|b| format!("\\{:02x}", b))
        .collect::<Vec<_>>()
        .join("");
    let wat = format!(
        r#"
        (module
          (memory (export "memory") 1)
          (data (i32.const {offset}) "{bytes}")
          (global $next (mut i32) (i32.const 4096))
          (func (export "gdb_alloc") (param $len i32) (result i32)
            (local $ptr i32)
            (local.set $ptr (global.get $next))
            (global.set $next (i32.add (global.get $next) (local.get $len)))
            (local.get $ptr))
          (func (export "handler") (param $ptr i32) (param $len i32) (result i64)
            (i64.const {packed})))
        "#,
        offset = offset,
        bytes = hex_bytes,
        packed = packed,
    );
    wat::parse_str(&wat).expect("valid WAT for echo guest")
}

#[cfg(feature = "compute")]
#[tokio::test]
async fn invoke_end_to_end_wasm_returns_response() {
    let h = harness().await;
    let wasm = echo_wasm();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&wasm);
    let (status, _, _) = call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({
            "slug": "wasm-echo",
            "verify_jwt": false,
            "body": b64
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, _, body_bytes) = call_raw(
        &h.app,
        "POST",
        "/functions/v1/wasm-echo",
        &[
            ("apikey", h.anon.as_str()),
            ("content-type", "application/json"),
        ],
        Some(br#"{"name":"world"}"#.to_vec()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body_bytes.as_slice(), b"hi from wasm");
}

#[cfg(feature = "compute")]
#[tokio::test]
async fn invoke_missing_handler_export_is_boot_error() {
    let h = harness().await;
    // Valid WASM but no `handler` export → BootError with MissingExport
    // trap surfaced.
    let no_handler_wat = r#"
        (module
          (memory (export "memory") 1)
          (func (export "gdb_alloc") (param i32) (result i32) (i32.const 1024)))
    "#;
    let wasm = wat::parse_str(no_handler_wat).unwrap();
    call(
        &h.app,
        "POST",
        "/functions/v1/_admin/functions",
        Some(&h.service),
        None,
        Some(json!({
            "slug": "no-handler",
            "verify_jwt": false,
            "body": base64::engine::general_purpose::STANDARD.encode(&wasm)
        })),
    )
    .await;

    let (status, _, body) = call(
        &h.app,
        "POST",
        "/functions/v1/no-handler",
        Some(&h.anon),
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "SUPA_COMPAT_FUNCTION_BOOT_ERROR");
}
