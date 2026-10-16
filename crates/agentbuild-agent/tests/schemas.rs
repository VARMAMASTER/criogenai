//! The published JSON Schemas (design 0013 §3): values serialized by the
//! Rust engine validate against `schemas/agentbuild.schema.json`, and
//! wrong values are rejected with useful paths.

use std::path::Path;
use std::time::Duration;

use agentbuild_agent::{Agent, Approval, Decision, ToolError, tool_fn, validate_arguments};
use agentbuild_providers::{
    BlockDelta, ContentBlock, FakeModel, FakeReply, MediaSource, Message, MessageDelta, Model,
    ModelError, ModelErrorKind, ModelRequest, OutputSpec, OutputStrategy, Role, Settings,
    ToolAnnotations, ToolChoice, ToolResultError, ToolSpec, ToolStatus,
};
use agentbuild_storage::{AuditEvent, AuditLog, InMemoryAuditLog};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

fn bundle() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/agentbuild.schema.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The problems `value` has as a `def`.
fn problems(def: &str, value: &Value) -> Vec<String> {
    let mut schema = bundle();
    assert!(schema["$defs"].get(def).is_some(), "no $defs/{def}");
    schema["$ref"] = json!(format!("#/$defs/{def}"));
    validate_arguments(&schema, value)
}

#[track_caller]
fn valid(def: &str, value: &impl Serialize) {
    let value = serde_json::to_value(value).unwrap();
    let found = problems(def, &value);
    assert!(found.is_empty(), "{def} {value}: {found:?}");
}

/// What Rust writes after reading `json`: for types the public API cannot
/// build directly.
fn round_trip<T: Serialize + DeserializeOwned>(json: Value) -> Value {
    serde_json::to_value(serde_json::from_value::<T>(json).unwrap()).unwrap()
}

fn args(value: Value) -> Map<String, Value> {
    value.as_object().unwrap().clone()
}

#[test]
fn every_content_block_and_message_kind() {
    let blocks = vec![
        ContentBlock::text("hi"),
        ContentBlock::reasoning("thinking"),
        ContentBlock::json(json!({"a": 1})),
        ContentBlock::image(
            MediaSource::Url {
                url: "https://x/y.png".into(),
            },
            None,
        ),
        ContentBlock::image(
            MediaSource::Base64 {
                data: "AAAA".into(),
            },
            Some("image/png".into()),
        ),
        ContentBlock::audio(
            MediaSource::Base64 {
                data: "AAAA".into(),
            },
            Some("audio/wav".into()),
        ),
        ContentBlock::file(
            MediaSource::FileId {
                file_id: "file-1".into(),
            },
            None,
        ),
        ContentBlock::tool_call("call_1", "lookup", args(json!({"q": "x"}))),
        ContentBlock::invalid_tool_call("call_2", Some("lookup".into()), "{bad", "not JSON"),
        ContentBlock::provider("anthropic", json!({"type": "server_tool_use"})),
        ContentBlock::text("with extras").with_extra("openai", json!({"refusal": null})),
    ];
    for block in &blocks {
        valid("ContentBlock", block);
    }
    valid("Message", &Message::with_content(Role::Assistant, blocks));
    valid("Message", &Message::system("Be brief.").named("rules"));
    valid("Message", &Message::user("hello"));
    valid(
        "Message",
        &Message::tool_result(
            "call_1",
            "lookup",
            ToolStatus::Success,
            vec![ContentBlock::json(json!([1]))],
        ),
    );
    valid(
        "Message",
        &Message::tool_error(
            "call_1",
            "lookup",
            ToolResultError::new("tool_error", "nope"),
        ),
    );
}

