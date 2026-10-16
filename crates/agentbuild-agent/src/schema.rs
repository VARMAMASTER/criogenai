//! Tool-argument validation against a tool's JSON Schema (design 0002 §5.1).
//!
//! A deliberate subset of JSON Schema, enough for tool arguments and the
//! published contracts (design 0013 §4): `type` (one or a list),
//! `properties`, `required`, `additionalProperties` (`false` or a
//! schema), `items`, `enum`, `const`, `minimum`, `maximum`, `minLength`,
//! `maxLength`, `minItems`, `maxItems`, `anyOf`, `oneOf`, `allOf` and
//! local `$ref` (`#/$defs/…`, `#/definitions/…`). Other keywords
//! (`pattern`, `format`, remote references) are ignored, so a schema using
//! them is accepted but not fully enforced.
//!
//! Messages name the path, so the model can correct itself:
//! `limit: expected integer, got "five"`.

use serde_json::{Map, Value};

/// Every problem with `value`, as `path: message` lines. Empty means valid.
pub fn validate(schema: &Value, value: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    Checker {
        root: schema,
        depth: 0,
    }
    .check(schema, value, "", &mut errors);
    errors
}

/// How deep `$ref`s may nest without the value getting smaller: stops a
/// schema that refers to itself (`{"$ref": "#"}`) from looping.
const MAX_REF_DEPTH: u32 = 64;

/// The document `$ref`s point into, and how deep they have gone.
#[derive(Clone, Copy)]
struct Checker<'a> {
    root: &'a Value,
    depth: u32,
}

impl Checker<'_> {
    fn check(self, schema: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
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
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str)
            && let Some(target) = reference
                .strip_prefix('#')
                .and_then(|pointer| self.root.pointer(pointer))
        {
            if self.depth >= MAX_REF_DEPTH {
                errors.push(format!(
                    "{}: the schema refers to itself too deeply",
                    at(path)
                ));
                return;
            }
            let inner = Checker {
                root: self.root,
                depth: self.depth + 1,
            };
            inner.check(target, value, path, errors);
        }
        if let Some(all) = schema.get("allOf").and_then(Value::as_array) {
            for option in all {
                self.check(option, value, path, errors);
            }
        }
        for keyword in ["anyOf", "oneOf"] {
            let Some(options) = schema.get(keyword).and_then(Value::as_array) else {
                continue;
            };
            let results: Vec<Vec<String>> = options
                .iter()
                .map(|option| {
                    let mut found = Vec::new();
                    self.check(option, value, path, &mut found);
                    found
                })
                .collect();
            let matching = results.iter().filter(|r| r.is_empty()).count();
            if matching == 0 {
                // The closest option's problems say the most: one whose
                // `const` properties (a discriminator like `type`) match the
                // value, then the one with the fewest problems.
                let closest = options
                    .iter()
                    .zip(results)
                    .min_by_key(|(option, found)| {
                        (!discriminator_matches(option, value), found.len())
                    })
                    .map(|(_, found)| found);
                match closest {
                    Some(closest) if !closest.is_empty() => errors.extend(closest),
                    _ => errors.push(format!("{}: matches none of the allowed shapes", at(path))),
                }
            } else if keyword == "oneOf" && matching > 1 {
                errors.push(format!(
                    "{}: matches {matching} of the shapes, but must match exactly one",
                    at(path)
                ));
            }
        }

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
                        self.check(item_schema, item, &format!("{path}[{i}]"), errors);
                    }
                }
            }
            Value::Object(object) => self.check_object(schema, object, path, errors),
            _ => {}
        }
    }

    fn check_object(
        self,
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
                Some(property) => self.check(property, value, &join(key), errors),
                None => match schema.get("additionalProperties") {
                    Some(Value::Bool(false)) => {
                        errors.push(format!("{}: unknown argument", join(key)));
                    }
                    Some(extra @ Value::Object(_)) => self.check(extra, value, &join(key), errors),
                    _ => {}
                },
            }
        }
    }
}

/// `false` if `schema` has a property with a `const` that `value` (an
/// object) contradicts.
fn discriminator_matches(schema: &Value, value: &Value) -> bool {
    let (Some(properties), Some(object)) = (
        schema.get("properties").and_then(Value::as_object),
        value.as_object(),
    ) else {
        return true;
    };
    properties.iter().all(
        |(key, property)| match (property.get("const"), object.get(key)) {
            (Some(expected), Some(actual)) => expected == actual,
            _ => true,
        },
    )
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

    #[test]
    fn any_of_one_of_all_of_and_local_refs() {
        // What Pydantic writes for `tags: Optional[list[Tag]] = None`.
        let schema = json!({
            "$defs": {"Tag": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}},
            "type": "object",
            "properties": {
                "tags": {"anyOf": [{"type": "array", "items": {"$ref": "#/$defs/Tag"}}, {"type": "null"}]},
                "pick": {"oneOf": [{"type": "integer"}, {"type": "number", "minimum": 10}]},
                "both": {"allOf": [{"type": "string"}, {"maxLength": 3}]},
            }
        });
        assert!(validate(&schema, &json!({"tags": null, "pick": 1, "both": "abc"})).is_empty());
        assert!(validate(&schema, &json!({"tags": [{"name": "x"}]})).is_empty());
        assert_eq!(
            validate(&schema, &json!({"tags": [{"nom": "x"}]})),
            ["tags[0].name: required"]
        );
        assert_eq!(
            validate(&schema, &json!({"both": "abcd"})),
            ["both: must have at most 3 characters"]
        );
        // 12 is an integer and a number of at least 10: two shapes.
        assert_eq!(
            validate(&schema, &json!({"pick": 12})),
            ["pick: matches 2 of the shapes, but must match exactly one"]
        );
        assert_eq!(
            validate(&schema, &json!({"pick": "x"})),
            ["pick: expected integer, got \"x\""]
        );
        // Unknown or remote references are ignored, and a schema that
        // refers to itself cannot loop.
        assert!(validate(&json!({"$ref": "https://example.com/s.json"}), &json!(1)).is_empty());
        let looping = validate(&json!({"$ref": "#"}), &json!(1));
        assert_eq!(
            looping,
            ["(arguments): the schema refers to itself too deeply"]
        );
    }

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
