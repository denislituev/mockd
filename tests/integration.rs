//! End-to-end integration tests.
//!
//! These spin up a real mockd server on an ephemeral port and exercise it over
//! HTTP using `reqwest`. They cover routing, path parameters, query/header/body
//! matching, templates, status codes, delays and `close_connection`.

use std::time::Duration;

use serde_json::{json, Value};

use mockd::config::Config;
use mockd::server::Server;

const CONFIG: &str = r#"
listen: "127.0.0.1:0"
routes:
  - method: GET
    path: /health
    response:
      status: 200
      body:
        ok: true

  - method: GET
    path: /users/{id}
    response:
      status: 200
      body:
        id: "{{path.id}}"
        name: "User {{path.id}}"

  - method: GET
    path: /users
    when:
      query:
        role: admin
    response:
      status: 200
      headers:
        Cache-Control: no-store
      body:
        admin: true

  - method: GET
    path: /users
    response:
      status: 200
      body:
        admin: false

  - method: GET
    path: /tenants/me
    when:
      headers:
        X-Tenant-Id: tenant-a
    response:
      status: 200
      body:
        tenant: "{{header.X-Tenant-Id}}"

  - method: POST
    path: /login
    when:
      body:
        username: admin
    response:
      status: 200
      body:
        token: admin-token

  - method: POST
    path: /login
    response:
      status: 401
      body:
        error: invalid credentials

  - method: DELETE
    path: /users/{id}
    response:
      status: 204

  - method: GET
    path: /slow
    response:
      status: 200
      delay: 200ms
      body:
        ok: true

  # Sequence: returns 500 twice, then 200 forever. Used to test retry logic.
  - method: GET
    path: /flaky
    response:
      sequence:
        - status: 500
          body:
            error: transient
        - status: 500
          body:
            error: transient
        - status: 200
          body:
            ok: true

  # Body templating: echo the request body back in the response.
  - method: POST
    path: /echo
    response:
      status: 201
      body:
        id: "{{body.id}}"
        name: "{{body.name}}"
        echoed: true

  # Helper functions: fresh values per call.
  - method: GET
    path: /fresh
    response:
      status: 200
      body:
        id: "{{uuid}}"
        created_at: "{{now}}"
        priority: "{{randomInt(1,5)}}"
"#;

/// Bind a mockd server to an ephemeral port, spawn it, and return its base URL.
async fn spawn() -> String {
    spawn_with(false).await
}

/// Like [`spawn`] but with CORS enabled.
async fn spawn_cors() -> String {
    spawn_with(true).await
}

async fn spawn_with(cors: bool) -> String {
    let config = Config::parse(CONFIG).expect("config parses");
    let server = Server::from_config(config)
        .expect("server builds")
        .with_cors(cors);
    let app = server.app();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("binds");
    let addr = listener.local_addr().expect("has addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("server runs");
    });

    format!("http://{addr}")
}

async fn body(resp: reqwest::Response) -> Value {
    resp.json::<Value>().await.expect("json body")
}

#[tokio::test]
async fn health_returns_ok() {
    let base = spawn().await;
    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(body(resp).await, json!({"ok": true}));
}

