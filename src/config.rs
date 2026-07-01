//! Domain models and YAML configuration loading.
//!
//! This module defines the shape of a mockd configuration file:
//!
//! ```yaml
//! listen: ":8080"
//! routes:
//!   - method: GET
//!     path: /users/{id}
//!     when:
//!       query:
//!         role: admin
//!     response:
//!       status: 200
//!       body:
//!         id: "{{path.id}}"
//! ```

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Method
// ---------------------------------------------------------------------------

/// Supported HTTP methods.
///
/// Serialized in upper-case form (`GET`, `POST`, ...) to match the way methods
/// are written in HTTP and in the configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    /// Parse an HTTP method string into a [`Method`].
    ///
    /// Returns `None` for methods that mockd does not yet support.
    pub fn from_http_str(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "GET" => Some(Method::Get),
            "POST" => Some(Method::Post),
            "PUT" => Some(Method::Put),
            "PATCH" => Some(Method::Patch),
            "DELETE" => Some(Method::Delete),
            _ => None,
        }
    }

    /// The canonical upper-case representation of the method.
    pub fn as_str(&self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Patch => "PATCH",
            Method::Delete => "DELETE",
        }
    }
}

impl FromStr for Method {
    type Err = UnknownMethodError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Method::from_http_str(s).ok_or_else(|| UnknownMethodError(s.to_string()))
    }
}

/// Error returned when a method string cannot be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unsupported HTTP method: {0}")]
pub struct UnknownMethodError(pub String);

// ---------------------------------------------------------------------------
// Request matching
// ---------------------------------------------------------------------------

/// Rules used to decide whether a [`Route`] matches an incoming request.
///
/// All fields are optional; an empty [`RequestMatch`] matches every request.
/// Header matching is performed case-insensitively by the router.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequestMatch {
    /// Query parameters that must be present with the given value.
    #[serde(default)]
    pub query: HashMap<String, String>,

    /// Request headers that must be present with the given value.
    ///
    /// Matched case-insensitively.
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// A JSON value that must be a subset of the request body.
    ///
    /// Subset matching means: every field of a JSON object in `body` must be
    /// present (and equal) in the request body. Arrays must match element by
    /// element and have the same length. Scalar values use equality.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

// ---------------------------------------------------------------------------
// Response
// ---------------------------------------------------------------------------

/// How a matched request should be answered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResponseConfig {
    /// HTTP status code. Defaults to `200`.
    #[serde(default = "default_status")]
    pub status: u16,

    /// Response headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Response body. Rendered as JSON.
    ///
    /// May contain template expressions such as `{{path.id}}` (see the
    /// [`template`](crate::template) module).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,

    /// Optional artificial delay before the response is sent.
    ///
    /// Parsed from human-friendly durations, e.g. `2s`, `250ms`, `1m 30s`.
    #[serde(
        default,
        with = "duration_option",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Option<String>")]
    pub delay: Option<Duration>,

    /// When `true`, the server signals that the connection should be closed
    /// after the response (by sending the `Connection: close` header).
    #[serde(default)]
    pub close_connection: bool,
}

impl Default for ResponseConfig {
    fn default() -> Self {
        ResponseConfig {
            status: default_status(),
            headers: HashMap::new(),
            body: None,
            delay: None,
            close_connection: false,
        }
    }
}

fn default_status() -> u16 {
    200
}

// ---------------------------------------------------------------------------
// Response spec: a single response or a sequence of responses
// ---------------------------------------------------------------------------

/// Either a single response or an ordered sequence of responses.
///
/// A route's `response` field accepts either shape via YAML:
///
/// ```yaml
/// # Single response (the existing form).
/// response:
///   status: 200
///   body: { ok: true }
///
/// # Sequence: each call advances to the next response.
/// # After the last one is reached, the last response is repeated forever.
/// response:
///   sequence:
///     - status: 500
///     - status: 500
///     - status: 200
///       body: { ok: true }
/// ```
///
/// Sequence responses are useful for testing retry, polling and pagination
/// logic in clients.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ResponseSpec {
    /// A response sequence. Each match advances to the next item; the last
    /// item is sticky (repeated on every subsequent call).
    Sequence {
        /// The ordered responses.
        sequence: Vec<ResponseConfig>,
    },
    /// A single static response.
    Single(ResponseConfig),
}

