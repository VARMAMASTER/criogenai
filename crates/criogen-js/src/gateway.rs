//! The gateway from TypeScript (design 0018): model calls without an agent,
//! with the agent's retries and fallbacks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use criogen_agent::{ResilientModel, validate_arguments};
use criogen_core::{CancelToken, RetryPolicy};
use criogen_providers::{
    BlockDelta, BoxFuture, Message, MessageDelta, Model, ModelError, ModelErrorKind, ModelRequest,
    ModelResponse, OutputSpec, Settings,
};
use serde_json::{Value, json};

use crate::config::{embedder, model, retry_policy};
use crate::{Failure, Registered, Result, bad, failure, lock, parse, text, to_json};

/// The messages of `input`: text, or messages whose `content` may be a
/// string (one text block).
fn gateway_messages(input: &Value) -> Result<Vec<Message>> {
    match input {
        Value::String(text) => Ok(vec![Message::user(text.clone())]),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                let mut item = item.clone();
                if let Some(text) = item.get("content").and_then(Value::as_str) {
                    item["content"] = json!([{"type": "text", "text": text}]);
                }
                serde_json::from_value(item).map_err(|err| bad(format!("a message: {err}")))
            })
            .collect(),
        _ => Err(bad("input is text or a list of messages")),
    }
}

/// The request of a `chat` / `stream` payload. `tools` (tool specs, as a
/// model function receives them) lets the model answer with tool calls,
/// which the caller runs: the reply's `message` holds them.
fn gateway_request(payload: &Value) -> Result<ModelRequest> {
    let mut messages = gateway_messages(&payload["input"])?;
    if let Some(instructions) = text(payload, "instructions") {
        messages.insert(0, Message::system(instructions));
    }
    let mut settings = Settings::default();
    settings.temperature = payload["temperature"].as_f64();
    settings.max_tokens = payload["maxTokens"]
        .as_u64()
        .map(|n| u32::try_from(n).unwrap_or(u32::MAX));
    let mut request = ModelRequest::new(messages).with_settings(settings);
    if let Some(schema) = payload.get("output").filter(|o| !o.is_null()) {
        request.output = Some(OutputSpec::new(schema.clone()));
    }
    if let Some(tools) = payload.get("tools").filter(|t| t.is_array()) {
        request.tools =
            serde_json::from_value(tools.clone()).map_err(|err| bad(format!("tools: {err}")))?;
    }
    Ok(request)
}

fn model_failure(error: ModelError) -> Failure {
    failure(
        "model_failed",
        error.to_string(),
        Some(json!({"model_error": error})),
    )
}

/// The reply, with the parsed and checked output when one was asked for.
fn chat_reply(response: ModelResponse, schema: Option<&Value>) -> Result<Value> {
    let text = response.message.text();
    let output = match schema {
        None => Value::Null,
        Some(schema) => {
            let value: Value = serde_json::from_str(text.trim()).map_err(|err| {
                failure(
                    "output_invalid",
                    format!("the reply is not JSON: {err}"),
                    None,
                )
            })?;
            let problems = validate_arguments(schema, &value);
            if !problems.is_empty() {
                return Err(failure(
                    "output_invalid",
                    format!(
                        "the reply does not match the schema: {}",
                        problems.join("; ")
                    ),
                    Some(json!({"problems": problems})),
                ));
            }
            value
        }
    };
    let meta = &response.message.metadata;
    let model = format!(
        "{}:{}",
        meta.provider.as_deref().unwrap_or_default(),
        meta.model.as_deref().unwrap_or_default()
    );
    Ok(json!({
        "text": text,
        "message": to_json(&response.message),
        "usage": to_json(&response.usage),
        "stopReason": to_json(&response.finish_reason),
        "model": model,
        "output": output,
    }))
}

/// `work`, or `cancelled` once `cancel` fires (the HTTP request is dropped).
async fn or_cancelled<T>(
    work: impl std::future::Future<Output = Result<T>>,
    cancel: CancelToken,
) -> Result<T> {
    use std::future::Future as _;
    let mut work = std::pin::pin!(work);
    let mut cancelled = std::pin::pin!(cancel.cancelled());
    std::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(outcome) = work.as_mut().poll(cx) {
            return std::task::Poll::Ready(outcome);
        }
        if cancelled.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(Err(failure(
                "cancelled",
                "the call was cancelled",
                None,
            )));
        }
        std::task::Poll::Pending
    })
    .await
}

/// Model calls without an agent: the agent's retries and fallbacks
/// (`ResilientModel`) around the model each call names.
pub struct Gateway {
    fallbacks: Vec<Arc<dyn Model>>,
    fallback_on: Vec<ModelErrorKind>,
    retry: RetryPolicy,
    max_retry_after: Duration,
    /// Models by their spec: each provider client is made once.
    models: Mutex<HashMap<String, Arc<dyn Model>>>,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gateway")
            .field("fallbacks", &self.fallbacks.len())
            .field("fallback_on", &self.fallback_on)
            .finish_non_exhaustive()
    }
}

