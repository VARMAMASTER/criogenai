//! The bridge between Criogen's engine and JavaScript (designs 0012, 0021),
//! shared by the TypeScript SDK's two engines: the Node addon (napi-rs) and
//! the WebAssembly module (wasm-bindgen) for the browser and other
//! JavaScript hosts.
//!
//! TypeScript builds every configuration as JSON and holds the user's
//! tools, hooks, nodes and routers, which reach Rust as [`JsFunction`]s:
//! request JSON in, a Promise of an envelope JSON out. This crate reads the
//! configurations, builds agents, workflows and the gateway, runs them, and
//! turns results and failures back into JSON. The bindings only adapt the
//! calling conventions (napi Promises, wasm-bindgen Promises), so the two
//! engines behave the same, and one TypeScript test suite checks both.

use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use criogen_core::CancelToken;
use criogen_providers::BoxFuture;
use serde_json::{Value, json};

mod agent;
mod config;
mod function_model;
mod gateway;
mod mcp;
mod workflow;

pub use agent::{EventStream, agent_run, agent_stream, agent_thread, build_agent, forget_agent};
pub use config::{
    configure_storage, embedder, model, open_storage, storage_error, storage_operation,
};
pub use function_model::{register_function, release_function};
pub use gateway::{ChatStream, Gateway};
pub use mcp::{mcp_call, mcp_close, mcp_connect, mcp_refresh};
pub use workflow::{build_workflow, workflow_resume, workflow_run, workflow_thread};

// ---------- failures ----------

/// A failure on its way to JavaScript. The bindings reject with an `Error`
/// whose message is [`Failure::to_json`]; the TypeScript side turns that
/// into a `CriogenError` of the code's class.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// The stable error code (`schemas/error-codes.json`).
    pub code: String,
    /// What went wrong, for people.
    pub message: String,
    /// Structured details (a snapshot, the node, a cause key), if any.
    pub details: Option<Value>,
}

impl Failure {
    /// `{"code", "message", "details"}`, the text the TypeScript side reads.
    pub fn to_json(&self) -> String {
        json!({"code": self.code, "message": self.message, "details": self.details}).to_string()
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Failure {}

/// A bridge result.
pub type Result<T> = std::result::Result<T, Failure>;

/// A failure with `code`.
pub fn failure(code: &str, message: impl Into<String>, details: Option<Value>) -> Failure {
    Failure {
        code: code.to_string(),
        message: message.into(),
        details,
    }
}

pub(crate) fn bad(message: impl Into<String>) -> Failure {
    failure("invalid_value", message, None)
}

/// `unsupported`: `what` needs a server (a database, encryption at rest,
/// telemetry export), and the engine runs in a JavaScript host.
pub fn unsupported(what: &str) -> Failure {
    failure(
        "unsupported",
        format!(
            "{what} runs on servers, not in the browser (here the engine is WebAssembly); \
             use the Node package on a server for it"
        ),
        None,
    )
}

/// Reads JSON text from JavaScript.
pub fn parse(text: &str) -> Result<Value> {
    serde_json::from_str(text).map_err(|e| bad(format!("unreadable JSON: {e}")))
}

pub(crate) fn to_json<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

pub(crate) fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

pub(crate) fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect()
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------- JavaScript functions ----------

/// Why calling a JavaScript function failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// It could not be called (the JavaScript side is gone).
    Call(String),
    /// It threw, or its Promise rejected: the reason.
    Rejected(String),
}

/// A JavaScript function as the engine calls it: request JSON in, a Promise
/// of an envelope JSON out (a tool, a hook, a workflow node or router).
pub trait JsFunction: Send + Sync + 'static {
    /// Calls the function and waits for its answer.
    fn call(&self, request: String) -> BoxFuture<'static, std::result::Result<String, CallError>>;
}

/// A shared [`JsFunction`].
pub type JsFn = Arc<dyn JsFunction>;

/// Calls a JavaScript node, router or hook: request JSON in, envelope out.
pub(crate) async fn call_js(function: JsFn, request: String) -> std::result::Result<Value, String> {
    let envelope = function.call(request).await.map_err(|err| match err {
        CallError::Call(err) => format!("could not call the function: {err}"),
        CallError::Rejected(reason) => format!("the function failed: {reason}"),
    })?;
    serde_json::from_str(&envelope).map_err(|err| format!("unreadable answer: {err}"))
}

// ---------- cancellation ----------

/// Cancel tokens of calls in flight, by id (JavaScript's `AbortSignal`
/// fires them with [`cancel_call`]).
fn tokens() -> &'static Mutex<HashMap<u32, CancelToken>> {
    static TOKENS: OnceLock<Mutex<HashMap<u32, CancelToken>>> = OnceLock::new();
    TOKENS.get_or_init(Mutex::default)
}

/// A registered token; unregistered when dropped.
#[derive(Debug)]
pub struct Registered(u32, CancelToken);

impl Registered {
    /// The token registered under `id` (a new one if the id is unknown).
    pub fn take(id: u32) -> Self {
        let token = lock(tokens()).entry(id).or_default().clone();
        Self(id, token)
    }

    /// The call's cancel token.
    pub fn token(&self) -> &CancelToken {
        &self.1
    }
}

impl Drop for Registered {
    fn drop(&mut self) {
        lock(tokens()).remove(&self.0);
    }
}

/// A new cancellation id, registered at once so an abort that comes before
/// the call starts is not lost.
pub fn new_cancel_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    lock(tokens()).insert(id, CancelToken::new());
    id
}

/// Cancels the call registered under `id` (no-op once it ended).
pub fn cancel_call(id: u32) {
    if let Some(token) = lock(tokens()).get(&id) {
        token.cancel();
    }
}

/// Forgets an id whose call never started.
pub fn release_cancel_id(id: u32) {
    lock(tokens()).remove(&id);
}

/// The named providers (design 0006 §4.1) as JSON: `[{"name", "title",
/// "baseUrl", "keyEnv", "anthropic"}]`. The SDK's own key table is checked
/// against it in the tests.
pub fn presets() -> String {
    let all: Vec<Value> = criogen_providers::PRESETS
        .iter()
        .map(|p| {
            json!({"name": p.name, "title": p.title, "baseUrl": p.base_url,
                "keyEnv": p.key_env, "anthropic": p.anthropic})
        })
        .collect();
    Value::Array(all).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_crosses_as_the_json_the_sdk_reads() {
        let f = failure("tool_failed", "boom", Some(json!({"call_id": "c1"})));
        let back: Value = serde_json::from_str(&f.to_json()).unwrap();
        assert_eq!(
            back,
            json!({"code": "tool_failed", "message": "boom", "details": {"call_id": "c1"}})
        );
        assert_eq!(f.to_string(), "tool_failed: boom");
    }

    #[test]
    fn an_abort_before_the_call_starts_is_not_lost() {
        let id = new_cancel_id();
        cancel_call(id);
        let registered = Registered::take(id);
        assert!(registered.token().is_cancelled());
        drop(registered);
        // Unregistered: a later id is a fresh token.
        assert!(!Registered::take(new_cancel_id()).token().is_cancelled());
    }

    #[test]
    fn unsupported_names_what_needs_a_server() {
        let f = unsupported("SQLite storage");
        assert_eq!(f.code, "unsupported");
        assert!(f.message.starts_with("SQLite storage runs on servers"));
    }
}
