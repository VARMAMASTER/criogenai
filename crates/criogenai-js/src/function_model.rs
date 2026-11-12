//! Models written in JavaScript (design 0021 §7): every model call goes to a
//! function. A model running in the page (WebGPU, a browser's built-in
//! model), a company's own client, or scripted replies chosen by what the
//! request says. Registered by handle, so a spec (`{"kind": "function",
//! "handle"}`) can name one anywhere a model goes: agents, fallbacks, the
//! gateway, agents inside workflows.
//!
//! The function gets the request as JSON (`ModelRequest`, as sent to
//! providers: messages, tools, settings, output) and answers with an
//! envelope: `{"text"}`, `{"toolCalls": [{name, args}]}`, `{"message"}` (an
//! exact assistant message), `{"error": {kind, message}}` (a classified
//! failure: `rate_limited` and `overloaded` are retried, as from a
//! provider), or `{"crash"}` (it threw).

// As in criogenai-providers: the `Ok` side (a whole message) is larger than
// `ModelError`, so boxing the error would not make the `Result` smaller.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use criogenai_providers::{
    BoxFuture, Capabilities, ContentBlock, FinishReason, Message, Model, ModelError,
    ModelErrorKind, ModelRequest, ModelResponse, Role, Usage,
};
use serde_json::{Map, Value};

use crate::{CallError, JsFn, lock, to_json};

fn functions() -> &'static Mutex<HashMap<u32, JsFn>> {
    static FUNCTIONS: OnceLock<Mutex<HashMap<u32, JsFn>>> = OnceLock::new();
    FUNCTIONS.get_or_init(Mutex::default)
}

/// Registers a JavaScript model function; the handle names it in specs.
pub fn register_function(function: JsFn) -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let handle = NEXT.fetch_add(1, Ordering::Relaxed);
    lock(functions()).insert(handle, function);
    handle
}

/// Forgets a function (its JavaScript object is gone). Models already built
/// from it keep working.
pub fn release_function(handle: u32) {
    lock(functions()).remove(&handle);
}

pub(crate) fn function_by_handle(handle: u32) -> Option<JsFn> {
    lock(functions()).get(&handle).cloned()
}

/// A model whose every call goes to a JavaScript function.
pub(crate) struct FunctionModel {
    function: JsFn,
    provider: String,
    model: String,
    calls: AtomicUsize,
}

impl FunctionModel {
    pub(crate) fn new(function: JsFn, name: &str) -> Self {
        Self {
            function,
            provider: "function".to_string(),
            model: name.to_string(),
            calls: AtomicUsize::new(0),
        }
    }

    fn fail(&self, kind: ModelErrorKind, message: impl AsRef<str>) -> ModelError {
        ModelError::new(kind, message).for_model(self.provider.clone(), self.model.clone())
    }

    async fn answer(&self, request: &ModelRequest) -> Result<ModelResponse, ModelError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let envelope = self
            .function
            .call(to_json(request).to_string())
            .await
            .map_err(|err| match err {
                CallError::Call(err) => self.fail(
                    ModelErrorKind::Unknown,
                    format!("could not call the model function: {err}"),
                ),
                CallError::Rejected(reason) => self.fail(
                    ModelErrorKind::Unknown,
                    format!("the model function failed: {reason}"),
                ),
            })?;
        let envelope: Value = serde_json::from_str(&envelope).map_err(|err| {
            self.fail(
                ModelErrorKind::InvalidResponse,
                format!("unreadable answer from the model function: {err}"),
            )
        })?;
        if let Some(crash) = envelope.get("crash").and_then(Value::as_str) {
            return Err(self.fail(
                ModelErrorKind::Unknown,
                format!("the model function threw: {crash}"),
            ));
        }
        if let Some(error) = envelope.get("error").filter(|e| e.is_object()) {
            let kind = error
                .get("kind")
                .cloned()
                .and_then(|kind| serde_json::from_value(kind).ok())
                .unwrap_or(ModelErrorKind::Unknown);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the model function reported an error");
            return Err(self.fail(kind, message));
        }
        let (content, finish) = if let Some(message) = envelope.get("message") {
            let message: Message = serde_json::from_value(message.clone()).map_err(|err| {
                self.fail(
                    ModelErrorKind::InvalidResponse,
                    format!("the model function's message is not a message: {err}"),
                )
            })?;
            let finish = match message.metadata.finish_reason {
                Some(reason) => reason,
                None if message.call_ids().next().is_some() => FinishReason::ToolCalls,
                None => FinishReason::Stop,
            };
            (message.content, finish)
        } else if let Some(calls) = envelope.get("toolCalls").and_then(Value::as_array) {
            let blocks = calls
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let args = match c.get("args") {
                        Some(Value::Object(map)) => map.clone(),
                        _ => Map::new(),
                    };
                    let name = c.get("name").and_then(Value::as_str).unwrap_or_default();
                    ContentBlock::tool_call(format!("call_{call}_{i}"), name, args)
                })
                .collect();
            (blocks, FinishReason::ToolCalls)
        } else if let Some(text) = envelope.get("text").and_then(Value::as_str) {
            (vec![ContentBlock::text(text)], FinishReason::Stop)
        } else {
            return Err(self.fail(
                ModelErrorKind::InvalidResponse,
                "the model function answered neither text, toolCalls, a message nor an error",
            ));
        };
        let mut message = Message::with_content(Role::Assistant, content);
        message.metadata.model = Some(self.model.clone());
        message.metadata.provider = Some(self.provider.clone());
        // Roughly four bytes per token: enough for budgets and cost hints.
        let tokens = |bytes: usize| (bytes as u64).div_ceil(4);
        let input: usize = request.messages.iter().map(|m| m.text().len()).sum();
        let usage = Usage::new(tokens(input), tokens(message.text().len()));
        Ok(ModelResponse::new(message, usage, finish))
    }
}

