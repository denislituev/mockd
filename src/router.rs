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
//!    satisfied: required query parameters, headers and a JSON body subset.
//!    Values support exact, `matches` (regex) and `contains` (substring)
//!    matchers; regexes are compiled once in [`Router::new`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde_json::Value;

use crate::config::{FieldMatcher, Method, RequestMatch, ResponseConfig, Route};

/// A compiled set of routes ready to answer requests.
#[derive(Debug, Clone)]
pub struct Router {
    routes: Vec<CompiledRoute>,
}

#[derive(Debug, Clone)]
struct CompiledRoute {
    method: Method,
    segments: Vec<Segment>,
    when: Option<CompiledWhen>,
    /// The list of responses for this route. A single-element vec means a
    /// plain static route; a longer vec is a sequence whose counter advances
    /// on each match (last item is sticky).
    responses: Vec<ResponseConfig>,
    /// Per-route counter for sequence responses. Shared across `Router`
    /// clones so that all callers observe the same progression.
    counter: Arc<AtomicUsize>,
}

/// A `when` block with regexes precompiled and header keys lowercased.
#[derive(Debug, Clone)]
struct CompiledWhen {
    query: Vec<(String, CompiledMatcher)>,
    headers: Vec<(String, CompiledMatcher)>,
    body: Option<CompiledBody>,
}

/// A single query/header value matcher with its regex precompiled.
#[derive(Debug, Clone)]
enum CompiledMatcher {
    Exact(String),
    Matches(regex::Regex),
    Contains(String),
}

