//! Minimal template rendering for mockd responses.
//!
//! Templates are expressions of the form `{{ namespace.key }}` embedded in
//! string values inside a JSON response body. The supported namespaces are:
//!
//! - `path.<name>` — a captured path parameter, e.g. `{{path.id}}`.
//! - `query.<name>` — a query parameter, e.g. `{{query.role}}`.
//! - `header.<name>` — a request header, e.g. `{{header.x-tenant-id}}`.
//! - `body.<json.path>` — a value extracted from the JSON request body using
//!   dot navigation through objects and array indices, e.g.
//!   `{{body.user.name}}` or `{{body.items.0.id}}`.
//!
//! In addition, the following helper functions are available (called without
//! a namespace):
//!
//! - `{{uuid}}` — a fresh UUIDv4 string, e.g. `550e8400-e29b-41d4-a716-446655440000`.
//! - `{{now}}` — the current UTC time as an ISO 8601 string, e.g.
//!   `2024-01-15T12:34:56Z`.
//! - `{{randomInt(min,max)}}` — a random integer in the inclusive range
//!   `[min, max]`, e.g. `{{randomInt(1,100)}}`. Useful for generating IDs.
//!
//! ## Interpolation vs. coercion
//!
//! When a string value consists *exactly* of a single expression, the result is
//! coerced into the most appropriate JSON type:
//!
//! - numeric strings become JSON numbers (`{{path.id}}` with `id = 42` → `42`),
//! - `true` / `false` become booleans,
//! - `null` becomes JSON null,
//! - anything else stays a string.
//!
//! When an expression is part of a larger string, it is interpolated as text:
//!
//! `"user-{{path.id}}"` with `id = 42` → `"user-42"`.
//!
//! Unknown or missing variables resolve to an empty string during
//! interpolation, and to JSON null when used as a whole-value coercion.
//!
//! Note: helper functions such as `{{uuid}}` produce a fresh value on every
//! render and therefore never coerce to `null`.

use std::collections::HashMap;

use rand::Rng;
use serde_json::Value;
use time::macros::format_description;
use time::OffsetDateTime;
use uuid::Uuid;

/// Lookup tables used while rendering templates.
#[derive(Debug, Clone, Default)]
pub struct TemplateContext {
    /// Captured path parameters (e.g. `{"id": "42"}`).
    pub path: HashMap<String, String>,
    /// Query parameters.
    pub query: HashMap<String, String>,
    /// Request headers. Keys are expected to be lower-cased.
    pub headers: HashMap<String, String>,
    /// Parsed JSON request body (`Value::Null` when the body was not JSON).
    pub body: Value,
}

impl TemplateContext {
    /// Build an empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve an expression to a string value.
    ///
    /// The expression may be:
    /// - a helper function name (with or without arguments),
    /// - `path.<key>`, `query.<key>`, `header.<key>`, or
    /// - `body.<json.path>` (dot navigation through objects/arrays).
    ///
    /// Returns `None` when the expression is unknown or the value is absent.
    /// Helper functions never return `None`.
    pub fn lookup(&self, expression: &str) -> Option<String> {
        // Helper functions take priority; they have no namespace prefix
        // (or include parentheses for arguments).
        if let Some(value) = lookup_function(expression) {
            return Some(value);
        }

        let (namespace, rest) = expression.split_once('.')?;
        match namespace {
            "path" => self.path.get(rest).cloned(),
            "query" => self.query.get(rest).cloned(),
            "header" => self.header_lookup(rest),
            "body" => lookup_body(&self.body, rest),
            _ => None,
        }
    }