impl Gateway {
    /// Config `{fallbacks, fallbackOn, retry, maxRetryAfterSecs}`.
    pub fn new(config: &str) -> Result<Self> {
        let config = parse(config)?;
        let fallbacks = config["fallbacks"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(model)
            .collect::<Result<Vec<_>>>()?;
        let fallback_on = match config.get("fallbackOn").filter(|k| k.is_array()) {
            Some(kinds) => serde_json::from_value(kinds.clone())
                .map_err(|err| bad(format!("fallbackOn: {err}")))?,
            None => Vec::new(),
        };
        Ok(Self {
            fallbacks,
            fallback_on,
            retry: config
                .get("retry")
                .filter(|s| s.is_object())
                .map_or_else(RetryPolicy::new, retry_policy),
            max_retry_after: Duration::from_secs_f64(
                config["maxRetryAfterSecs"].as_f64().unwrap_or(60.0),
            ),
            models: Mutex::default(),
        })
    }

    fn chain(&self, spec: &Value) -> Result<ResilientModel> {
        let key = spec.to_string();
        // The guard must end before the insert below.
        let cached = lock(&self.models).get(&key).cloned();
        let primary = match cached {
            Some(model) => model,
            None => {
                // A fake model's script goes on from call to call, as an agent's does.
                let made = model(spec)?;
                lock(&self.models).insert(key, made.clone());
                made
            }
        };
        let mut chain = ResilientModel::new(primary)
            .retry(self.retry.clone())
            .max_retry_after(self.max_retry_after);
        for fallback in &self.fallbacks {
            chain = chain.fallback(fallback.clone());
        }
        for kind in &self.fallback_on {
            chain = chain.fallback_on(*kind);
        }
        Ok(chain)
    }

    /// One model call: payload `{model, input, instructions, temperature,
    /// maxTokens, output}`.
    pub async fn chat(&self, payload: String, cancel_id: u32) -> Result<String> {
        let cancel = Registered::take(cancel_id);
        let payload = parse(&payload)?;
        let chain = self.chain(&payload["model"])?;
        let request = gateway_request(&payload)?;
        let work = async {
            let response = chain.invoke(&request).await.map_err(model_failure)?;
            chat_reply(response, request.output.as_ref().map(|o| &o.schema))
        };
        Ok(or_cancelled(work, cancel.token().clone())
            .await?
            .to_string())
    }

    /// The same call, streamed: text chunks, then the reply. Returns the
    /// stream and the task that feeds it, for the binding to spawn.
    pub fn stream(&self, payload: &str) -> Result<(ChatStream, BoxFuture<'static, ()>)> {
        let payload = parse(payload)?;
        let chain = self.chain(&payload["model"])?;
        let request = gateway_request(&payload)?;
        let (chunks, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (reply, reply_receiver) = tokio::sync::oneshot::channel();
        let cancel = CancelToken::new();
        let token = cancel.clone();
        let task = async move {
            let work = async {
                // A new message id means a retry started over (design 0018 §2).
                let mut current: Option<String> = None;
                let mut sink = |delta: MessageDelta| {
                    if current.as_deref() != Some(delta.message_id.as_str()) {
                        if current.is_some() {
                            let _ = chunks.send(json!({"type": "restart", "text": ""}));
                        }
                        current = Some(delta.message_id.clone());
                    }
                    if let BlockDelta::Text { text } = delta.delta {
                        let _ = chunks.send(json!({"type": "text", "text": text}));
                    }
                };
                let response = chain
                    .stream(&request, &mut sink)
                    .await
                    .map_err(model_failure)?;
                chat_reply(response, request.output.as_ref().map(|o| &o.schema))
            };
            let _ = reply.send(or_cancelled(work, token).await);
        };
        Ok((
            ChatStream {
                chunks: Arc::new(tokio::sync::Mutex::new(receiver)),
                reply: Arc::new(tokio::sync::Mutex::new(Some(reply_receiver))),
                cancel,
            },
            Box::pin(task),
        ))
    }

    /// Embeddings: payload `{embedder, texts}`.
    pub async fn embed(&self, payload: String, cancel_id: u32) -> Result<String> {
        let cancel = Registered::take(cancel_id);
        let payload = parse(&payload)?;
        let embedder = embedder(&payload["embedder"])?;
        let texts: Vec<String> = serde_json::from_value(payload["texts"].clone())
            .map_err(|err| bad(format!("texts: {err}")))?;
        let work = async {
            let vectors = embedder.embed(&texts).await.map_err(model_failure)?;
            Ok(json!(vectors))
        };
        Ok(or_cancelled(work, cancel.token().clone())
            .await?
            .to_string())
    }
}

/// A streamed gateway call. Cheap to clone; clones share the stream.
#[derive(Debug, Clone)]
pub struct ChatStream {
    chunks: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Value>>>,
    reply: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<Result<Value>>>>>,
    cancel: CancelToken,
}

impl ChatStream {
    /// The next chunk as JSON, or `None` after the last.
    pub async fn next(&self) -> Result<Option<String>> {
        Ok(self.chunks.lock().await.recv().await.map(|c| c.to_string()))
    }

    /// Waits for the call; its reply as JSON.
    pub async fn reply(&self) -> Result<String> {
        let taken = self.reply.lock().await.take();
        let Some(receiver) = taken else {
            return Err(bad("the reply was already taken"));
        };
        let reply = receiver
            .await
            .unwrap_or_else(|_| Err(failure("cancelled", "the call was cancelled", None)))?;
        Ok(reply.to_string())
    }

    /// Cancels the call.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}
