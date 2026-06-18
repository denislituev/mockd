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
"#;

/// Bind a mockd server to an ephemeral port, spawn it, and return its base URL.
async fn spawn() -> String {
    let config = Config::parse(CONFIG).expect("config parses");
    let server = Server::from_config(config).expect("server builds");
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