/// A body pattern node: literal JSON or an embedded matcher operator.
#[derive(Debug, Clone)]
enum CompiledBody {
    Matches(regex::Regex),
    Contains(String),
    Value(Value),
    Object(Vec<(String, CompiledBody)>),
    Array(Vec<CompiledBody>),
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
    /// Returns an error if any route has an invalid path pattern, an empty
    /// `sequence: []` response spec, or an invalid regex in a `when`
    /// matcher (regexes are compiled once here, not per request).
    pub fn new(routes: Vec<Route>) -> Result<Self, RouterError> {
        let mut compiled = Vec::with_capacity(routes.len());
        for (index, route) in routes.into_iter().enumerate() {
            let segments = compile_path(&route.path).map_err(|e| RouterError::InvalidPath {
                route_index: index,
                source: e,
            })?;
            let when =
                route
                    .when
                    .as_ref()
                    .map(compile_when)
                    .transpose()
                    .map_err(|(field, source)| RouterError::InvalidRegex {
                        route_index: index,
                        field,
                        source,
                    })?;
            let responses = route.response.into_responses();
            if responses.is_empty() {
                return Err(RouterError::EmptySequence { route_index: index });
            }
            compiled.push(CompiledRoute {
                method: route.method,
                segments,
                when,
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
    /// performed case-insensitively for exact values regardless.
    ///
    /// For sequence routes, each successful match advances the internal
    /// counter; the last response in the sequence is repeated forever.
    ///
    /// When no route matches, `RUST_LOG=mockd=debug` explains why each
    /// candidate route was skipped.
    pub fn resolve(
        &self,
        method: Method,
        path: &str,
        query: &HashMap<String, String>,
        headers: &HashMap<String, String>,
        body: &Value,
    ) -> Option<Match> {
        let request_segments: Vec<&str> = path_segments(path).collect();

        for (index, route) in self.routes.iter().enumerate() {
            if route.method != method {
                tracing::debug!(route = index, reason = "method mismatch", "route skipped");
                continue;
            }
            let Some(path_params) = match_path(&route.segments, &request_segments) else {
                tracing::debug!(route = index, reason = "path mismatch", "route skipped");
                continue;
            };
            if let Err(reason) = match_when(route.when.as_ref(), query, headers, body) {
                tracing::debug!(route = index, reason = %reason, "route skipped");
                continue;
            }
            let response = pick_response(route);
            return Some(Match {
                path_params,
                response,
            });
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

/// Compile a `when` block: precompile regexes and lowercase header keys.
///
/// The error carries the config path of the offending field (e.g.
/// `when.query.email`) for reporting.
fn compile_when(when: &RequestMatch) -> Result<CompiledWhen, (String, regex::Error)> {
    let mut query = Vec::with_capacity(when.query.len());
    for (key, matcher) in &when.query {
        let compiled = compile_matcher(matcher).map_err(|e| (format!("when.query.{key}"), e))?;
        query.push((key.clone(), compiled));
    }

    let mut headers = Vec::with_capacity(when.headers.len());
    for (key, matcher) in &when.headers {
        let compiled = compile_matcher(matcher).map_err(|e| (format!("when.headers.{key}"), e))?;
        headers.push((key.to_ascii_lowercase(), compiled));
    }

    let body = when
        .body
        .as_ref()
        .map(|pattern| compile_body(pattern, "when.body"))
        .transpose()?;

    Ok(CompiledWhen {
        query,
        headers,
        body,
    })
}

fn compile_matcher(matcher: &FieldMatcher) -> Result<CompiledMatcher, regex::Error> {
    match matcher {
        FieldMatcher::Exact(value) => Ok(CompiledMatcher::Exact(value.clone())),
        FieldMatcher::Matches(m) => Ok(CompiledMatcher::Matches(regex::Regex::new(&m.matches)?)),
        FieldMatcher::Contains(c) => Ok(CompiledMatcher::Contains(c.contains.clone())),
    }
}

/// Compile a body pattern.
///
/// A JSON object with exactly one key `matches`/`contains` and a string
/// value is an operator; any other object (or array) is a literal subset
/// pattern compiled recursively.
fn compile_body(pattern: &Value, path: &str) -> Result<CompiledBody, (String, regex::Error)> {
    match pattern {
        Value::Object(map) => {
            if map.len() == 1 {
                if let Some(pattern) = map.get("matches").and_then(Value::as_str) {
                    return regex::Regex::new(pattern)
                        .map(CompiledBody::Matches)
                        .map_err(|e| (format!("{path}.matches"), e));
                }
                if let Some(needle) = map.get("contains").and_then(Value::as_str) {
                    return Ok(CompiledBody::Contains(needle.to_string()));
                }
            }
            let entries = map
                .iter()
                .map(|(key, value)| {
                    compile_body(value, &format!("{path}.{key}"))
                        .map(|compiled| (key.clone(), compiled))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CompiledBody::Object(entries))
        }
        Value::Array(items) => {
            let compiled = items
                .iter()
                .enumerate()
                .map(|(i, item)| compile_body(item, &format!("{path}[{i}]")))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CompiledBody::Array(compiled))
        }
        literal => Ok(CompiledBody::Value(literal.clone())),
    }
}

/// Evaluate the optional `when` block.
///
/// Returns `Ok(())` when every condition holds, or `Err(reason)` with a
/// human-readable explanation of the first failing condition (surfaced in
/// debug logging when a route is skipped).
fn match_when(
    when: Option<&CompiledWhen>,
    query: &HashMap<String, String>,
    headers: &HashMap<String, String>,
    body: &Value,
) -> Result<(), String> {
    let Some(when) = when else {
        return Ok(());
    };

    for (key, matcher) in &when.query {
        match query.get(key) {
            Some(actual) => {
                if !matcher_matches(matcher, actual, false) {
                    return Err(format!("query \"{key}\": {}", expectation_desc(matcher)));
                }
            }
            None => return Err(format!("query \"{key}\" is missing")),
        }
    }

    for (key, matcher) in &when.headers {
        match headers.get(key) {
            Some(actual) => {
                if !matcher_matches(matcher, actual, true) {
                    return Err(if is_sensitive_header(key) {
                        format!("header \"{key}\": value did not match the expected matcher")
                    } else {
                        header_mismatch_desc(key, matcher, actual)
                    });
                }
            }
            None => return Err(format!("header \"{key}\" is missing")),
        }
    }

    if let Some(pattern) = &when.body {
        body_matches(pattern, body, "body")?;
    }

    Ok(())
}

/// Apply a compiled matcher to an actual value.
///
/// Exact header values are compared case-insensitively (historical
/// behavior); `matches` and `contains` are case-sensitive everywhere.
fn matcher_matches(matcher: &CompiledMatcher, actual: &str, ignore_case: bool) -> bool {
    match matcher {
        CompiledMatcher::Exact(expected) => {
            if ignore_case {
                actual.eq_ignore_ascii_case(expected)
            } else {
                actual == expected
            }
        }
        CompiledMatcher::Matches(re) => re.is_match(actual),
        CompiledMatcher::Contains(needle) => actual.contains(needle.as_str()),
    }
}

/// Headers whose values must never appear in logs.
const SENSITIVE_HEADERS: [&str; 4] = [
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
];

fn is_sensitive_header(name: &str) -> bool {
    SENSITIVE_HEADERS.contains(&name.to_ascii_lowercase().as_str())
}

/// Describe what a matcher expects (never includes request values).
fn expectation_desc(matcher: &CompiledMatcher) -> String {
    match matcher {
        CompiledMatcher::Exact(expected) => format!("expected \"{expected}\""),
        CompiledMatcher::Matches(re) => format!("value does not match /{}/", re.as_str()),
        CompiledMatcher::Contains(needle) => format!("value does not contain \"{needle}\""),
    }
}

/// Describe why a header matcher did not accept the actual value. Regular
/// headers log both sides; sensitive headers are masked by the caller.
fn header_mismatch_desc(key: &str, matcher: &CompiledMatcher, actual: &str) -> String {
    match matcher {
        CompiledMatcher::Exact(expected) => {
            format!("header \"{key}\": expected \"{expected}\", got \"{actual}\"")
        }
        CompiledMatcher::Matches(re) => format!(
            "header \"{key}\": \"{actual}\" does not match /{}/",
            re.as_str()
        ),
        CompiledMatcher::Contains(needle) => {
            format!("header \"{key}\": \"{actual}\" does not contain \"{needle}\"")
        }
    }
}

/// Subset match between a compiled body pattern and the actual request body.
///
/// - Objects: every key in the pattern must be present and recursively match.
/// - Arrays: must have the same length and match element by element.
/// - Scalars: equality.
/// - `matches`/`contains` operators only apply to string values; a
///   non-string value never matches an operator.
///
/// On failure, `Err` carries the failing path (e.g. `body.user.email`);
/// actual request values are never included, type mismatches report the
/// JSON type instead.
fn body_matches(pattern: &CompiledBody, actual: &Value, path: &str) -> Result<(), String> {
    match pattern {
        CompiledBody::Matches(re) => match actual.as_str() {
            Some(value) if re.is_match(value) => Ok(()),
            Some(_) => Err(format!("{path}: value does not match /{}/", re.as_str())),
            None => Err(format!(
                "{path}: expected a string matching /{}/",
                re.as_str()
            )),
        },
        CompiledBody::Contains(needle) => match actual.as_str() {
            Some(value) if value.contains(needle.as_str()) => Ok(()),
            Some(_) => Err(format!("{path}: value does not contain \"{needle}\"")),
            None => Err(format!("{path}: expected a string containing \"{needle}\"")),
        },
        CompiledBody::Value(expected) => {
            if expected == actual {
                Ok(())
            } else {
                Err(format!(
                    "{path}: expected {expected}, got {}",
                    json_type_name(actual)
                ))
            }
        }
        CompiledBody::Object(entries) => {
            let Value::Object(map) = actual else {
                return Err(format!(
                    "{path}: expected an object, got {}",
                    json_type_name(actual)
                ));
            };
            for (key, sub) in entries {
                match map.get(key) {
                    Some(value) => body_matches(sub, value, &format!("{path}.{key}"))?,
                    None => return Err(format!("{path}: missing field \"{key}\"")),
                }
            }
            Ok(())
        }
        CompiledBody::Array(items) => {
            let Value::Array(values) = actual else {
                return Err(format!(
                    "{path}: expected an array, got {}",
                    json_type_name(actual)
                ));
            };
            if items.len() != values.len() {
                return Err(format!(
                    "{path}: expected {} item(s), got {}",
                    items.len(),
                    values.len()
                ));
            }
            for (i, (sub, value)) in items.iter().zip(values).enumerate() {
                body_matches(sub, value, &format!("{path}[{i}]"))?;
            }
            Ok(())
        }
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
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

    /// A `matches` regex in a `when` block could not be compiled.
    #[error("invalid regex in route {route_index} at {field}: {source}")]
    InvalidRegex {
        route_index: usize,
        field: String,
        #[source]
        source: regex::Error,
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
    use crate::config::{
        ContainsMatcher, FieldMatcher, MatchesMatcher, Method, RequestMatch, ResponseConfig,
        ResponseSpec,
    };
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
            query: [("role".to_string(), FieldMatcher::Exact("admin".to_string()))].into(),
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
            headers: [(
                "X-Tenant-Id".to_string(),
                FieldMatcher::Exact("tenant-a".to_string()),
            )]
            .into(),
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
            query: [("role".to_string(), FieldMatcher::Exact("admin".to_string()))].into(),
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

    // ------------------------------------------------------------------
    // Matcher operators (matches / contains)
    // ------------------------------------------------------------------

    fn query_matcher_route(pattern: &str) -> Router {
        let mut r = route(Method::Get, "/x");
        r.when = Some(RequestMatch {
            query: [(
                "v".to_string(),
                FieldMatcher::Matches(MatchesMatcher {
                    matches: pattern.to_string(),
                }),
            )]
            .into(),
            ..Default::default()
        });
        Router::new(vec![r]).unwrap()
    }

    #[test]
    fn query_regex_matcher_anchors_are_explicit() {
        let router = query_matcher_route("^b+");
        let (mut q, h, b) = empty_inputs();
        q.insert("v".into(), "bbb".into());
        assert!(router.resolve(Method::Get, "/x", &q, &h, &b).is_some());
        q.insert("v".into(), "abb".into());
        assert!(router.resolve(Method::Get, "/x", &q, &h, &b).is_none());
    }

    #[test]
    fn query_regex_matcher_is_case_sensitive() {
        let router = query_matcher_route("^A$");
        let (mut q, h, b) = empty_inputs();
        q.insert("v".into(), "a".into());
        assert!(router.resolve(Method::Get, "/x", &q, &h, &b).is_none());
    }

    #[test]
    fn query_contains_matcher() {
        let mut r = route(Method::Get, "/x");
        r.when = Some(RequestMatch {
            query: [(
                "env".to_string(),
                FieldMatcher::Contains(ContainsMatcher {
                    contains: "stag".to_string(),
                }),
            )]
            .into(),
            ..Default::default()
        });
        let router = Router::new(vec![r]).unwrap();
        let (mut q, h, b) = empty_inputs();
        q.insert("env".into(), "staging-eu".into());
        assert!(router.resolve(Method::Get, "/x", &q, &h, &b).is_some());
        q.insert("env".into(), "production".into());
        assert!(router.resolve(Method::Get, "/x", &q, &h, &b).is_none());
    }

    #[test]
    fn header_regex_is_case_sensitive_but_exact_is_not() {
        let mut r = route(Method::Get, "/x");
        r.when = Some(RequestMatch {
            headers: [(
                "Authorization".to_string(),
                FieldMatcher::Matches(MatchesMatcher {
                    matches: "^Bearer ".to_string(),
                }),
            )]
            .into(),
            ..Default::default()
        });
        let router = Router::new(vec![r]).unwrap();
        let (q, mut h, b) = empty_inputs();
        h.insert("authorization".into(), "Bearer abc".into());
        assert!(router.resolve(Method::Get, "/x", &q, &h, &b).is_some());
        h.insert("authorization".into(), "bearer abc".into());
        assert!(router.resolve(Method::Get, "/x", &q, &h, &b).is_none());
    }

    #[test]
    fn body_matches_operator() {
        let mut r = route(Method::Post, "/x");
        r.when = Some(RequestMatch {
            body: Some(json!({"email": {"matches": ".*@example\\.com$"}})),
            ..Default::default()
        });
        let router = Router::new(vec![r]).unwrap();
        let (q, h, _) = empty_inputs();
        let ok = json!({"email": "bob@example.com", "extra": 1});
        assert!(router.resolve(Method::Post, "/x", &q, &h, &ok).is_some());
        let bad = json!({"email": "bob@evil.com"});
        assert!(router.resolve(Method::Post, "/x", &q, &h, &bad).is_none());
    }

    #[test]
    fn body_contains_operator() {
        let mut r = route(Method::Post, "/x");
        r.when = Some(RequestMatch {
            body: Some(json!({"message": {"contains": "boom"}})),
            ..Default::default()
        });
        let router = Router::new(vec![r]).unwrap();
        let (q, h, _) = empty_inputs();
        let ok = json!({"message": "it went boom here"});
        assert!(router.resolve(Method::Post, "/x", &q, &h, &ok).is_some());
        let bad = json!({"message": "all good"});
        assert!(router.resolve(Method::Post, "/x", &q, &h, &bad).is_none());
    }

    #[test]
    fn body_operator_requires_string_actual() {
        let mut r = route(Method::Post, "/x");
        r.when = Some(RequestMatch {
            body: Some(json!({"count": {"matches": "[0-9]+"}})),
            ..Default::default()
        });
        let router = Router::new(vec![r]).unwrap();
        let (q, h, _) = empty_inputs();
        let number = json!({"count": 42});
        assert!(router
            .resolve(Method::Post, "/x", &q, &h, &number)
            .is_none());
    }

    #[test]
    fn body_operator_needs_exactly_one_key() {
        let mut r = route(Method::Post, "/x");
        r.when = Some(RequestMatch {
            body: Some(json!({"filter": {"matches": "a", "other": 1}})),
            ..Default::default()
        });
        let router = Router::new(vec![r]).unwrap();
        let (q, h, _) = empty_inputs();
        let ok = json!({"filter": {"matches": "a", "other": 1}, "z": 0});
        assert!(router.resolve(Method::Post, "/x", &q, &h, &ok).is_some());
        let bad = json!({"filter": "a"});
        assert!(router.resolve(Method::Post, "/x", &q, &h, &bad).is_none());
    }

    #[test]
    fn body_operator_requires_string_pattern() {
        let mut r = route(Method::Post, "/x");
        r.when = Some(RequestMatch {
            body: Some(json!({"filter": {"matches": 42}})),
            ..Default::default()
        });
        let router = Router::new(vec![r]).unwrap();
        let (q, h, _) = empty_inputs();
        let ok = json!({"filter": {"matches": 42}});
        assert!(router.resolve(Method::Post, "/x", &q, &h, &ok).is_some());
    }

    #[test]
    fn rejects_invalid_regex_with_field_path() {
        let mut r = route(Method::Get, "/x");
        r.when = Some(RequestMatch {
            query: [(
                "q".to_string(),
                FieldMatcher::Matches(MatchesMatcher {
                    matches: "(unclosed".to_string(),
                }),
            )]
            .into(),
            ..Default::default()
        });
        let err = Router::new(vec![r]).unwrap_err();
        match err {
            RouterError::InvalidRegex {
                route_index: 0,
                field,
                ..
            } => assert_eq!(field, "when.query.q"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn rejects_invalid_body_regex_with_path() {
        let mut r = route(Method::Post, "/x");
        r.when = Some(RequestMatch {
            body: Some(json!({"user": {"email": {"matches": "["}}})),
            ..Default::default()
        });
        let err = Router::new(vec![r]).unwrap_err();
        match err {
            RouterError::InvalidRegex {
                route_index: 0,
                field,
                ..
            } => assert_eq!(field, "when.body.user.email.matches"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn match_when_explains_the_first_failing_condition() {
        let mut when = RequestMatch::default();
        when.query
            .insert("role".to_string(), FieldMatcher::Exact("admin".to_string()));
        let compiled = compile_when(&when).unwrap();
        let (q, h, b) = empty_inputs();
        let reason = match_when(Some(&compiled), &q, &h, &b).unwrap_err();
        assert_eq!(reason, "query \"role\" is missing");
    }

    #[test]
    fn sensitive_header_values_are_never_explained() {
        let mut when = RequestMatch::default();
        when.headers.insert(
            "Authorization".to_string(),
            FieldMatcher::Matches(MatchesMatcher {
                matches: "^Bearer ".to_string(),
            }),
        );
        let compiled = compile_when(&when).unwrap();
        let (q, mut h, b) = empty_inputs();
        h.insert("authorization".into(), "Basic super-secret".into());
        let reason = match_when(Some(&compiled), &q, &h, &b).unwrap_err();
        assert_eq!(
            reason,
            "header \"authorization\": value did not match the expected matcher"
        );
        assert!(!reason.contains("super-secret"));
    }

    #[test]
    fn regular_header_mismatch_still_shows_values() {
        let mut when = RequestMatch::default();
        when.headers.insert(
            "X-Environment".to_string(),
            FieldMatcher::Exact("staging".to_string()),
        );
        let compiled = compile_when(&when).unwrap();
        let (q, mut h, b) = empty_inputs();
        h.insert("x-environment".into(), "production".into());
        let reason = match_when(Some(&compiled), &q, &h, &b).unwrap_err();
        assert_eq!(
            reason,
            "header \"x-environment\": expected \"staging\", got \"production\""
        );
    }

    #[test]
    fn query_mismatch_never_includes_actual_value() {
        let mut when = RequestMatch::default();
        when.query.insert(
            "token".to_string(),
            FieldMatcher::Matches(MatchesMatcher {
                matches: "^abc".to_string(),
            }),
        );
        let compiled = compile_when(&when).unwrap();
        let (mut q, h, b) = empty_inputs();
        q.insert("token".into(), "super-secret-token".into());
        let reason = match_when(Some(&compiled), &q, &h, &b).unwrap_err();
        assert_eq!(reason, "query \"token\": value does not match /^abc/");
        assert!(!reason.contains("super-secret-token"));
    }

    #[test]
    fn body_literal_mismatch_never_includes_actual_value() {
        let when = RequestMatch {
            body: Some(json!({"password": "foo"})),
            ..Default::default()
        };
        let compiled = compile_when(&when).unwrap();
        let (q, h, _) = empty_inputs();
        let body = json!({"username": "alice", "password": "my-secret-password"});
        let reason = match_when(Some(&compiled), &q, &h, &body).unwrap_err();
        assert_eq!(reason, "body.password: expected \"foo\", got a string");
        assert!(!reason.contains("my-secret-password"));
    }

    #[test]
    fn body_operator_mismatch_never_includes_actual_value() {
        let when = RequestMatch {
            body: Some(json!({"email": {"matches": ".*@example\\.com$"}})),
            ..Default::default()
        };
        let compiled = compile_when(&when).unwrap();
        let (q, h, _) = empty_inputs();
        let body = json!({"email": "alice@evil.com"});
        let reason = match_when(Some(&compiled), &q, &h, &body).unwrap_err();
        assert_eq!(
            reason,
            "body.email: value does not match /.*@example\\.com$/"
        );
        assert!(!reason.contains("alice@evil.com"));
    }

    #[test]
    fn body_type_mismatch_reports_type_not_value() {
        let when = RequestMatch {
            body: Some(json!({"user": {"name": "x"}})),
            ..Default::default()
        };
        let compiled = compile_when(&when).unwrap();
        let (q, h, _) = empty_inputs();
        let body = json!({"user": "secret-name"});
        let reason = match_when(Some(&compiled), &q, &h, &body).unwrap_err();
        assert_eq!(reason, "body.user: expected an object, got a string");
        assert!(!reason.contains("secret-name"));
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