impl ResponseSpec {
    /// Flatten this spec into the underlying list of responses.
    ///
    /// `Single(r)` becomes `vec![r]`; `Sequence { sequence }` is returned as-is.
    pub fn into_responses(self) -> Vec<ResponseConfig> {
        match self {
            ResponseSpec::Single(r) => vec![r],
            ResponseSpec::Sequence { sequence } => sequence,
        }
    }
}

impl Default for ResponseSpec {
    fn default() -> Self {
        ResponseSpec::Single(ResponseConfig::default())
    }
}

// ---------------------------------------------------------------------------
// Route
// ---------------------------------------------------------------------------

/// A single mock route.
///
/// A route is selected when its HTTP `method` and `path` match the request,
/// and (optionally) the [`RequestMatch`] rules in `when` are satisfied.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Route {
    /// HTTP method for this route.
    pub method: Method,

    /// Path pattern, e.g. `/users/{id}`.
    ///
    /// Segments wrapped in curly braces are captured as path parameters.
    pub path: String,

    /// Optional additional request matching rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<RequestMatch>,

    /// Response (single or sequence) produced when the route matches.
    #[serde(default)]
    pub response: ResponseSpec,
}

// ---------------------------------------------------------------------------
// Top-level config file
// ---------------------------------------------------------------------------

/// Top-level configuration file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Config {
    /// Socket address to listen on, e.g. `":8080"` or `"127.0.0.1:9000"`.
    #[serde(default = "default_listen")]
    pub listen: String,

    /// Mock routes, evaluated in declaration order (first match wins).
    #[serde(default)]
    pub routes: Vec<Route>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            listen: default_listen(),
            routes: Vec::new(),
        }
    }
}

fn default_listen() -> String {
    ":8080".to_string()
}

impl Config {
    /// Parse a configuration from a YAML string.
    pub fn parse(input: &str) -> Result<Self, ConfigError> {
        serde_yaml::from_str(input).map_err(ConfigError::from)
    }

    /// Load and parse a configuration from a file.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path.as_ref()).map_err(ConfigError::Read)?;
        Self::parse(&contents)
    }
    /// Save JSON schema
    pub fn write_config_schema(path: &Path) -> anyhow::Result<()> {
        let schema = schemars::schema_for!(Config);
        let mut json = serde_json::to_string_pretty(&schema)?;
        json.push('\n');

        std::fs::create_dir_all(path)?;
        let schema_path = path.join("schema.json");
        std::fs::write(schema_path, json)?;

        Ok(())
    }
}

