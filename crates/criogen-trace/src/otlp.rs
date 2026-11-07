//! OTLP/HTTP JSON in, spans and run summaries out (design 0023).
//!
//! Everything received is data: ids must be hex, strings are cut, and a
//! malformed span is skipped rather than trusted.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value, json};

use crate::Prices;

/// A string attribute is cut to this many bytes (on a character boundary).
pub const MAX_ATTRIBUTE_BYTES: usize = 64 * 1024;
/// Spans kept per run; later ones are dropped.
pub const MAX_SPANS_PER_RUN: usize = 500;
/// Attributes kept per span.
const MAX_ATTRIBUTES: usize = 128;

/// One span as the viewer keeps it.
#[derive(Debug, Clone, PartialEq)]
pub struct SpanRow {
    pub trace_id: String,
    pub span_id: String,
    /// Start in nanoseconds since the epoch.
    pub start_ns: i64,
    /// The span as the API returns it.
    pub body: Value,
}

fn hex(value: Option<&Value>, len: usize) -> Option<String> {
    let text = value?.as_str()?;
    (text.len() == len && text.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| text.to_ascii_lowercase())
}

fn nanos(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::String(s) => s.parse::<u64>().ok().and_then(|n| i64::try_from(n).ok()),
        Value::Number(n) => n.as_u64().and_then(|n| i64::try_from(n).ok()),
        _ => None,
    }
}

/// `s` cut to at most `max` bytes, on a character boundary.
pub fn cut(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (cut)", &s[..end])
}

fn any_value(value: &Value, depth: usize) -> Value {
    if depth > 4 {
        return Value::Null;
    }
    if let Some(s) = value.get("stringValue").and_then(Value::as_str) {
        return Value::String(cut(s, MAX_ATTRIBUTE_BYTES));
    }
    if let Some(b) = value.get("boolValue").and_then(Value::as_bool) {
        return Value::Bool(b);
    }
    if let Some(i) = value.get("intValue") {
        return match i {
            Value::String(s) => s.parse::<i64>().map_or(Value::Null, |n| json!(n)),
            Value::Number(n) => Value::Number(n.clone()),
            _ => Value::Null,
        };
    }
    if let Some(d) = value.get("doubleValue").and_then(Value::as_f64) {
        return json!(d);
    }
    if let Some(items) = value
        .get("arrayValue")
        .and_then(|a| a.get("values"))
        .and_then(Value::as_array)
    {
        return Value::Array(
            items
                .iter()
                .take(256)
                .map(|v| any_value(v, depth + 1))
                .collect(),
        );
    }
    Value::Null
}

fn attributes(list: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for kv in list
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .take(MAX_ATTRIBUTES)
    {
        let Some(key) = kv.get("key").and_then(Value::as_str) else {
            continue;
        };
        let value = kv.get("value").map_or(Value::Null, |v| any_value(v, 0));
        out.insert(cut(key, 256), value);
    }
    out
}

fn kind(parent: &Option<String>, attrs: &Map<String, Value>) -> &'static str {
    match attrs
        .get("langfuse.observation.type")
        .and_then(Value::as_str)
    {
        Some("generation") => "model",
        Some("tool") => "tool",
        Some("event") => "event",
        _ if parent.is_none() => "run",
        _ => "node",
    }
}

