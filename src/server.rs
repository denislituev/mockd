//! HTTP server layer (Axum).
//!
//! [`Server`] wires a compiled [`Router`] to an Axum
//! application. A single fallback handler receives every request, delegates
//! matching to the router, applies any configured delay, renders template
//! expressions in the response body and produces the HTTP response.
//!
//! Optional CORS support adds permissive cross-origin headers and handles
//! `OPTIONS` preflight requests before route matching.
//!
//! For testing, [`build_app`] returns a plain `axum::Router` that can be bound
//! to any address (including an ephemeral one).

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method as AxumMethod, Response, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::any;
use serde_json::Value;

use crate::config::{Config, Method};
use crate::router::{Match, Router, RouterError};
use crate::template::{render, TemplateContext};

/// A mockd server bound to a configuration.
#[derive(Debug, Clone)]
pub struct Server {
    router: Router,
    listen: String,
    cors: bool,
}

/// Errors that can occur while building or running a [`Server`].
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// The routes could not be compiled.
    #[error("invalid routes: {0}")]
    Router(#[from] RouterError),

    /// The configured listen address could not be bound.
    #[error("could not bind to {addr}: {source}")]
    Bind {
        addr: String,
        #[source]
        source: std::io::Error,
    },

    /// The server stopped with an error.
    #[error("server error: {0}")]
    Serve(#[source] std::io::Error),

    /// A response status code was invalid.
    #[error("invalid status code: {0}")]
    InvalidStatus(u16),
}

impl Server {
    /// Build a server from a parsed [`Config`].
    ///
    /// CORS is disabled by default; use [`Server::with_cors`] to enable it.
    pub fn from_config(config: Config) -> Result<Self, ServerError> {
        let router = Router::new(config.routes)?;
        Ok(Server {
            router,
            listen: config.listen,
            cors: false,
        })
    }

    /// Enable or disable permissive CORS support.
    pub fn with_cors(mut self, enabled: bool) -> Self {
        self.cors = enabled;
        self
    }

    /// Build the Axum application for this server.
    ///
    /// Exposed primarily for integration tests; [`Server::serve`] is the
    /// normal entry point.
    pub fn app(&self) -> axum::Router {
        build_app(self.router.clone(), self.cors)
    }

    /// Number of compiled routes.
    pub fn route_count(&self) -> usize {
        self.router.len()
    }

    /// Bind the configured address and serve until interrupted.
    pub async fn serve<F>(&self, shutdown: F) -> Result<(), ServerError>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let listen = normalize_listen(&self.listen);
        let listener = tokio::net::TcpListener::bind(&listen)
            .await
            .map_err(|source| ServerError::Bind {
                addr: listen.clone(),
                source,
            })?;
        let addr = listener_local_addr(&listener);
        tracing::info!("listening on {addr}");
        let app = self.app();
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await
            .map_err(ServerError::Serve)?;
        Ok(())
    }
}

/// Normalize a listen address so that the common shorthand `:PORT` binds to
/// all interfaces, matching the convention used by Go/nginx and others.
///
/// - `:8080`       -> `0.0.0.0:8080`
/// - `127.0.0.1:8080` -> unchanged
/// - `[::1]:8080`  -> unchanged
fn normalize_listen(addr: &str) -> String {
    if let Some(rest) = addr.strip_prefix(':') {
        format!("0.0.0.0:{rest}")
    } else {
        addr.to_string()
    }
}

fn listener_local_addr(listener: &tokio::net::TcpListener) -> String {
    listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| "(unknown)".to_string())
}

/// Build an Axum application backed by the given router.
///
/// When `cors` is `true`, every response carries permissive CORS headers and
/// `OPTIONS` preflight requests are answered with `204 No Content` without
/// being forwarded to the router.
pub fn build_app(router: Router, cors: bool) -> axum::Router {
    let state = Arc::new(AppState { router, cors });
    axum::Router::new().fallback(any(handler)).with_state(state)
}

/// Shared per-application state passed to the handler.
#[derive(Clone)]
struct AppState {
    router: Router,
    cors: bool,
}

