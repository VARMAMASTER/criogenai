//! OTLP/HTTP JSON encoding of spans (`ExportTraceServiceRequest`).
//!
//! Follows the OTLP JSON mapping: trace and span ids are lowercase hex,
//! 64-bit integers are strings, enums are numbers.

use serde_json::{Value, json};

use crate::spans::{Attr, Span, SpanEvent, Status};

/// `SPAN_KIND_INTERNAL`.
const KIND_INTERNAL: u8 = 1;

/// One export request body for `spans`.
pub(crate) fn encode(spans: &[Span], service_name: &str) -> Value {
    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": [
                    kv("service.name", &Attr::Str(service_name.to_string())),
                    kv("telemetry.sdk.name", &Attr::Str("criogen".into())),
                    kv("telemetry.sdk.language", &Attr::Str("rust".into())),
                    kv("telemetry.sdk.version", &Attr::Str(env!("CARGO_PKG_VERSION").into())),
                ]
            },
            "scopeSpans": [{
                "scope": {"name": "criogen", "version": env!("CARGO_PKG_VERSION")},
                "spans": spans.iter().map(span).collect::<Vec<_>>(),
            }]
        }]
    })
}

fn span(span: &Span) -> Value {
    let mut value = json!({
        "traceId": span.trace_id,
        "spanId": span.span_id,
        "name": span.name,
        "kind": KIND_INTERNAL,
        "startTimeUnixNano": nanos(span.start_ms),
        "endTimeUnixNano": nanos(span.end_ms.max(span.start_ms)),
        "attributes": span.attributes.iter().map(|(k, v)| kv(k, v)).collect::<Vec<_>>(),
        "status": status(&span.status),
    });
    if let Some(parent) = &span.parent_id {
        value["parentSpanId"] = json!(parent);
    }
    if !span.events.is_empty() {
        value["events"] = span.events.iter().map(event).collect();
    }
    value
}

fn event(event: &SpanEvent) -> Value {
    json!({
        "timeUnixNano": nanos(event.time_ms),
        "name": event.name,
        "attributes": event.attributes.iter().map(|(k, v)| kv(k, v)).collect::<Vec<_>>(),
    })
}

fn status(status: &Status) -> Value {
    match status {
        Status::Unset => json!({"code": 0}),
        Status::Ok => json!({"code": 1}),
        Status::Error(message) => json!({"code": 2, "message": message}),
    }
}

fn kv(key: &str, value: &Attr) -> Value {
    json!({"key": key, "value": any_value(value)})
}

fn any_value(value: &Attr) -> Value {
    match value {
        Attr::Str(s) => json!({"stringValue": s}),
        Attr::Int(i) => json!({"intValue": i.to_string()}),
        Attr::Bool(b) => json!({"boolValue": b}),
        Attr::StrList(items) => json!({
            "arrayValue": {"values": items.iter().map(|s| json!({"stringValue": s})).collect::<Vec<_>>()}
        }),
    }
}

/// Milliseconds as a nanosecond string (OTLP JSON encodes 64-bit integers
/// as strings).
fn nanos(ms: u64) -> String {
    format!("{ms}000000")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_otlp_json_shape() {
        let spans = vec![Span {
            trace_id: "0123456789abcdef0123456789abcdef".into(),
            span_id: "0123456789abcdef".into(),
            parent_id: Some("fedcba9876543210".into()),
            name: "model".into(),
            start_ms: 1_700_000_000_000,
            end_ms: 1_700_000_000_250,
            attributes: vec![
                ("a".into(), Attr::Str("x".into())),
                ("n".into(), Attr::Int(7)),
                ("b".into(), Attr::Bool(true)),
                ("tags".into(), Attr::StrList(vec!["t1".into()])),
            ],
            events: vec![SpanEvent {
                name: "warning".into(),
                time_ms: 1_700_000_000_100,
                attributes: vec![],
            }],
            status: Status::Error("boom".into()),
        }];
        let body = encode(&spans, "svc");
        let resource = &body["resourceSpans"][0];
        assert_eq!(
            resource["resource"]["attributes"][0]["value"]["stringValue"],
            "svc"
        );
        let span = &resource["scopeSpans"][0]["spans"][0];
        assert_eq!(span["traceId"], "0123456789abcdef0123456789abcdef");
        assert_eq!(span["parentSpanId"], "fedcba9876543210");
        assert_eq!(span["kind"], 1);
        assert_eq!(span["startTimeUnixNano"], "1700000000000000000");
        assert_eq!(span["endTimeUnixNano"], "1700000000250000000");
        assert_eq!(span["attributes"][1]["value"]["intValue"], "7");
        assert_eq!(span["attributes"][2]["value"]["boolValue"], true);
        assert_eq!(
            span["attributes"][3]["value"]["arrayValue"]["values"][0]["stringValue"],
            "t1"
        );
        assert_eq!(span["events"][0]["name"], "warning");
        assert_eq!(span["status"], json!({"code": 2, "message": "boom"}));
    }
}