/// The spans of an OTLP/HTTP JSON export request. Malformed spans are
/// skipped.
pub fn parse(request: &Value) -> Vec<SpanRow> {
    let mut rows = Vec::new();
    let resources = request.get("resourceSpans").and_then(Value::as_array);
    for resource in resources.into_iter().flatten() {
        let service = attributes(resource.get("resource").and_then(|r| r.get("attributes")))
            .get("service.name")
            .cloned()
            .unwrap_or(Value::Null);
        let scopes = resource.get("scopeSpans").and_then(Value::as_array);
        for scope in scopes.into_iter().flatten() {
            let spans = scope.get("spans").and_then(Value::as_array);
            for span in spans.into_iter().flatten() {
                let (Some(trace_id), Some(span_id)) =
                    (hex(span.get("traceId"), 32), hex(span.get("spanId"), 16))
                else {
                    continue;
                };
                let parent = hex(span.get("parentSpanId"), 16);
                let Some(start) = nanos(span.get("startTimeUnixNano")) else {
                    continue;
                };
                let end = nanos(span.get("endTimeUnixNano"))
                    .unwrap_or(start)
                    .max(start);
                let attrs = attributes(span.get("attributes"));
                let status = span.get("status");
                let code = status.and_then(|s| s.get("code")).and_then(Value::as_i64);
                let level = attrs
                    .get("langfuse.observation.level")
                    .and_then(Value::as_str);
                // The exporter's level wins: a tool error the model may
                // recover from is a warning (0009).
                let status_text = match (level, code) {
                    (Some("ERROR"), _) => "error",
                    (Some("WARNING"), _) => "warning",
                    (_, Some(2)) => "error",
                    _ => "ok",
                };
                let message = status
                    .and_then(|s| s.get("message"))
                    .and_then(Value::as_str)
                    .filter(|m| !m.is_empty())
                    .or_else(|| {
                        attrs
                            .get("langfuse.observation.status_message")
                            .and_then(Value::as_str)
                    })
                    .map(|m| cut(m, 4096));
                let name = span
                    .get("name")
                    .and_then(Value::as_str)
                    .map_or_else(|| "span".to_string(), |n| cut(n, 512));
                let body = json!({
                    "id": span_id,
                    "parent": parent,
                    "name": name,
                    "kind": kind(&parent, &attrs),
                    "start_ms": start as f64 / 1e6,
                    "end_ms": end as f64 / 1e6,
                    "status": status_text,
                    "message": message,
                    "service": service,
                    "attributes": attrs,
                });
                rows.push(SpanRow {
                    trace_id,
                    span_id,
                    start_ns: start,
                    body,
                });
            }
        }
    }
    rows
}

fn int(attrs: &Value, key: &str) -> i64 {
    attrs.get(key).and_then(Value::as_i64).unwrap_or(0)
}

/// A run's summary from its spans (any order).
pub fn summarize(trace_id: &str, spans: &[Value], prices: &Prices) -> Value {
    let root = spans
        .iter()
        .find(|s| s["parent"].is_null())
        .or_else(|| spans.first());
    let start = spans
        .iter()
        .filter_map(|s| s["start_ms"].as_f64())
        .fold(f64::INFINITY, f64::min);
    let end = spans
        .iter()
        .filter_map(|s| s["end_ms"].as_f64())
        .fold(f64::NEG_INFINITY, f64::max);
    let (mut model_calls, mut tool_calls, mut input, mut output) = (0, 0, 0i64, 0i64);
    let mut models = BTreeSet::new();
    let mut cost = Some(0.0f64);
    let mut by_model: HashMap<String, (i64, i64)> = HashMap::new();
    for span in spans {
        let attrs = &span["attributes"];
        match span["kind"].as_str() {
            Some("model") => {
                model_calls += 1;
                let i = int(attrs, "gen_ai.usage.input_tokens");
                let o = int(attrs, "gen_ai.usage.output_tokens");
                input += i;
                output += o;
                let model = attrs["gen_ai.request.model"]
                    .as_str()
                    .or_else(|| attrs["langfuse.observation.model.name"].as_str())
                    .unwrap_or("unknown");
                let provider = attrs["gen_ai.system"].as_str().unwrap_or_default();
                let key = if provider.is_empty() {
                    model.to_string()
                } else {
                    format!("{provider}:{model}")
                };
                models.insert(key.clone());
                let entry = by_model.entry(key).or_default();
                entry.0 += i;
                entry.1 += o;
            }
            Some("tool") => tool_calls += 1,
            _ => {}
        }
    }
    for (key, (i, o)) in &by_model {
        let model = key.split_once(':').map_or(key.as_str(), |(_, m)| m);
        match prices.get(key).or_else(|| prices.get(model)) {
            Some(p) => {
                cost = cost.map(|c| c + (*i as f64 * p.input + *o as f64 * p.output) / 1e6);
            }
            None => cost = None,
        }
    }
    let failed = spans.iter().any(|s| s["status"] == "error");
    let warnings = spans.iter().filter(|s| s["status"] == "warning").count();
    // The run's input, when the exporter captured content: a hint of what
    // the run was about (the first user text, cut).
    let hint = root_input(root);
    let root_attrs = root.map(|r| &r["attributes"]);
    let status = root_attrs
        .and_then(|a| a["criogen.status"].as_str())
        .map(str::to_string)
        .unwrap_or_else(|| if failed { "failed".into() } else { "ok".into() });
    let name = root_attrs
        .and_then(|a| a["langfuse.trace.name"].as_str())
        .or_else(|| root.and_then(|r| r["name"].as_str()))
        .unwrap_or("run");
    json!({
        "id": trace_id,
        "run_id": root_attrs.and_then(|a| a["criogen.run_id"].as_str()),
        "name": name,
        "start_ms": if start.is_finite() { json!(start) } else { Value::Null },
        "duration_ms": if start.is_finite() && end.is_finite() { json!(end - start) } else { json!(0) },
        "status": status,
        "failed": failed,
        "warnings": warnings,
        "input": hint,
        "model_calls": model_calls,
        "tool_calls": tool_calls,
        "input_tokens": input,
        "output_tokens": output,
        "models": models,
        "cost": if prices.is_empty() || by_model.is_empty() { Value::Null } else { json!(cost) },
        "spans": spans.len(),
    })
}

