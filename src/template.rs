//! Minimal template rendering for mockd responses.
//!
//! Templates are expressions of the form `{{ namespace.key }}` embedded in
//! string values inside a JSON response body. The supported namespaces are:
//!
//! - `path.<name>` — a captured path parameter, e.g. `{{path.id}}`.
//! - `query.<name>` — a query parameter, e.g. `{{query.role}}`.
//! - `header.<name>` — a request header, e.g. `{{header.x-tenant-id}}`.
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

use std::collections::HashMap;

use serde_json::Value;

/// Lookup tables used while rendering templates.
#[derive(Debug, Clone, Default)]
pub struct TemplateContext {
    /// Captured path parameters (e.g. `{"id": "42"}`).
    pub path: HashMap<String, String>,
    /// Query parameters.
    pub query: HashMap<String, String>,
    /// Request headers. Keys are expected to be lower-cased.
    pub headers: HashMap<String, String>,
}

impl TemplateContext {
    /// Build an empty context.
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve `path.<key>`, `query.<key>` or `header.<key>` to a string value.
    ///
    /// Returns `None` when the namespace is unknown or the variable is absent.
    pub fn lookup(&self, expression: &str) -> Option<&str> {
        let (namespace, rest) = expression.split_once('.')?;
        let map = match namespace {
            "path" => &self.path,
            "query" => &self.query,
            "header" => &self.headers,
            _ => return None,
        };
        // Headers are matched case-insensitively. For path/query we use exact
        // keys, but a case-insensitive fallback keeps header lookups ergonomic
        // regardless of how the caller cased the key.
        if let Some(v) = map.get(rest) {
            return Some(v.as_str());
        }
        if namespace == "header" {
            let lower = rest.to_ascii_lowercase();
            map.get(&lower).map(String::as_str)
        } else {
            None
        }
    }
}

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
            Some(raw) => coerce(raw),
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
                    out.push_str(val);
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
}