#[test]
fn requests_responses_and_deltas() {
    let tool = ToolSpec::new("lookup", "Looks up", json!({"type": "object"}))
        .with_annotations(ToolAnnotations::read_only());
    valid("ToolSpec", &tool);
    let mut output = OutputSpec::new(json!({"type": "object"}));
    output.strategy = Some(OutputStrategy::Tool);
    for choice in [
        ToolChoice::Auto,
        ToolChoice::None,
        ToolChoice::Required,
        ToolChoice::Tool("lookup".into()),
    ] {
        valid("ToolChoice", &choice);
        let mut request =
            ModelRequest::new(vec![Message::user("hi")]).with_tools(vec![tool.clone()]);
        request.tool_choice = choice;
        request.output = Some(output.clone());
        valid("ModelRequest", &request);
    }
    for settings in [
        json!({"max_tokens": 10, "temperature": 0.5, "stop": ["x"], "parallel_tool_calls": false,
               "reasoning": {"effort": "high"}}),
        json!({"reasoning": {"budget_tokens": 2048}}),
    ] {
        let settings: Settings = serde_json::from_value(settings).unwrap();
        valid("Settings", &settings);
        valid(
            "ModelRequest",
            &ModelRequest::new(vec![Message::user("hi")]).with_settings(settings),
        );
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
        .then(FakeReply::tool_call("lookup", json!({"q": "x"})));
    let response = runtime
        .block_on(model.invoke(&ModelRequest::new(vec![Message::user("hi")])))
        .unwrap();
    valid("ModelResponse", &response);
    valid("Usage", &response.usage);

    for delta in [
        json!({"type": "text", "text": "Hel"}),
        json!({"type": "reasoning", "text": "hmm", "signature": "sig"}),
        json!({"type": "tool_call", "id": "call_1", "name": "lookup", "args_fragment": "{\"q\""}),
        json!({"type": "block", "block": {"type": "text", "text": "whole"}}),
    ] {
        let delta =
            round_trip::<MessageDelta>(json!({"message_id": "m1", "index": 0, "delta": delta}));
        valid("MessageDelta", &delta);
        let inner = round_trip::<BlockDelta>(delta["delta"].clone());
        valid("BlockDelta", &inner);
    }
}

#[test]
fn every_model_error_kind() {
    for kind in [
        ModelErrorKind::RateLimited,
        ModelErrorKind::Overloaded,
        ModelErrorKind::Server,
        ModelErrorKind::Timeout,
        ModelErrorKind::Network,
        ModelErrorKind::Auth,
        ModelErrorKind::Permission,
        ModelErrorKind::NotFound,
        ModelErrorKind::InvalidRequest,
        ModelErrorKind::ContextOverflow,
        ModelErrorKind::ContentFilter,
        ModelErrorKind::QuotaExhausted,
        ModelErrorKind::InvalidResponse,
        ModelErrorKind::Unknown,
    ] {
        valid("ModelError", &ModelError::new(kind, "failed"));
    }
    valid(
        "ModelError",
        &round_trip::<ModelError>(json!({
            "kind": "rate_limited", "message": "slow down", "provider": "openai", "model": "m",
            "status": 429, "provider_code": "rate_limit_exceeded", "retry_after_ms": 1500,
            "request_id": "req_1", "dispatch": "streaming", "emitted_deltas": true,
        })),
    );
}

#[test]
fn approvals_decisions_runs_and_audit_records() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let agent = Agent::builder(
        FakeModel::with_delay(Duration::ZERO, Duration::ZERO)
            .then(FakeReply::tool_call("refund", json!({"order": "A-1"}))),
    )
    .tool(tool_fn(
        ToolSpec::new("refund", "Refunds", json!({"type": "object"})),
        |_args, _ctx| async { Ok::<_, ToolError>("done") },
    ))
    .approval(Approval::new().tool("refund"))
    .build()
    .unwrap();
    let result = runtime.block_on(agent.run("refund A-1")).unwrap();
    let requests = result.approval_requests();
    assert_eq!(requests.len(), 1);
    valid("ApprovalRequest", &requests[0]);
    // Messages the engine wrote, with metadata (model, usage, timing).
    for message in &result.messages {
        valid("Message", message);
    }

    for decision in [
        Decision::approve(),
        Decision::edit(args(json!({"order": "A-2"}))),
        Decision::reject("no"),
        Decision::respond("already refunded"),
        serde_json::from_value(json!({"type": "reject"})).unwrap(),
        serde_json::from_value(json!({"type": "edit", "name": "other", "args": {}})).unwrap(),
    ] {
        valid("Decision", &decision);
    }

    let log = InMemoryAuditLog::new();
    let first = runtime
        .block_on(log.append(AuditEvent::new("run.finished").outcome("completed")))
        .unwrap();
    let second = runtime
        .block_on(
            log.append(
                AuditEvent::new("approval.decided")
                    .actor(Some("user:dr-rao"))
                    .thread(Some("visit-1"))
                    .run(Some("r1"))
                    .subject("refund")
                    .outcome("approve")
                    .detail("call_id", "call_1"),
            ),
        )
        .unwrap();
    valid("AuditRecord", &first);
    valid("AuditRecord", &second);

    // An error as the SDKs report it.
    let failing =
        Agent::builder(FakeModel::with_delay(Duration::ZERO, Duration::ZERO).failing_on("BOOM"))
            .build()
            .unwrap();
    let error = runtime.block_on(failing.run("BOOM")).unwrap_err();
    valid(
        "ErrorObject",
        &json!({"code": error.code, "message": error.message, "details": error.details}),
    );
}

#[test]
fn wrong_values_are_rejected_with_paths() {
    let cases = [
        ("Message", json!({"role": "robot"}), "role: must be one of"),
        ("Message", json!({"content": []}), "role: required"),
        (
            "Message",
            json!({"role": "user", "content": [{"type": "text"}]}),
            "content[0].text: required",
        ),
        (
            "Message",
            json!({"role": "user", "content": [{"type": "video", "url": "x"}]}),
            "content[0].type: must be",
        ),
        (
            "ContentBlock",
            json!({"type": "tool_call", "id": "c", "name": "f"}),
            "args: required",
        ),
        ("ToolChoice", json!("sometimes"), "must be one of"),
        ("Decision", json!({"type": "respond"}), "message: required"),
        (
            "ModelError",
            json!({"kind": "on_fire", "message": "x"}),
            "kind: must be one of",
        ),
        (
            "AuditRecord",
            json!({"seq": 0, "at_ms": 1, "action": "x", "prev_hash": "0", "hash": "1"}),
            "seq: must be at least 1",
        ),
    ];
    for (def, value, needle) in cases {
        let found = problems(def, &value);
        assert!(
            found.iter().any(|p| p.contains(needle)),
            "{def} {value}: wanted {needle:?}, got {found:?}"
        );
    }
}
