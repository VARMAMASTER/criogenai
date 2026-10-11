//! Hooks around the agent loop (design 0004 §3), with `FakeModel` on the
//! paused clock.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use agentbuild_agent::{
    Agent, Approval, Hook, HookAction, HookContext, HookError, ModelNext, Outcome, StopReason,
    ToolCallRequest, ToolError, ToolNext, ToolOutput, tool_fn,
};
use agentbuild_providers::{
    BoxFuture, ContentBlock, FakeModel, FakeReply, Message, ModelError, ModelErrorKind,
    ModelRequest, ModelResponse, Role, ToolSpec,
};
use serde_json::{Value, json};

type Log = Arc<Mutex<Vec<String>>>;

fn fake(replies: Vec<FakeReply>) -> Arc<FakeModel> {
    let mut model = FakeModel::with_delay(Duration::ZERO, Duration::ZERO);
    for reply in replies {
        model = model.then(reply);
    }
    Arc::new(model)
}

fn echo_tool(log: &Log) -> impl agentbuild_agent::Tool {
    let log = log.clone();
    tool_fn(
        ToolSpec::new(
            "echo",
            "",
            json!({"type": "object", "properties": {"text": {"type": "string"}}}),
        ),
        move |args, _| {
            let log = log.clone();
            async move {
                log.lock()
                    .unwrap()
                    .push(format!("tool echo {}", Value::Object(args.clone())));
                Ok::<_, ToolError>(
                    json!({"echo": args.get("text").cloned().unwrap_or(Value::Null)}),
                )
            }
        },
    )
}

/// Records every hook call as `<name>:<stage>`.
struct Recorder {
    name: &'static str,
    log: Log,
}

impl Recorder {
    fn note(&self, stage: &str) {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:{stage}", self.name));
    }
}

impl Hook for Recorder {
    fn name(&self) -> &str {
        self.name
    }
    fn on_start<'a>(&'a self, _: &'a HookContext) -> BoxFuture<'a, Result<HookAction, HookError>> {
        self.note("on_start");
        Box::pin(async { Ok(HookAction::proceed()) })
    }
    fn before_model<'a>(
        &'a self,
        _: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        self.note("before_model");
        Box::pin(async { Ok(HookAction::proceed()) })
    }
    fn wrap_model<'a>(
        &'a self,
        request: ModelRequest,
        next: ModelNext<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move {
            self.note("wrap_model>");
            let response = next.call(request).await;
            self.note("wrap_model<");
            response
        })
    }
    fn after_model<'a>(
        &'a self,
        _: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        self.note("after_model");
        Box::pin(async { Ok(HookAction::proceed()) })
    }
    fn wrap_tool<'a>(
        &'a self,
        call: ToolCallRequest,
        next: ToolNext<'a>,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            self.note("wrap_tool>");
            let result = next.call(call).await;
            self.note("wrap_tool<");
            result
        })
    }
    fn on_end<'a>(&'a self, _: &'a HookContext, outcome: Outcome) -> BoxFuture<'a, ()> {
        self.note(&format!("on_end({outcome:?})"));
        Box::pin(async {})
    }
}

#[tokio::test(start_paused = true)]
async fn hooks_run_in_onion_order() {
    let log = Log::default();
    let mut builder = Agent::builder_shared(fake(vec![
        FakeReply::tool_call("echo", json!({"text": "hi"})),
        FakeReply::text("done"),
    ]))
    .tool(echo_tool(&log));
    for name in ["A", "B", "C"] {
        builder = builder.hook(Recorder {
            name,
            log: log.clone(),
        });
    }
    let result = builder.build().unwrap().run("go").await.unwrap();
    assert_eq!(result.text(), Some("done"));
    let log = log.lock().unwrap().clone();
    let turn = [
        "A:before_model",
        "B:before_model",
        "C:before_model",
        "A:wrap_model>",
        "B:wrap_model>",
        "C:wrap_model>",
        "C:wrap_model<",
        "B:wrap_model<",
        "A:wrap_model<",
        "C:after_model",
        "B:after_model",
        "A:after_model",
    ];
    let mut expected: Vec<&str> = vec!["A:on_start", "B:on_start", "C:on_start"];
    expected.extend(turn);
    expected.extend([
        "A:wrap_tool>",
        "B:wrap_tool>",
        "C:wrap_tool>",
        r#"tool echo {"text":"hi"}"#,
        "C:wrap_tool<",
        "B:wrap_tool<",
        "A:wrap_tool<",
    ]);
    expected.extend(turn);
    expected.extend([
        "C:on_end(Completed)",
        "B:on_end(Completed)",
        "A:on_end(Completed)",
    ]);
    assert_eq!(log, expected);
}

/// Keeps only the last two messages before each model call.
struct Summarizer;