/// The single request handler that backs every mockd route.
async fn handler(
    State(state): State<Arc<AppState>>,
    method: AxumMethod,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let method_str = method.as_str().to_string();
    let path = uri.path().to_string();

    // CORS preflight: must be handled before route matching because OPTIONS
    // is not part of mockd's Method enum.
    if state.cors
        && method == AxumMethod::OPTIONS
        && headers.contains_key("access-control-request-method")
    {
        tracing::info!(%method_str, %path, status = 204, "cors preflight");
        return cors_preflight(&headers);
    }

    // HEAD is answered by GET routes (RFC 9110): same routing, status and
    // headers, no body.
    let core_method = if method == AxumMethod::HEAD {
        Method::Get
    } else {
        let Some(core_method) = Method::from_http_str(method.as_str()) else {
            tracing::info!(%method_str, %path, status = 404, "unsupported method");
            return not_found(state.cors);
        };
        core_method
    };

    let query = parse_query(uri.query().unwrap_or(""));
    let header_map = collect_headers(&headers);
    let request_body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);

    let Some(Match {
        path_params,
        response,
    }) = state
        .router
        .resolve(core_method, &path, &query, &header_map, &request_body)
    else {
        tracing::info!(%method_str, %path, status = 404, "no matching route");
        return not_found(state.cors);
    };

    // Optional artificial delay (used to test timeouts).
    if let Some(delay) = response.delay {
        tracing::debug!(?delay, "applying artificial delay");
        tokio::time::sleep(delay).await;
    }

    // Render template expressions in the body using the request context.
    let rendered = response.body.map(|b| {
        let ctx = TemplateContext {
            path: path_params.clone(),
            query: query.clone(),
            headers: header_map.clone(),
            body: request_body.clone(),
        };
        render(&b, &ctx)
    });

    let status = response.status;
    let close_connection = response.close_connection;

    let mut resp = build_response(status, &response.headers, rendered, close_connection)
        .unwrap_or_else(|_| internal_error());

    if state.cors {
        add_cors_headers(resp.headers_mut());
    }

    tracing::info!(%method_str, %path, status, "handled");
    resp
}

/// Parse a raw query string into a map.
///
/// Keys and values are percent-decoded (`+` stays a literal plus sign).
/// Keys without a value map to an empty string.
fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    if query.is_empty() {
        return map;
    }
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        map.insert(percent_decode(k), percent_decode(v));
    }
    map
}

/// Percent-decode a query key or value; invalid escapes pass through.
fn percent_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .to_string()
}

/// Collect request headers into a map with lower-cased keys.
fn collect_headers(headers: &HeaderMap) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for (name, value) in headers.iter() {
        let key = name.as_str().to_ascii_lowercase();
        let val = value.to_str().unwrap_or("").to_string();
        map.entry(key).or_insert(val);
    }
    map
}

/// Build an HTTP response from the mockd response definition.
fn build_response(
    status: u16,
    headers: &HashMap<String, String>,
    body: Option<Value>,
    close_connection: bool,
) -> Result<Response<Body>, ServerError> {
    let status = StatusCode::from_u16(status).map_err(|_| ServerError::InvalidStatus(status))?;

    let mut builder = Response::builder().status(status);

    let has_content_type = headers
        .keys()
        .any(|k| k.eq_ignore_ascii_case("content-type"));

    for (name, value) in headers {
        builder = builder.header(name.as_str(), value.as_str());
    }

    if close_connection {
        builder = builder.header("connection", "close");
    }

    let bytes = if let Some(body) = body {
        if !has_content_type {
            builder = builder.header("content-type", "application/json");
        }
        serde_json::to_vec(&body).unwrap_or_default()
    } else {
        Vec::new()
    };

    Ok(builder.body(Body::from(bytes)).unwrap())
}

/// Append the permissive CORS headers used for non-preflight responses.
fn add_cors_headers(headers: &mut HeaderMap) {
    headers.insert("access-control-allow-origin", HeaderValue::from_static("*"));
    // The response varies by Origin, even though we always echo `*`, so that
    // caches do not return a CORS-configured response to a non-CORS request.
    headers.insert("vary", HeaderValue::from_static("origin"));
}

/// Build a `204 No Content` response for a CORS preflight.
///
/// The allowed request headers are echoed from `Access-Control-Request-Headers`
/// if present, otherwise `*` is advertised.
fn cors_preflight(req_headers: &HeaderMap) -> Response<Body> {
    let allow_headers = req_headers
        .get("access-control-request-headers")
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("*"));

    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("access-control-allow-origin", "*")
        .header(
            "access-control-allow-methods",
            "GET, POST, PUT, PATCH, DELETE, OPTIONS",
        )
        .header("access-control-allow-headers", allow_headers)
        .header("access-control-max-age", "86400")
        .header("vary", "origin")
        .body(Body::empty())
        .unwrap()
}