/// Errors that can occur while loading a configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("could not read config file: {0}")]
    Read(#[source] std::io::Error),

    /// The YAML could not be parsed into a [`Config`].
    #[error("could not parse config: {0}")]
    Parse(#[from] serde_yaml::Error),
}

// ---------------------------------------------------------------------------
// Duration serde helper (kept private to this module)
// ---------------------------------------------------------------------------

/// Serde adapter for `Option<Duration>` using human-friendly encoding
/// (`2s`, `250ms`, `1m 30s`, ...).
mod duration_option {
    use super::*;

    pub fn serialize<S>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match value {
            None => serializer.serialize_none(),
            Some(d) => serializer.serialize_str(&humantime::format_duration(*d).to_string()),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Duration>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt {
            None => Ok(None),
            Some(raw) => humantime::parse_duration(&raw)
                .map(Some)
                .map_err(serde::de::Error::custom),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_round_trip_uppercase() {
        let yaml = "GET";
        let method: Method = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(method, Method::Get);

        let back: String = serde_yaml::to_string(&method).unwrap();
        assert!(back.contains("GET"));
    }

    #[test]
    fn method_from_http_str_is_case_insensitive() {
        assert_eq!(Method::from_http_str("get"), Some(Method::Get));
        assert_eq!(Method::from_http_str("Delete"), Some(Method::Delete));
        assert_eq!(Method::from_http_str("FOO"), None);
    }

    #[test]
    fn empty_uses_defaults() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.listen, ":8080");
        assert!(cfg.routes.is_empty());
    }

    #[test]
    fn parses_full_example() {
        let yaml = r#"
listen: ":9090"
routes:
  - method: GET
    path: /users/{id}
    when:
      query:
        role: admin
    response:
      status: 200
      delay: 2s
      body:
        id: "{{path.id}}"
"#;
        let cfg = Config::parse(yaml).unwrap();
        assert_eq!(cfg.listen, ":9090");
        assert_eq!(cfg.routes.len(), 1);
        let route = &cfg.routes[0];
        assert_eq!(route.method, Method::Get);
        assert_eq!(route.path, "/users/{id}");
        assert_eq!(
            route.when.as_ref().unwrap().query.get("role").unwrap(),
            "admin"
        );
        let resp = match &route.response {
            ResponseSpec::Single(r) => r,
            ResponseSpec::Sequence { .. } => panic!("expected Single"),
        };
        assert_eq!(resp.status, 200);
        assert_eq!(resp.delay, Some(Duration::from_secs(2)));
    }

    #[test]
    fn invalid_yaml_is_rejected() {
        let yaml = "listen: :8080\n  routes: [broken\n";
        assert!(Config::parse(yaml).is_err());
    }

    #[test]
    fn unknown_method_is_rejected() {
        let yaml = "routes:\n  - method: FOO\n    path: /x\n";
        assert!(Config::parse(yaml).is_err());
    }

    #[test]
    fn round_trip_keeps_listen_and_routes() {
        let yaml = r#"
listen: ":8080"
routes:
  - method: POST
    path: /items
    response:
      status: 201
"#;
        let cfg = Config::parse(yaml).unwrap();
        let reserialized = serde_yaml::to_string(&cfg).unwrap();
        let cfg2 = Config::parse(&reserialized).unwrap();
        assert_eq!(cfg, cfg2);
    }

    #[test]
    fn missing_file_errors() {
        let err = Config::from_file("/nonexistent/path/to/config.yaml").unwrap_err();
        assert!(matches!(err, ConfigError::Read(_)));
    }

    #[test]
    fn parses_sequence_response() {
        let yaml = r#"
routes:
  - method: GET
    path: /flaky
    response:
      sequence:
        - status: 500
        - status: 200
          body:
            ok: true
"#;
        let cfg = Config::parse(yaml).unwrap();
        let route = &cfg.routes[0];
        match &route.response {
            ResponseSpec::Sequence { sequence } => {
                assert_eq!(sequence.len(), 2);
                assert_eq!(sequence[0].status, 500);
                assert_eq!(sequence[1].status, 200);
                assert_eq!(
                    sequence[1].body,
                    Some(Value::Object(serde_json::Map::from_iter([(
                        "ok".to_string(),
                        Value::Bool(true)
                    )])))
                );
            }
            other => panic!("expected Sequence, got {other:?}"),
        }
    }

    #[test]
    fn parses_single_response_by_default() {
        // Same shape as before the sequence feature; must still parse as Single.
        let yaml = r#"
routes:
  - method: GET
    path: /health
    response:
      status: 200
      body:
        ok: true
"#;
        let cfg = Config::parse(yaml).unwrap();
        assert!(matches!(cfg.routes[0].response, ResponseSpec::Single(_)));
    }

    #[test]
    fn sequence_round_trip() {
        let yaml = r#"
routes:
  - method: GET
    path: /retry
    response:
      sequence:
        - status: 500
        - status: 200
"#;
        let cfg = Config::parse(yaml).unwrap();
        let reserialized = serde_yaml::to_string(&cfg).unwrap();
        let cfg2 = Config::parse(&reserialized).unwrap();
        assert_eq!(cfg, cfg2);
    }

    /// Guard against the committed schema drifting from the Rust types.
    #[test]
    fn schema_does_not_drift() {
        let schema = schemars::schema_for!(Config);
        let mut actual = serde_json::to_string_pretty(&schema).unwrap();
        actual.push('\n');
        let expected = std::fs::read_to_string("docs/schema.json")
            .expect("docs/schema.json is missing; run `cargo run -- generate`");
        assert_eq!(actual, expected);
    }
}