#[tokio::test]
async fn path_param_is_templated_and_coerced_to_number() {
    let base = spawn().await;
    let resp = reqwest::get(format!("{base}/users/42")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let v = body(resp).await;
    // {{path.id}} is a whole-string expression -> coerced to a JSON number.
    assert_eq!(v["id"], json!(42));
    // Embedded in a larger string -> interpolated as text.
    assert_eq!(v["name"], json!("User 42"));
}

#[tokio::test]
async fn query_matcher_disambiguates_same_path() {
    let base = spawn().await;

    let with_role = reqwest::get(format!("{base}/users?role=admin"))
        .await
        .unwrap();
    assert_eq!(with_role.status(), 200);
    assert_eq!(with_role.headers()["cache-control"], "no-store");
    assert_eq!(body(with_role).await, json!({"admin": true}));

    let without_role = reqwest::get(format!("{base}/users")).await.unwrap();
    assert_eq!(without_role.status(), 200);
    assert_eq!(body(without_role).await, json!({"admin": false}));
}

#[tokio::test]
async fn header_matcher_works_case_insensitively() {
    let base = spawn().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/tenants/me"))
        .header("x-tenant-id", "tenant-a") // lower-case header in request
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(body(resp).await, json!({"tenant": "tenant-a"}));
}

#[tokio::test]
async fn header_matcher_rejects_wrong_value() {
    let base = spawn().await;
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("{base}/tenants/me"))
        .header("X-Tenant-Id", "tenant-b")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn body_matcher_selects_route() {
    let base = spawn().await;
    let client = reqwest::Client::new();

    let ok = client
        .post(format!("{base}/login"))
        .json(&json!({"username": "admin", "password": "secret"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), 200);
    assert_eq!(body(ok).await, json!({"token": "admin-token"}));

    let bad = client
        .post(format!("{base}/login"))
        .json(&json!({"username": "guest"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
}

#[tokio::test]
async fn delete_returns_204_with_empty_body() {
    let base = spawn().await;
    let client = reqwest::Client::new();
    let resp = client
        .delete(format!("{base}/users/7"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    let text = resp.text().await.unwrap();
    assert!(text.is_empty());
}

#[tokio::test]
async fn unmatched_route_is_404() {
    let base = spawn().await;
    let resp = reqwest::get(format!("{base}/nope")).await.unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn delay_is_applied() {
    let base = spawn().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let start = std::time::Instant::now();
    let resp = client.get(format!("{base}/slow")).send().await.unwrap();
    let elapsed = start.elapsed();
    assert_eq!(resp.status(), 200);
    assert!(
        elapsed >= Duration::from_millis(180),
        "expected delay, elapsed={elapsed:?}"
    );
}

#[tokio::test]
async fn default_content_type_is_json() {
    let base = spawn().await;
    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json"
    );
}

// ---------------------------------------------------------------------------
// Sequence responses
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sequence_returns_responses_in_order() {
    let base = spawn().await;
    let client = reqwest::Client::new();

    // First two calls fail.
    for _ in 0..2 {
        let resp = client.get(format!("{base}/flaky")).send().await.unwrap();
        assert_eq!(resp.status(), 500);
        assert_eq!(body(resp).await, json!({"error": "transient"}));
    }

    // From the third call onward, the success response sticks.
    for _ in 0..3 {
        let resp = client.get(format!("{base}/flaky")).send().await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(body(resp).await, json!({"ok": true}));
    }
}

// ---------------------------------------------------------------------------
// Body templating
// ---------------------------------------------------------------------------

#[tokio::test]
async fn body_template_echoes_request_fields() {
    let base = spawn().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{base}/echo"))
        .json(&json!({"id": 99, "name": "alice", "extra": "ignored"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    assert_eq!(
        body(resp).await,
        json!({"id": 99, "name": "alice", "echoed": true})
    );
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn helper_functions_produce_realistic_values() {
    let base = spawn().await;
    let resp = reqwest::get(format!("{base}/fresh")).await.unwrap();
    assert_eq!(resp.status(), 200);
    let v = body(resp).await;

    // `id` is a 36-char UUID string.
    let id = v["id"].as_str().expect("id is a string");
    assert_eq!(id.len(), 36);
    assert_eq!(id.chars().filter(|&c| c == '-').count(), 4);

    // `created_at` matches YYYY-MM-DDTHH:MM:SSZ (20 chars).
    let ts = v["created_at"].as_str().expect("created_at is a string");
    assert_eq!(ts.len(), 20);
    assert!(ts.ends_with('Z'));

    // `priority` is a number in [1, 5].
    let priority = v["priority"].as_i64().expect("priority is a number");
    assert!((1..=5).contains(&priority));
}

#[tokio::test]
async fn helper_functions_produce_unique_uuids_per_call() {
    let base = spawn().await;
    let a = body(reqwest::get(format!("{base}/fresh")).await.unwrap()).await;
    let b = body(reqwest::get(format!("{base}/fresh")).await.unwrap()).await;
    assert_ne!(a["id"], b["id"]);
}

// ---------------------------------------------------------------------------
// CORS
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cors_enabled_adds_allow_origin_header() {
    let base = spawn_cors().await;
    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers().get("access-control-allow-origin").unwrap(),
        "*"
    );
}

#[tokio::test]
async fn cors_enabled_handles_preflight() {
    let base = spawn_cors().await;
    let client = reqwest::Client::new();
    let resp = client
        .request(reqwest::Method::OPTIONS, format!("{base}/users"))
        .header("Origin", "https://example.com")
        .header("Access-Control-Request-Method", "POST")
        .header("Access-Control-Request-Headers", "Content-Type")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204);
    assert_eq!(
        resp.headers().get("access-control-allow-origin").unwrap(),
        "*"
    );
    assert!(resp
        .headers()
        .get("access-control-allow-methods")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("POST"));
    assert_eq!(
        resp.headers().get("access-control-allow-headers").unwrap(),
        "Content-Type"
    );
}

#[tokio::test]
async fn cors_disabled_does_not_add_headers() {
    let base = spawn().await;
    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers().get("access-control-allow-origin").is_none());
}
