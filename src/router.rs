//! Route matching.
//!
//! The [`Router`] owns the compiled set of mock routes and answers the
//! question *"given this request, which route matches and what should the
//! response be?"*.
//!
//! ## Matching algorithm
//!
//! Routes are evaluated in declaration order; the first matching route wins.
//! A route matches when:
//!
//! 1. the HTTP [`Method`] equals the request method,
//! 2. the path pattern matches the request path segment by segment
//!    (capturing `{param}` segments), and
//! 3. every rule in the optional `when` block ([`RequestMatch`]) is
//!    satisfied: required query parameters, headers (case-insensitively)
//!    and a JSON body subset.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value;

use crate::config::{Method, RequestMatch, ResponseConfig, Route};

/// A compiled set of routes ready to answer requests.
#[derive(Debug, Clone)]
pub struct Router {
    routes: Vec<CompiledRoute>,
}

#[derive(Debug, Clone)]
struct CompiledRoute {
    method: Method,
    segments: Vec<Segment>,
    when: Option<RequestMatch>,
    /// The list of responses for this route. A single-element vec means a
    /// plain static route; a longer vec is a sequence whose counter advances
    /// on each match (last item is sticky).
    responses: Vec<ResponseConfig>,
    /// Per-route counter for sequence responses. Shared across `Router`
    /// clones so that all callers observe the same progression.
    counter: Arc<AtomicUsize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    Param(String),
}

/// The result of matching a request against the [`Router`].
#[derive(Debug, Clone)]
pub struct Match {
    /// Captured path parameters (e.g. `{"id": "42"}`).
    pub path_params: HashMap<String, String>,
    /// The response that should be produced.
    pub response: ResponseConfig,
}

impl Router {
    /// Compile a set of routes.
    ///
    /// Returns an error if any route has an invalid path pattern or an empty
    /// `sequence: []` response spec.
    pub fn new(routes: Vec<Route>) -> Result<Self, RouterError> {
        let mut compiled = Vec::with_capacity(routes.len());
        for (index, route) in routes.into_iter().enumerate() {
            let segments = compile_path(&route.path).map_err(|e| RouterError::InvalidPath {
                route_index: index,
                source: e,
            })?;
            let responses = route.response.into_responses();
            if responses.is_empty() {
                return Err(RouterError::EmptySequence { route_index: index });
            }
            compiled.push(CompiledRoute {
                method: route.method,
                segments,
                when: route.when,
                responses,
                counter: Arc::new(AtomicUsize::new(0)),
            });
        }
        Ok(Router { routes: compiled })
    }

    /// Number of compiled routes.
    pub fn len(&self) -> usize {
        self.routes.len()
    }

    /// Whether the router has no routes.
    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Resolve a request to a [`Match`].
    ///
    /// All inputs use plain, server-agnostic types. `headers` should use
    /// lower-cased header names; header *matching* against route rules is
    /// performed case-insensitively regardless.
    ///
    /// For sequence routes, each successful match advances the internal
    /// counter; the last response in the sequence is repeated forever.
    pub fn resolve(
        &self,
        method: Method,
        path: &str,
        query: &HashMap<String, String>,
        headers: &HashMap<String, String>,
        body: &Value,
    ) -> Option<Match> {
        let request_segments: Vec<&str> = path_segments(path).collect();

        for route in &self.routes {
            if route.method != method {
                continue;
            }
            if let Some(path_params) = match_path(&route.segments, &request_segments) {
                if match_when(route.when.as_ref(), query, headers, body) {
                    let response = pick_response(route);
                    return Some(Match {
                        path_params,
                        response,
                    });
                }
            }
        }
        None
    }
}

/// Select the response for a matched route.
///
/// For a single-response route this is the only item. For a sequence route,
/// each call returns the next item until the last is reached, after which the
/// last item is returned on every subsequent call (sticky last).
fn pick_response(route: &CompiledRoute) -> ResponseConfig {
    let n = route.responses.len();
    if n == 1 {
        return route.responses[0].clone();
    }
    let idx = route.counter.fetch_add(1, Ordering::Relaxed);
    // Once we've passed the end, keep returning the last response.
    let clamped = idx.min(n - 1);
    route.responses[clamped].clone()
}

/// Split a request path into non-empty segments, ignoring the leading slash.
fn path_segments(path: &str) -> impl Iterator<Item = &str> {
    path.trim_end_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
}