fn root_input(root: Option<&Value>) -> Option<String> {
    let raw = root?["attributes"]["langfuse.observation.input"].as_str()?;
    // Chat JSON: the last user message's text; else the raw text.
    let text = serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| {
            let messages = v.as_array()?.clone();
            messages
                .iter()
                .rev()
                .find(|m| m["role"] == "user")
                .and_then(|m| match &m["content"] {
                    Value::String(s) => Some(s.clone()),
                    Value::Array(parts) => parts
                        .iter()
                        .find_map(|p| p["text"].as_str().map(str::to_string)),
                    _ => None,
                })
        })
        .unwrap_or_else(|| raw.to_string());
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(match text.char_indices().nth(140) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_are_parsed_and_bad_ones_skipped() {
        let request = json!({"resourceSpans": [{"scopeSpans": [{"spans": [
            {"traceId": "0123456789abcdef0123456789abcdef", "spanId": "0123456789abcdef",
             "name": "agent", "startTimeUnixNano": "1000000000", "endTimeUnixNano": "3000000000",
             "attributes": [{"key": "criogen.status", "value": {"stringValue": "completed"}},
                {"key": "langfuse.observation.input", "value": {"stringValue": "[{\"role\":\"user\",\"content\":\"Weather   in Paris?\"}]"}}]},
            {"traceId": "nothex", "spanId": "0123456789abcdef", "startTimeUnixNano": "1"},
            {"traceId": "0123456789abcdef0123456789abcdef", "spanId": "1111111111111111",
             "parentSpanId": "0123456789abcdef", "name": "model",
             "startTimeUnixNano": "1500000000", "endTimeUnixNano": "2500000000",
             "attributes": [
                {"key": "langfuse.observation.type", "value": {"stringValue": "generation"}},
                {"key": "gen_ai.request.model", "value": {"stringValue": "gpt-5-mini"}},
                {"key": "gen_ai.system", "value": {"stringValue": "openai"}},
                {"key": "gen_ai.usage.input_tokens", "value": {"intValue": "1000"}},
                {"key": "gen_ai.usage.output_tokens", "value": {"intValue": 500}}]}
        ]}]}]});
        let rows = parse(&request);
        assert_eq!(rows.len(), 2);
        let bodies: Vec<Value> = rows.iter().map(|r| r.body.clone()).collect();
        let mut prices = Prices::new();
        prices.insert(
            "gpt-5-mini".into(),
            crate::Price {
                input: 1.0,
                output: 2.0,
            },
        );
        let s = summarize(&rows[0].trace_id, &bodies, &prices);
        assert_eq!(s["model_calls"], 1);
        assert_eq!(s["input_tokens"], 1000);
        // The run's input (captured content) is a hint, never the token count.
        assert_eq!(s["input"], "Weather in Paris?");
        assert_eq!(s["status"], "completed");
        assert_eq!(s["duration_ms"], 2000.0);
        assert_eq!(s["models"], json!(["openai:gpt-5-mini"]));
        assert!((s["cost"].as_f64().unwrap() - 0.002).abs() < 1e-12);
        // No price for the model: no cost rather than a wrong one.
        assert!(summarize("x", &bodies, &Prices::new())["cost"].is_null());
    }

    #[test]
    fn long_strings_are_cut_on_a_character_boundary() {
        let s = "é".repeat(10);
        let c = cut(&s, 5);
        assert!(c.starts_with("éé") && c.ends_with("(cut)"));
    }
}