impl Hook for Summarizer {
    fn name(&self) -> &str {
        "summarizer"
    }
    fn before_model<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        Box::pin(async move {
            if ctx.messages.len() <= 2 {
                return Ok(HookAction::proceed());
            }
            let mut kept = vec![Message::user("(summary of earlier messages)").with_id("summary")];
            kept.extend(ctx.messages[ctx.messages.len() - 1..].iter().cloned());
            Ok(HookAction::proceed().replace_messages(kept))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn before_model_can_rewrite_the_transcript() {
    let model = fake(vec![FakeReply::text("short answer")]);
    let agent = Agent::builder_shared(model.clone())
        .hook(Summarizer)
        .build()
        .unwrap();
    let history = vec![
        Message::user("one"),
        Message::assistant("two"),
        Message::user("three"),
        Message::assistant("four"),
        Message::user("five"),
    ];
    let result = agent.run(history).await.unwrap();
    let sent: Vec<String> = model.requests()[0]
        .messages
        .iter()
        .map(Message::text)
        .collect();
    assert_eq!(sent, ["(summary of earlier messages)", "five"]);
    let stored: Vec<String> = result.messages.iter().map(Message::text).collect();
    assert_eq!(
        stored,
        ["(summary of earlier messages)", "five", "short answer"]
    );
}

/// Answers from a cache without calling the model (0004 A7).
struct CacheHit;

impl Hook for CacheHit {
    fn name(&self) -> &str {
        "cache"
    }
    fn before_model<'a>(
        &'a self,
        _: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        Box::pin(async { Ok(HookAction::proceed().end(Some(json!("cached answer")))) })
    }
}

#[tokio::test(start_paused = true)]
async fn before_model_can_end_the_run_without_a_model_call() {
    let model = fake(vec![FakeReply::text("never")]);
    let agent = Agent::builder_shared(model.clone())
        .hook(CacheHit)
        .build()
        .unwrap();
    let result = agent.run("hi").await.unwrap();
    assert_eq!(result.stop_reason, Some(StopReason::HookEnd));
    assert_eq!(result.output, Some(json!("cached answer")));
    assert_eq!(model.calls(), 0);
    assert_eq!(result.model_calls, 0);
}

/// Redacts phone numbers from answers, and stops if the answer leaks a
/// secret.
struct Guardrail;

impl Hook for Guardrail {
    fn name(&self) -> &str {
        "guardrail"
    }
    fn after_model<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        Box::pin(async move {
            let message = ctx.messages.last().unwrap();
            let text = message.text();
            if text.contains("SECRET") {
                return Ok(HookAction::proceed().end(Some(json!("I can't share that."))));
            }
            if text.contains("555-0100") {
                let redacted = Message::with_content(
                    Role::Assistant,
                    vec![ContentBlock::text(text.replace("555-0100", "[phone]"))],
                )
                .with_id(message.id.clone());
                return Ok(HookAction::proceed().replace_message(redacted));
            }
            Ok(HookAction::proceed())
        })
    }
}

#[tokio::test(start_paused = true)]
async fn after_model_can_replace_the_message_or_end() {
    let agent = Agent::builder_shared(fake(vec![FakeReply::text("Call 555-0100.")]))
        .hook(Guardrail)
        .build()
        .unwrap();
    let result = agent.run("phone?").await.unwrap();
    assert_eq!(result.text(), Some("Call [phone]."));
    assert_eq!(result.messages.last().unwrap().text(), "Call [phone].");

    let model = fake(vec![
        FakeReply::text("The password is SECRET-123."),
        FakeReply::text("never reached"),
    ]);
    let agent = Agent::builder_shared(model.clone())
        .hook(Guardrail)
        .build()
        .unwrap();
    let stopped = agent.run("password?").await.unwrap();
    assert_eq!(stopped.stop_reason, Some(StopReason::HookEnd));
    assert_eq!(stopped.output, Some(json!("I can't share that.")));
    assert_eq!(model.calls(), 1);
}

/// On context overflow, retries the call with only the last message
/// (0003 E15).
struct TrimOnOverflow;

impl Hook for TrimOnOverflow {
    fn name(&self) -> &str {
        "trim"
    }
    fn wrap_model<'a>(
        &'a self,
        request: ModelRequest,
        next: ModelNext<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move {
            match next.clone().call(request.clone()).await {
                Err(error) if error.kind == ModelErrorKind::ContextOverflow => {
                    let mut trimmed = request;
                    let last = trimmed.messages.pop().unwrap();
                    trimmed.messages = vec![last];
                    next.call(trimmed).await
                }
                other => other,
            }
        })
    }
}

#[tokio::test(start_paused = true)]
async fn wrap_model_can_recover_from_context_overflow() {
    let model = fake(vec![
        FakeReply::Error(ModelError::new(ModelErrorKind::ContextOverflow, "too long")),
        FakeReply::text("fits now"),
    ]);
    let agent = Agent::builder_shared(model.clone())
        .hook(TrimOnOverflow)
        .build()
        .unwrap();
    let history = vec![
        Message::user("a"),
        Message::assistant("b"),
        Message::user("c"),
    ];
    let result = agent.run(history).await.unwrap();
    assert_eq!(result.text(), Some("fits now"));
    assert_eq!(model.requests()[1].messages.len(), 1);
    assert_eq!(
        result.model_calls, 1,
        "one model call from the loop's point of view"
    );
}