/// Compile a path pattern into [`Segment`]s.
///
/// Patterns look like `/users/{id}/items/{itemId}`. A `{name}` placeholder
/// captures a single path segment. The pattern must be well-formed: balanced
/// braces and a non-empty name.
fn compile_path(pattern: &str) -> Result<Vec<Segment>, PathError> {
    let mut segments = Vec::new();
    for raw in pattern.split('/') {
        if raw.is_empty() {
            continue;
        }
        if let Some(name) = raw.strip_prefix('{').and_then(|r| r.strip_suffix('}')) {
            if name.is_empty() || name.contains('{') || name.contains('}') {
                return Err(PathError::InvalidParam(raw.to_string()));
            }
            segments.push(Segment::Param(name.to_string()));
        } else if raw.contains('{') || raw.contains('}') {
            return Err(PathError::UnbalancedBraces(raw.to_string()));
        } else {
            segments.push(Segment::Literal(raw.to_string()));
        }
    }
    if segments.is_empty() {
        return Err(PathError::EmptyPattern);
    }
    Ok(segments)
}

/// Match compiled segments against request segments, capturing params.
///
/// Returns the captured params if the segments match, or `None` otherwise.
fn match_path(segments: &[Segment], request: &[&str]) -> Option<HashMap<String, String>> {
    if segments.len() != request.len() {
        return None;
    }
    let mut params = HashMap::with_capacity(segments.len());
    for (seg, req) in segments.iter().zip(request.iter()) {
        match seg {
            Segment::Literal(lit) => {
                if lit != req {
                    return None;
                }
            }
            Segment::Param(name) => {
                params.insert(name.clone(), (*req).to_string());
            }
        }
    }
    Some(params)
}

/// Evaluate the optional `when` block.
fn match_when(
    when: Option<&RequestMatch>,
    query: &HashMap<String, String>,
    headers: &HashMap<String, String>,
    body: &Value,
) -> bool {
    let Some(when) = when else {
        return true;
    };
    when.query
        .iter()
        .all(|(k, v)| query.get(k).map(|actual| actual == v).unwrap_or(false))
        && when.headers.iter().all(|(k, v)| {
            let lower = k.to_ascii_lowercase();
            headers
                .get(&lower)
                .map(|actual| actual.eq_ignore_ascii_case(v))
                .unwrap_or(false)
        })
        && when
            .body
            .as_ref()
            .map(|pattern| body_matches(pattern, body))
            .unwrap_or(true)
}

/// Subset match between a JSON pattern and the actual request body.
///
/// - Objects: every key in the pattern must be present and recursively match.
/// - Arrays: must have the same length and match element by element.
/// - Scalars: equality.
fn body_matches(pattern: &Value, actual: &Value) -> bool {
    match (pattern, actual) {
        (Value::Object(pattern), Value::Object(actual)) => pattern.iter().all(|(key, value)| {
            actual
                .get(key)
                .map(|a| body_matches(value, a))
                .unwrap_or(false)
        }),
        (Value::Array(pattern), Value::Array(actual)) => {
            pattern.len() == actual.len()
                && pattern.iter().zip(actual).all(|(p, a)| body_matches(p, a))
        }
        _ => pattern == actual,
    }
}

/// Errors that can occur while building a [`Router`].
#[derive(Debug, thiserror::Error)]
pub enum RouterError {
    /// A route's path pattern could not be compiled.
    #[error("invalid path pattern in route {route_index}: {source}")]
    InvalidPath {
        route_index: usize,
        #[source]
        source: PathError,
    },

    /// A `response.sequence` was empty.
    #[error("empty `sequence` in route {route_index}; expected at least one item")]
    EmptySequence { route_index: usize },
}

