//! Tool-argument validation against a tool's JSON Schema (design 0002 §5.1).
//!
//! A deliberate subset of JSON Schema, enough for tool arguments: `type`
//! (one or a list), `properties`, `required`, `additionalProperties`
//! (`false` or a schema), `items`, `enum`, `const`, `minimum`, `maximum`,
//! `minLength`, `maxLength`, `minItems`, `maxItems`. Other keywords are
//! ignored, so a schema using them is accepted but not fully enforced.
//!
//! Messages name the path, so the model can correct itself:
//! `limit: expected integer, got "five"`.

use serde_json::{Map, Value};

/// Every problem with `value`, as `path: message` lines. Empty means valid.
pub fn validate(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    check(schema, value, "", &mut errors);
    errors
}

fn check(schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    let Some(schema) = schema.as_object() else {
        return; // `true` / `{}`-like schemas accept anything
    };
    let at = |path: &str| {
        if path.is_empty() {
            "(arguments)".to_string()
        } else {
            path.to_string()
        }
    };

    if let Some(types) = schema.get("type") {
        let allowed: Vec<&str> = match types {
            Value::String(t) => vec![t.as_str()],
            Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !allowed.is_empty() && !allowed.iter().any(|t| has_type(value, t)) {
            errors.push(format!(
                "{}: expected {}, got {}",
                at(path),
                allowed.join(" or "),
                describe(value)
            ));
            return; // further checks would only repeat the problem
        }
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(value)
    {
        let list: Vec<String> = options.iter().map(Value::to_string).collect();
        errors.push(format!(
            "{}: must be one of {}, got {}",
            at(path),
            list.join(", "),
            describe(value)
        ));
    }
    if let Some(expected) = schema.get("const")
        && expected != value
    {
        errors.push(format!("{}: must be {expected}", at(path)));
    }

    match value {
        Value::Number(n) => {
            let x = n.as_f64().unwrap_or(f64::NAN);
            if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
                && x < min
            {
                errors.push(format!("{}: must be at least {min}, got {n}", at(path)));
            }
            if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
                && x > max
            {
                errors.push(format!("{}: must be at most {max}, got {n}", at(path)));
            }
        }
        Value::String(s) => {
            let len = s.chars().count() as u64;
            if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
                && len < min
            {
                errors.push(format!("{}: must have at least {min} characters", at(path)));
            }
            if let Some(max) = schema.get("maxLength").and_then(Value::as_u64)
                && len > max
            {
                errors.push(format!("{}: must have at most {max} characters", at(path)));
            }
        }
        Value::Array(items) => {
            let len = items.len() as u64;
            if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
                && len < min
            {
                errors.push(format!("{}: must have at least {min} items", at(path)));
            }
            if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
                && len > max
            {
                errors.push(format!("{}: must have at most {max} items", at(path)));
            }
            if let Some(item_schema) = schema.get("items") {
                for (i, item) in items.iter().enumerate() {
                    check(item_schema, item, &format!("{path}[{i}]"), errors);
                }
            }
        }
        Value::Object(object) => check_object(schema, object, path, errors),
        _ => {}
    }
}

fn check_object(
    schema: &Map<String, Value>,
    object: &Map<String, Value>,
    path: &str,
    errors: &mut Vec<String>,
) {
    let join = |key: &str| {
        if path.is_empty() {
            key.to_string()
        } else {
            format!("{path}.{key}")
        }
    };
    let properties = schema.get("properties").and_then(Value::as_object);
    for required in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !object.contains_key(required) {
            errors.push(format!("{}: required", join(required)));
        }
    }
    // Sorted, so messages come in the same order whatever map type
    // serde_json was built with.
    let mut entries: Vec<(&String, &Value)> = object.iter().collect();
    entries.sort_by_key(|(key, _)| *key);
    for (key, value) in entries {
        match properties.and_then(|p| p.get(key)) {
            Some(property) => check(property, value, &join(key), errors),
            None => match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    errors.push(format!("{}: unknown argument", join(key)));
                }
                Some(extra @ Value::Object(_)) => check(extra, value, &join(key), errors),
                _ => {}
            },
        }
    }
}

fn has_type(value: &Value, name: &str) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => match value {
            Value::Number(n) => {
                n.is_i64() || n.is_u64() || n.as_f64().is_some_and(|f| f.fract() == 0.0)
            }
            _ => false,
        },
        _ => true, // unknown type names are not enforced
    }
}

/// A short description of a value for messages: strings and numbers are
/// shown, larger values are named.
fn describe(value: &Value) -> String {
    match value {
        Value::String(s) if s.chars().count() <= 40 => format!("{s:?}"),
        Value::String(_) => "a long string".into(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".into(),
        Value::Array(_) => "an array".into(),
        Value::Object(_) => "an object".into(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "minLength": 1},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                "mode": {"enum": ["fast", "full"]},
                "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 3},
                "filter": {
                    "type": "object",
                    "properties": {"year": {"type": ["integer", "null"]}},
                    "additionalProperties": false
                }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    #[test]
    fn valid_arguments_pass() {
        let args = json!({"query": "rust", "limit": 5, "mode": "fast",
                          "tags": ["a"], "filter": {"year": null}});
        assert!(validate(&schema(), &args).is_empty());
        assert!(validate(&schema(), &json!({"query": "x", "limit": 5.0})).is_empty());
    }

    #[test]
    fn every_problem_is_reported_with_its_path() {
        let args = json!({
            "limit": "five",
            "mode": "slow",
            "tags": ["a", 2, "c", "d"],
            "filter": {"year": "2020", "month": 1},
            "extra": true
        });
        let errors = validate(&schema(), &args);
        assert_eq!(
            errors,
            [
                "query: required",
                "extra: unknown argument",
                "filter.month: unknown argument",
                "filter.year: expected integer or null, got \"2020\"",
                "limit: expected integer, got \"five\"",
                "mode: must be one of \"fast\", \"full\", got \"slow\"",
                "tags: must have at most 3 items",
                "tags[1]: expected string, got 2",
            ]
        );
    }

    #[test]
    fn bounds_and_root_type() {
        let errors = validate(&schema(), &json!({"query": "", "limit": 0}));
        assert_eq!(
            errors,
            [
                "limit: must be at least 1, got 0",
                "query: must have at least 1 characters"
            ]
        );
        assert_eq!(
            validate(&schema(), &json!([1])),
            ["(arguments): expected object, got an array"]
        );
    }

    #[test]
    fn open_schemas_accept_extra_keys() {
        let open = json!({"type": "object", "properties": {"a": {"type": "string"}}});
        assert!(validate(&open, &json!({"a": "x", "b": 1})).is_empty());
        assert!(validate(&json!({}), &json!({"anything": [1, 2]})).is_empty());
    }
}