fn not_found(cors: bool) -> Response<Body> {
    let mut resp = (
        StatusCode::NOT_FOUND,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        r#"{"error":"no matching route"}"#,
    )
        .into_response();
    if cors {
        add_cors_headers(resp.headers_mut());
    }
    resp
}

fn internal_error() -> Response<Body> {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        r#"{"error":"internal mockd error"}"#,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_basic() {
        let q = parse_query("role=admin&tenant=a&flag");
        assert_eq!(q.get("role").unwrap(), "admin");
        assert_eq!(q.get("tenant").unwrap(), "a");
        assert_eq!(q.get("flag").unwrap(), "");
    }

    #[test]
    fn parse_query_empty() {
        assert!(parse_query("").is_empty());
    }

    #[test]
    fn parse_query_percent_decodes_keys_and_values() {
        let q = parse_query("name=John%20Doe&caf%C3%A9=menu");
        assert_eq!(q.get("name").unwrap(), "John Doe");
        assert_eq!(q.get("café").unwrap(), "menu");
    }

    #[test]
    fn parse_query_leaves_plus_literal() {
        let q = parse_query("token=a+b");
        assert_eq!(q.get("token").unwrap(), "a+b");
    }

    #[test]
    fn parse_query_decodes_encoded_plus() {
        let q = parse_query("token=a%2Bb");
        assert_eq!(q.get("token").unwrap(), "a+b");
    }

    #[test]
    fn parse_query_invalid_escapes_pass_through() {
        let q = parse_query("x=100%ZZ");
        assert_eq!(q.get("x").unwrap(), "100%ZZ");
    }

    #[test]
    fn collect_headers_lowercases() {
        let mut hm = HeaderMap::new();
        hm.insert("X-Tenant-Id", "a".parse().unwrap());
        let m = collect_headers(&hm);
        assert_eq!(m.get("x-tenant-id").unwrap(), "a");
    }

    #[test]
    fn build_response_sets_json_content_type_when_body_present() {
        let resp = build_response(
            200,
            &HashMap::new(),
            Some(serde_json::json!({"ok": true})),
            false,
        )
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap(),
            "application/json"
        );
    }

    #[test]
    fn build_response_keeps_explicit_content_type() {
        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_string(), "text/plain".to_string());
        let resp = build_response(200, &headers, Some(Value::String("hi".into())), false).unwrap();
        assert_eq!(
            resp.headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap(),
            "text/plain"
        );
    }

    #[test]
    fn build_response_close_connection_header() {
        let resp = build_response(500, &HashMap::new(), None, true).unwrap();
        assert_eq!(
            resp.headers().get("connection").unwrap().to_str().unwrap(),
            "close"
        );
    }

    #[test]
    fn build_response_rejects_invalid_status() {
        // Status codes must be in the 100..=999 range; 6000 is rejected by
        // the underlying `StatusCode::from_u16`.
        let err = build_response(6000, &HashMap::new(), None, false).unwrap_err();
        assert!(matches!(err, ServerError::InvalidStatus(6000)));
    }

    #[test]
    fn normalize_listen_handles_shorthand() {
        assert_eq!(normalize_listen(":8080"), "0.0.0.0:8080");
        assert_eq!(normalize_listen("127.0.0.1:9000"), "127.0.0.1:9000");
        assert_eq!(normalize_listen("[::1]:8080"), "[::1]:8080");
    }

    #[test]
    fn cors_preflight_has_cors_headers() {
        let resp = cors_preflight(&HeaderMap::new());
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-origin")
                .unwrap()
                .to_str()
                .unwrap(),
            "*"
        );
        assert!(resp
            .headers()
            .get("access-control-allow-methods")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("GET"));
        // No Access-Control-Request-Headers -> default to "*".
        assert_eq!(
            resp.headers()
                .get("access-control-allow-headers")
                .unwrap()
                .to_str()
                .unwrap(),
            "*"
        );
    }

    #[test]
    fn cors_preflight_echoes_requested_headers() {
        let mut req = HeaderMap::new();
        req.insert(
            "access-control-request-headers",
            "X-Tenant-Id, Authorization".parse().unwrap(),
        );
        let resp = cors_preflight(&req);
        assert_eq!(
            resp.headers()
                .get("access-control-allow-headers")
                .unwrap()
                .to_str()
                .unwrap(),
            "X-Tenant-Id, Authorization"
        );
    }
}