/// Errors in an individual path pattern.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PathError {
    /// A `{...}` placeholder was malformed.
    #[error("invalid path parameter `{0}`")]
    InvalidParam(String),
    /// Curly braces appear outside a placeholder.
    #[error("unbalanced braces in segment `{0}`")]
    UnbalancedBraces(String),
    /// The pattern contained no segments.
    #[error("path pattern is empty")]
    EmptyPattern,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Method, RequestMatch, ResponseConfig, ResponseSpec};
    use serde_json::json;

    fn route(method: Method, path: &str) -> Route {
        Route {
            method,
            path: path.to_string(),
            when: None,
            response: ResponseSpec::Single(ResponseConfig::default()),
        }
    }

    /// Helper: build a route whose single response has the given status.
    fn route_with_status(method: Method, path: &str, status: u16) -> Route {
        let mut r = route(method, path);
        if let ResponseSpec::Single(resp) = &mut r.response {
            resp.status = status;
        }
        r
    }

    fn empty_inputs() -> (HashMap<String, String>, HashMap<String, String>, Value) {
        (HashMap::new(), HashMap::new(), Value::Null)
    }

    #[test]
    fn literal_path_matches() {
        let router = Router::new(vec![route(Method::Get, "/users")]).unwrap();
        let (q, h, b) = empty_inputs();
        let m = router.resolve(Method::Get, "/users", &q, &h, &b);
        assert!(m.is_some());
    }

    #[test]
    fn leading_slash_optional() {
        let router = Router::new(vec![route(Method::Get, "/users")]).unwrap();
        let (q, h, b) = empty_inputs();
        assert!(router.resolve(Method::Get, "users", &q, &h, &b).is_some());
    }

    #[test]
    fn method_must_match() {
        let router = Router::new(vec![route(Method::Get, "/users")]).unwrap();
        let (q, h, b) = empty_inputs();
        assert!(router.resolve(Method::Post, "/users", &q, &h, &b).is_none());
    }

    #[test]
    fn captures_path_params() {
        let router = Router::new(vec![route(Method::Get, "/users/{id}/items/{itemId}")]).unwrap();
        let (q, h, b) = empty_inputs();
        let m = router
            .resolve(Method::Get, "/users/42/items/7", &q, &h, &b)
            .unwrap();
        assert_eq!(m.path_params.get("id").unwrap(), "42");
        assert_eq!(m.path_params.get("itemId").unwrap(), "7");
    }

    #[test]
    fn segment_count_must_match() {
        let router = Router::new(vec![route(Method::Get, "/users/{id}")]).unwrap();
        let (q, h, b) = empty_inputs();
        assert!(router
            .resolve(Method::Get, "/users/42/items", &q, &h, &b)
            .is_none());
    }

    #[test]
    fn first_match_wins() {
        let r1 = route_with_status(Method::Get, "/users/{id}", 200);
        let r2 = route_with_status(Method::Get, "/users/{id}", 201);
        let router = Router::new(vec![r1, r2]).unwrap();
        let (q, h, b) = empty_inputs();
        let m = router.resolve(Method::Get, "/users/1", &q, &h, &b).unwrap();
        assert_eq!(m.response.status, 200);
    }

    #[test]
    fn matches_query_param() {
        let mut r = route(Method::Get, "/users");
        r.when = Some(RequestMatch {
            query: [("role".to_string(), "admin".to_string())].into(),
            ..Default::default()
        });
        let router = Router::new(vec![r]).unwrap();
        let (mut q, h, b) = empty_inputs();
        assert!(router.resolve(Method::Get, "/users", &q, &h, &b).is_none());
        q.insert("role".into(), "admin".into());
        assert!(router.resolve(Method::Get, "/users", &q, &h, &b).is_some());
    }

    #[test]
    fn matches_header_case_insensitively() {
        let mut r = route(Method::Get, "/users");
        r.when = Some(RequestMatch {
            headers: [("X-Tenant-Id".to_string(), "tenant-a".to_string())].into(),
            ..Default::default()
        });
        let router = Router::new(vec![r]).unwrap();
        let (q, mut h, b) = empty_inputs();
        h.insert("x-tenant-id".into(), "TENANT-A".into());
        assert!(router.resolve(Method::Get, "/users", &q, &h, &b).is_some());
    }

    #[test]
    fn matches_body_subset() {
        let mut r = route(Method::Post, "/login");
        r.when = Some(RequestMatch {
            body: Some(json!({"username": "admin"})),
            ..Default::default()
        });
        let router = Router::new(vec![r]).unwrap();
        let (q, h, _) = empty_inputs();
        let body = json!({"username": "admin", "password": "secret"});
        assert!(router
            .resolve(Method::Post, "/login", &q, &h, &body)
            .is_some());
        let other = json!({"username": "guest"});
        assert!(router
            .resolve(Method::Post, "/login", &q, &h, &other)
            .is_none());
    }

    #[test]
    fn when_block_can_disambiguate_same_path() {
        // Two routes with the same path: the one without `when` is a fallback,
        // the one with `when` is more specific. Declaring the specific one
        // first makes it win for matching requests.
        let mut admin = route_with_status(Method::Get, "/users", 201);
        admin.when = Some(RequestMatch {
            query: [("role".to_string(), "admin".to_string())].into(),
            ..Default::default()
        });
        let generic = route_with_status(Method::Get, "/users", 200);
        let router = Router::new(vec![admin, generic]).unwrap();

        let (mut q, h, b) = empty_inputs();
        q.insert("role".into(), "admin".into());
        let m = router.resolve(Method::Get, "/users", &q, &h, &b).unwrap();
        assert_eq!(m.response.status, 201);

        q.clear();
        let m = router.resolve(Method::Get, "/users", &q, &h, &b).unwrap();
        assert_eq!(m.response.status, 200);
    }

    #[test]
    fn rejects_invalid_path_pattern_empty_param() {
        let routes = vec![route(Method::Get, "/users/{}")];
        assert!(Router::new(routes).is_err());
    }

    #[test]
    fn rejects_invalid_path_pattern_unbalanced() {
        let routes = vec![route(Method::Get, "/users/{id")];
        assert!(Router::new(routes).is_err());
    }

    #[test]
    fn rejects_empty_pattern() {
        let routes = vec![route(Method::Get, "/")];
        assert!(Router::new(routes).is_err());
    }

    // -----------------------------------------------------------------
    // Sequence responses
    // -----------------------------------------------------------------

    fn sequence_route(method: Method, path: &str, statuses: Vec<u16>) -> Route {
        let sequence = statuses
            .into_iter()
            .map(|status| ResponseConfig {
                status,
                ..ResponseConfig::default()
            })
            .collect();
        Route {
            method,
            path: path.to_string(),
            when: None,
            response: ResponseSpec::Sequence { sequence },
        }
    }

    #[test]
    fn sequence_returns_responses_in_order() {
        let router = Router::new(vec![sequence_route(
            Method::Get,
            "/flaky",
            vec![500, 500, 200],
        )])
        .unwrap();
        let (q, h, b) = empty_inputs();

        assert_eq!(
            router
                .resolve(Method::Get, "/flaky", &q, &h, &b)
                .unwrap()
                .response
                .status,
            500
        );
        assert_eq!(
            router
                .resolve(Method::Get, "/flaky", &q, &h, &b)
                .unwrap()
                .response
                .status,
            500
        );
        assert_eq!(
            router
                .resolve(Method::Get, "/flaky", &q, &h, &b)
                .unwrap()
                .response
                .status,
            200
        );
    }

    #[test]
    fn sequence_sticks_on_last_response_after_exhausting() {
        let router =
            Router::new(vec![sequence_route(Method::Get, "/retry", vec![500, 200])]).unwrap();
        let (q, h, b) = empty_inputs();

        // Consume the whole sequence.
        router.resolve(Method::Get, "/retry", &q, &h, &b);
        router.resolve(Method::Get, "/retry", &q, &h, &b);

        // Subsequent calls keep returning the last one.
        for _ in 0..5 {
            assert_eq!(
                router
                    .resolve(Method::Get, "/retry", &q, &h, &b)
                    .unwrap()
                    .response
                    .status,
                200
            );
        }
    }

    #[test]
    fn sequence_state_is_shared_between_router_clones() {
        // The Router is cloned per Axum worker; all clones must observe the
        // same sequence progression.
        let router = Router::new(vec![sequence_route(Method::Get, "/x", vec![1, 2, 3])]).unwrap();
        let cloned = router.clone();
        let (q, h, b) = empty_inputs();

        // Interleave calls from both clones.
        assert_eq!(
            router
                .resolve(Method::Get, "/x", &q, &h, &b)
                .unwrap()
                .response
                .status,
            1
        );
        assert_eq!(
            cloned
                .resolve(Method::Get, "/x", &q, &h, &b)
                .unwrap()
                .response
                .status,
            2
        );
        assert_eq!(
            router
                .resolve(Method::Get, "/x", &q, &h, &b)
                .unwrap()
                .response
                .status,
            3
        );
        // Exhausted -> sticks on 3.
        assert_eq!(
            cloned
                .resolve(Method::Get, "/x", &q, &h, &b)
                .unwrap()
                .response
                .status,
            3
        );
    }

    #[test]
    fn empty_sequence_is_rejected_at_compile_time() {
        let r = Route {
            method: Method::Get,
            path: "/x".to_string(),
            when: None,
            response: ResponseSpec::Sequence { sequence: vec![] },
        };
        let err = Router::new(vec![r]).unwrap_err();
        assert!(matches!(err, RouterError::EmptySequence { route_index: 0 }));
    }
}