impl Model for FunctionModel {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> Capabilities {
        // Structured output goes through the `final_output` tool.
        let mut capabilities = Capabilities::default();
        capabilities.tools = Some(true);
        capabilities.parallel_tool_calls = Some(true);
        capabilities.native_structured_output = Some(false);
        capabilities.streaming = Some(false);
        capabilities
    }

    fn invoke<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(self.answer(request))
    }
}

/// The model of a `{"kind": "function", "handle", "name"}` spec.
pub(crate) fn function_model(spec: &Value) -> crate::Result<Arc<dyn Model>> {
    let handle = spec["handle"].as_u64().unwrap_or_default() as u32;
    let function = function_by_handle(handle)
        .ok_or_else(|| crate::bad("the model function is gone (it was released)"))?;
    let name = spec["name"].as_str().unwrap_or("model");
    Ok(Arc::new(FunctionModel::new(function, name)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::JsFunction;
    use serde_json::json;

    /// A "JavaScript" function in Rust: answers by the request's last text.
    struct Scripted(fn(&Value) -> Value);

    impl JsFunction for Scripted {
        fn call(&self, request: String) -> BoxFuture<'static, Result<String, CallError>> {
            let request: Value = serde_json::from_str(&request).unwrap();
            let answer = (self.0)(&request).to_string();
            Box::pin(async move { Ok(answer) })
        }
    }

    fn model(answer: fn(&Value) -> Value) -> Arc<dyn Model> {
        let handle = register_function(Arc::new(Scripted(answer)));
        function_model(&json!({"kind": "function", "handle": handle, "name": "scripted"})).unwrap()
    }

    fn ask(model: &Arc<dyn Model>, text: &str) -> Result<ModelResponse, ModelError> {
        let request = ModelRequest::new(vec![Message::user(text)]);
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(model.invoke(&request))
    }

    #[test]
    fn it_sees_the_request_and_answers_text_or_tool_calls() {
        let echo = model(|request| {
            let last = request["messages"].as_array().unwrap().last().unwrap();
            json!({"text": format!("echo: {}", last["content"][0]["text"].as_str().unwrap())})
        });
        let reply = ask(&echo, "hello").unwrap();
        assert_eq!(reply.message.text(), "echo: hello");
        assert_eq!(reply.finish_reason, FinishReason::Stop);
        assert_eq!(reply.message.metadata.provider.as_deref(), Some("function"));

        let tools =
            model(|_| json!({"toolCalls": [{"name": "add", "args": {"a": 1}}, {"name": "pi"}]}));
        let reply = ask(&tools, "go").unwrap();
        assert_eq!(reply.finish_reason, FinishReason::ToolCalls);
        let calls: Vec<_> = reply
            .message
            .tool_calls()
            .map(|c| c.name.to_string())
            .collect();
        assert_eq!(calls, ["add", "pi"]);
    }

    #[test]
    fn errors_keep_their_kind_and_crashes_are_unknown() {
        let busy = model(|_| json!({"error": {"kind": "rate_limited", "message": "slow down"}}));
        let error = ask(&busy, "x").unwrap_err();
        assert_eq!(error.kind, ModelErrorKind::RateLimited);
        assert!(error.kind.is_transient());

        let crash = model(|_| json!({"crash": "TypeError: x is undefined"}));
        let error = ask(&crash, "x").unwrap_err();
        assert_eq!(error.kind, ModelErrorKind::Unknown);
        assert!(error.message.contains("TypeError"));

        let nonsense = model(|_| json!({"what": 1}));
        assert_eq!(
            ask(&nonsense, "x").unwrap_err().kind,
            ModelErrorKind::InvalidResponse
        );
    }

    #[test]
    fn a_released_function_cannot_build_new_models() {
        let handle = register_function(Arc::new(Scripted(|_| json!({"text": "hi"}))));
        let built = function_model(&json!({"handle": handle})).unwrap();
        release_function(handle);
        assert!(function_model(&json!({"handle": handle})).is_err());
        // What was built keeps working.
        assert_eq!(ask(&built, "x").unwrap().message.text(), "hi");
    }
}