/// Upper-cases tool arguments and serves `cached` without running the tool.
struct ToolShim;

impl Hook for ToolShim {
    fn name(&self) -> &str {
        "shim"
    }
    fn wrap_tool<'a>(
        &'a self,
        mut call: ToolCallRequest,
        next: ToolNext<'a>,
    ) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(async move {
            let text = call
                .args
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if text == "cached" {
                return Ok(ToolOutput::Text("from cache".into()));
            }
            if text == "forbidden" {
                return Err(ToolError::new("blocked by policy"));
            }
            call.args.insert("text".into(), json!(text.to_uppercase()));
            next.call(call).await
        })
    }
}

#[tokio::test(start_paused = true)]
async fn wrap_tool_can_change_short_circuit_or_block_calls() {
    let log = Log::default();
    let agent = Agent::builder_shared(fake(vec![
        FakeReply::tool_calls([
            ("echo", json!({"text": "loud"})),
            ("echo", json!({"text": "cached"})),
            ("echo", json!({"text": "forbidden"})),
        ]),
        FakeReply::text("ok"),
    ]))
    .tool(echo_tool(&log))
    .hook(ToolShim)
    .build()
    .unwrap();
    let result = agent.run("x").await.unwrap();
    assert_eq!(
        *log.lock().unwrap(),
        [r#"tool echo {"text":"LOUD"}"#],
        "only one real call"
    );
    let results: Vec<String> = result
        .messages
        .iter()
        .filter(|m| m.role == Role::Tool)
        .map(Message::text)
        .collect();
    assert_eq!(results[1], "from cache");
    assert_eq!(results[2], "Error: blocked by policy");
}

/// Counts model calls in its own state.
struct Counter;

impl Hook for Counter {
    fn name(&self) -> &str {
        "counter"
    }
    fn before_model<'a>(
        &'a self,
        ctx: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        Box::pin(async move {
            let seen = ctx.hook_state.as_ref().and_then(Value::as_u64).unwrap_or(0);
            Ok(HookAction::proceed().with_state(json!(seen + 1)))
        })
    }
}

#[tokio::test(start_paused = true)]
async fn hook_state_persists_across_turns() {
    let agent = Agent::builder_shared(fake(vec![
        FakeReply::tool_call("echo", json!({"text": "1"})),
        FakeReply::tool_call("echo", json!({"text": "2"})),
        FakeReply::text("done"),
    ]))
    .tool(echo_tool(&Log::default()))
    .hook(Counter)
    .build()
    .unwrap();
    let result = agent.run("x").await.unwrap();
    assert_eq!(result.run.state.get("hooks").unwrap()["counter"], 3);
}

struct Broken;

impl Hook for Broken {
    fn name(&self) -> &str {
        "broken"
    }
    fn after_model<'a>(
        &'a self,
        _: &'a HookContext,
    ) -> BoxFuture<'a, Result<HookAction, HookError>> {
        Box::pin(async { Err(HookError::new("database unavailable")) })
    }
}

#[tokio::test(start_paused = true)]
async fn a_failing_hook_fails_the_run_and_on_end_still_runs() {
    let log = Log::default();
    let agent = Agent::builder_shared(fake(vec![FakeReply::text("hi")]))
        .hook(Recorder {
            name: "R",
            log: log.clone(),
        })
        .hook(Broken)
        .build()
        .unwrap();
    let err = agent.run("x").await.unwrap_err();
    assert_eq!(err.code, "hook_failed");
    assert_eq!(err.details.as_ref().unwrap()["stage"], "after_model");
    assert!(err.message.contains("database unavailable"));
    assert!(
        err.snapshot().is_some(),
        "the run can be retried after a fix"
    );
    assert_eq!(log.lock().unwrap().last().unwrap(), "R:on_end(Failed)");
}

#[tokio::test(start_paused = true)]
async fn on_end_runs_for_suspended_segments_too() {
    let log = Log::default();
    let agent = Agent::builder_shared(fake(vec![
        FakeReply::tool_call("echo", json!({"text": "x"})),
        FakeReply::text("done"),
    ]))
    .tool(echo_tool(&log))
    .approval(Approval::new().tool("echo"))
    .hook(Recorder {
        name: "R",
        log: log.clone(),
    })
    .build()
    .unwrap();
    let paused = agent.run("x").await.unwrap();
    let call_id = paused.approval_requests()[0].call_id.clone();
    agent
        .resume(
            paused.snapshot().unwrap(),
            [(call_id, agentbuild_agent::Decision::approve())],
        )
        .await
        .unwrap();
    let starts_and_ends: Vec<String> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|e| e.contains("on_"))
        .cloned()
        .collect();
    assert_eq!(
        starts_and_ends,
        [
            "R:on_start",
            "R:on_end(Suspended)",
            "R:on_start",
            "R:on_end(Completed)"
        ]
    );
}

#[test]
fn duplicate_hook_names_fail_the_build() {
    let err = Agent::builder_shared(fake(vec![]))
        .hook(Counter)
        .hook(Counter)
        .build()
        .unwrap_err();
    assert_eq!(err.to_string(), "hook `counter` is added more than once");
}