    /// Case-insensitive header lookup. Path/query use exact keys.
    fn header_lookup(&self, key: &str) -> Option<String> {
        if let Some(v) = self.headers.get(key) {
            return Some(v.clone());
        }
        let lower = key.to_ascii_lowercase();
        self.headers.get(&lower).cloned()
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Resolve one of the built-in helper functions, or return `None` if `expr`
/// does not name a function.
fn lookup_function(expr: &str) -> Option<String> {
    // Argument-taking form: `randomInt(min,max)`.
    if let Some(args) = expr
        .strip_prefix("randomInt(")
        .and_then(|s| s.strip_suffix(')'))
    {
        let (lo, hi) = args.split_once(',')?;
        let lo: i64 = lo.trim().parse().ok()?;
        let hi: i64 = hi.trim().parse().ok()?;
        if lo > hi {
            return None;
        }
        let n = rand::thread_rng().gen_range(lo..=hi);
        return Some(n.to_string());
    }

    // No-argument helpers.
    match expr {
        "uuid" => Some(Uuid::new_v4().to_string()),
        "now" => Some(format_now_iso8601()),
        "random" => {
            let n: i64 = rand::thread_rng().gen();
            Some(n.to_string())
        }
        _ => None,
    }
}

/// Format the current UTC time as an ISO 8601 string (`YYYY-MM-DDTHH:MM:SSZ`).
fn format_now_iso8601() -> String {
    // The macro produces a `&'static [BorrowedFormatItem]` at compile time,
    // so there is no per-call allocation and nothing to cache.
    let format = format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    OffsetDateTime::now_utc()
        .format(format)
        .unwrap_or_default()
}

/// Navigate `body` using dot-separated keys (object fields or array indices).
fn lookup_body(body: &Value, path: &str) -> Option<String> {
    let mut current = body;
    for key in path.split('.') {
        if key.is_empty() {
            return None;
        }
        current = match current {
            Value::Object(map) => map.get(key)?,
            Value::Array(arr) => {
                let idx: usize = key.parse().ok()?;
                arr.get(idx)?
            }
            _ => return None,
        };
    }
    Some(value_to_string(current))
}

/// Render a JSON value as a string suitable for template interpolation.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        // Objects and arrays are emitted as compact JSON.
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Render every string value inside `value`, returning a new [`Value`].
///
/// Non-string values (numbers, booleans, arrays, objects, null) are returned
/// unchanged, except that arrays and objects are traversed recursively.
pub fn render(value: &Value, ctx: &TemplateContext) -> Value {
    match value {
        Value::String(s) => render_string(s, ctx),
        Value::Array(items) => Value::Array(items.iter().map(|v| render(v, ctx)).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), render(v, ctx));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

/// Regex-free detection of a string that is *exactly* one template expression,
/// possibly surrounded by whitespace.
fn extract_single_expression(s: &str) -> Option<String> {
    let trimmed = s.trim();
    let inner = trimmed.strip_prefix("{{")?.strip_suffix("}}")?;
    let expr = inner.trim();
    // Must not contain another expression or closing markers in the middle.
    if expr.contains("{{") || expr.contains("}}") {
        return None;
    }
    Some(expr.to_string())
}

fn render_string(s: &str, ctx: &TemplateContext) -> Value {
    // Whole-string expression: attempt type coercion.
    if let Some(expr) = extract_single_expression(s) {
        return match ctx.lookup(&expr) {
            Some(raw) => coerce(&raw),
            None => Value::Null,
        };
    }

    // Otherwise: interpolate every `{{ ... }}` occurrence.
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start + 2..];
        match after_open.find("}}") {
            Some(end) => {
                let expr = after_open[..end].trim();
                if let Some(val) = ctx.lookup(expr) {
                    out.push_str(&val);
                }
                rest = &after_open[end + 2..];
            }
            None => {
                // Unbalanced `{{` — emit the rest verbatim.
                out.push_str(&rest[start..]);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    Value::String(out)
}

/// Coerce a raw string into the most appropriate JSON value.
fn coerce(raw: &str) -> Value {
    if raw.eq_ignore_ascii_case("true") {
        return Value::Bool(true);
    }
    if raw.eq_ignore_ascii_case("false") {
        return Value::Bool(false);
    }
    if raw.eq_ignore_ascii_case("null") {
        return Value::Null;
    }
    if let Ok(n) = raw.parse::<i64>() {
        return Value::from(n);
    }
    if let Ok(n) = raw.parse::<f64>() {
        if n.is_finite() {
            return serde_json::Number::from_f64(n)
                .map(Value::Number)
                .unwrap_or_else(|| Value::String(raw.to_string()));
        }
    }
    Value::String(raw.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ctx() -> TemplateContext {
        let mut c = TemplateContext::new();
        c.path.insert("id".into(), "42".into());
        c.query.insert("role".into(), "admin".into());
        c.headers.insert("x-tenant-id".into(), "tenant-a".into());
        c
    }

    fn ctx_with_body() -> TemplateContext {
        let mut c = ctx();
        c.body = json!({
            "user": {
                "name": "alice",
                "age": 30,
                "roles": ["admin", "editor"],
                "active": true,
            },
            "items": [
                { "id": 1, "label": "first" },
                { "id": 2, "label": "second" }
            ]
        });
        c
    }

    #[test]
    fn whole_string_number_is_coerced() {
        let v = render(&json!("{{path.id}}"), &ctx());
        assert_eq!(v, json!(42));
    }

    #[test]
    fn whole_string_bool_is_coerced() {
        let mut c = TemplateContext::new();
        c.path.insert("flag".into(), "true".into());
        let v = render(&json!("{{path.flag}}"), &c);
        assert_eq!(v, json!(true));
    }

    #[test]
    fn whole_string_null_is_coerced() {
        let mut c = TemplateContext::new();
        c.path.insert("nothing".into(), "null".into());
        let v = render(&json!("{{path.nothing}}"), &c);
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn whole_string_missing_is_null() {
        let v = render(&json!("{{path.missing}}"), &ctx());
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn interpolation_within_larger_string() {
        let v = render(&json!("user-{{path.id}}"), &ctx());
        assert_eq!(v, json!("user-42"));
    }

    #[test]
    fn interpolation_multiple_expressions() {
        let v = render(&json!("{{query.role}}@{{header.x-tenant-id}}"), &ctx());
        assert_eq!(v, json!("admin@tenant-a"));
    }

    #[test]
    fn header_lookup_is_case_insensitive() {
        let v = render(&json!("{{header.X-Tenant-Id}}"), &ctx());
        assert_eq!(v, json!("tenant-a"));
    }

    #[test]
    fn renders_nested_objects_and_arrays() {
        let body = json!({
            "id": "{{path.id}}",
            "label": "user-{{path.id}}",
            "meta": {
                "role": "{{query.role}}",
                "tenant": "{{header.x-tenant-id}}"
            },
            "tags": ["{{query.role}}", "static"]
        });
        let v = render(&body, &ctx());
        assert_eq!(
            v,
            json!({
                "id": 42,
                "label": "user-42",
                "meta": {
                    "role": "admin",
                    "tenant": "tenant-a"
                },
                "tags": ["admin", "static"]
            })
        );
    }

    #[test]
    fn leaves_non_string_values_untouched() {
        let body = json!({"a": 1, "b": true, "c": null});
        let v = render(&body, &ctx());
        assert_eq!(v, body);
    }

    #[test]
    fn unknown_namespace_yields_null() {
        let v = render(&json!("{{cookie.sid}}"), &ctx());
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn unbalanced_braces_emitted_verbatim() {
        let v = render(&json!("value {{oops"), &ctx());
        assert_eq!(v, json!("value {{oops"));
    }

    #[test]
    fn empty_expression_resolves_to_empty_string_when_interpolated() {
        // `{{}}` -> expr is empty -> lookup None -> interpolated as empty.
        let v = render(&json!("a{{}}b"), &ctx());
        assert_eq!(v, json!("ab"));
    }

    // -----------------------------------------------------------------
    // body.* navigation
    // -----------------------------------------------------------------

    #[test]
    fn body_lookup_object_field() {
        let v = render(&json!("{{body.user.name}}"), &ctx_with_body());
        assert_eq!(v, json!("alice"));
    }

    #[test]
    fn body_lookup_number_is_coerced() {
        let v = render(&json!("{{body.user.age}}"), &ctx_with_body());
        assert_eq!(v, json!(30));
    }

    #[test]
    fn body_lookup_array_index_then_field() {
        let v = render(&json!("{{body.items.1.label}}"), &ctx_with_body());
        assert_eq!(v, json!("second"));
    }

    #[test]
    fn body_lookup_missing_path_is_null() {
        let v = render(&json!("{{body.user.nope}}"), &ctx_with_body());
        assert_eq!(v, Value::Null);
    }

    #[test]
    fn body_lookup_interpolated_in_larger_string() {
        let v = render(&json!("hello {{body.user.name}}!"), &ctx_with_body());
        assert_eq!(v, json!("hello alice!"));
    }

    #[test]
    fn body_lookup_when_body_is_null() {
        // No JSON body -> Value::Null -> any body.* resolves to null.
        let v = render(&json!("{{body.user.name}}"), &TemplateContext::new());
        assert_eq!(v, Value::Null);
    }

    // -----------------------------------------------------------------
    // helper functions
    // -----------------------------------------------------------------

    #[test]
    fn uuid_renders_as_string() {
        let v = render(&json!("{{uuid}}"), &TemplateContext::new());
        let s = v.as_str().expect("uuid is a string");
        // Sanity check: 36 chars with hyphens in the right positions.
        assert_eq!(s.len(), 36);
        assert_eq!(s.chars().filter(|&c| c == '-').count(), 4);
    }

    #[test]
    fn uuid_is_unique_per_render() {
        let a = render(&json!("{{uuid}}"), &TemplateContext::new());
        let b = render(&json!("{{uuid}}"), &TemplateContext::new());
        assert_ne!(a, b);
    }

    #[test]
    fn uuid_within_larger_string() {
        let v = render(&json!("id-{{uuid}}"), &TemplateContext::new());
        let s = v.as_str().unwrap();
        assert!(s.starts_with("id-"));
        assert!(s.len() > 3);
    }

    #[test]
    fn now_renders_as_iso8601_string() {
        let v = render(&json!("{{now}}"), &TemplateContext::new());
        let s = v.as_str().expect("now is a string");
        // YYYY-MM-DDTHH:MM:SSZ -> 20 chars.
        assert_eq!(s.len(), 20);
        assert!(s.ends_with('Z'));
    }

    #[test]
    fn random_int_within_bounds() {
        for _ in 0..1000 {
            let v = render(&json!("{{randomInt(1,10)}}"), &TemplateContext::new());
            let n = v.as_i64().expect("randomInt yields a number");
            assert!((1..=10).contains(&n));
        }
    }

    #[test]
    fn random_int_single_value() {
        let v = render(&json!("{{randomInt(5,5)}}"), &TemplateContext::new());
        assert_eq!(v, json!(5));
    }

    #[test]
    fn random_int_inverted_range_resolves_to_null() {
        // lo > hi -> lookup_function returns None -> coercion to null.
        let v = render(&json!("{{randomInt(10,1)}}"), &TemplateContext::new());
        assert_eq!(v, Value::Null);
    }
}
