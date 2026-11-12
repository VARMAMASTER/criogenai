//! The TypeScript SDK's engine as WebAssembly (design 0021): the same
//! JavaScript interface as the Node addon (`sdks/typescript/src/lib.rs`),
//! over the same bridge (`criogenai-js`), for the browser and every other
//! JavaScript host (Web Workers, Node, Deno, edge runtimes).
//!
//! Differences, all by necessity: storage is in memory (a database, and
//! encryption at rest, need a server: `unsupported`), telemetry export is
//! not available, and model calls go through the host's `fetch`. Tasks run
//! on the JavaScript event loop (`criogenai-rt`).

use std::sync::Arc;

use criogenai_agent::Agent;
use criogenai_core::Workflow;
use criogenai_js::{CallError, ChatStream, EventStream, Failure, Gateway, JsFn, JsFunction};
use criogenai_providers::BoxFuture;
use criogenai_rt::SendWrapper;
use criogenai_storage::Storage;
use js_sys::{Array, Function, Promise};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::{JsFuture, future_to_promise};

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console, js_name = error)]
    fn console_error(message: &str);
}

/// Once, at load: a panic ends the WebAssembly instance (`panic = abort`);
/// say why on the console first.
#[wasm_bindgen(start)]
pub fn start() {
    std::panic::set_hook(Box::new(|info| {
        console_error(&format!(
            "criogenai: the engine stopped ({info}). This is a bug; reload the page, \
             and please report it."
        ));
    }));
}

/// A failure on its way to JavaScript: an `Error` whose message is
/// `{"code", "message", "details"}`, as the Node addon throws.
fn to_js(failure: Failure) -> JsValue {
    js_sys::Error::new(&failure.to_json()).into()
}

fn promise<F>(work: F) -> Promise
where
    F: std::future::Future<Output = criogenai_js::Result<JsValue>> + 'static,
{
    future_to_promise(async move { work.await.map_err(to_js) })
}

/// What a thrown JavaScript value says.
fn reason(value: &JsValue) -> String {
    if let Some(error) = value.dyn_ref::<js_sys::Error>() {
        return String::from(error.message());
    }
    value
        .as_string()
        .unwrap_or_else(|| "a value that is not an Error".to_string())
}

/// A JavaScript function the engine calls: `(request JSON) => Promise<envelope JSON>`.
struct WebFunction(SendWrapper<Function>);

impl JsFunction for WebFunction {
    fn call(&self, request: String) -> BoxFuture<'static, Result<String, CallError>> {
        let function = SendWrapper::new((*self.0).clone());
        // JavaScript values are not `Send`; WebAssembly runs on one thread.
        Box::pin(SendWrapper::new(async move {
            let returned = function
                .call1(&JsValue::NULL, &JsValue::from_str(&request))
                .map_err(|thrown| CallError::Rejected(reason(&thrown)))?;
            let value = JsFuture::from(Promise::resolve(&returned))
                .await
                .map_err(|thrown| CallError::Rejected(reason(&thrown)))?;
            value
                .as_string()
                .ok_or_else(|| CallError::Rejected("the function did not answer with text".into()))
        }))
    }
}

fn functions(list: &Array, what: &str) -> Result<Vec<JsFn>, JsValue> {
    list.iter()
        .map(|value| {
            value
                .dyn_into::<Function>()
                .map(|f| Arc::new(WebFunction(SendWrapper::new(f))) as JsFn)
                .map_err(|_| {
                    to_js(criogenai_js::failure(
                        "invalid_value",
                        format!("every {what} must be a function"),
                        None,
                    ))
                })
        })
        .collect()
}

fn text(value: Option<String>) -> JsValue {
    value.map_or(JsValue::NULL, |s| JsValue::from_str(&s))
}

// ---------- cancellation and presets ----------

/// A new cancellation id, registered at once so an abort that comes before
/// the call starts is not lost.
#[wasm_bindgen(js_name = newCancelId)]
pub fn new_cancel_id() -> u32 {
    criogenai_js::new_cancel_id()
}

/// Cancels the call registered under `id` (no-op once it ended).
#[wasm_bindgen(js_name = cancelCall)]
pub fn cancel_call(id: u32) {
    criogenai_js::cancel_call(id);
}

/// Forgets an id whose call never started.
#[wasm_bindgen(js_name = releaseCancelId)]
pub fn release_cancel_id(id: u32) {
    criogenai_js::release_cancel_id(id);
}

/// The named providers (design 0006 §4.1) as JSON.
#[wasm_bindgen]
pub fn presets() -> String {
    criogenai_js::presets()
}

/// Registers a JavaScript model function (design 0021 §7); the handle
/// names it in model specs.
/// The engine's version (from 0.1.6 on).
#[wasm_bindgen(js_name = engineVersion)]
pub fn engine_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Problems with a value against a JSON Schema (evals, design 0024).
#[wasm_bindgen(js_name = validateJson)]
pub fn validate_json(schema: String, value: String) -> Result<String, JsValue> {
    criogenai_js::validate_json(&schema, &value).map_err(to_js)
}

/// Reaches an MCP server over HTTP (design 0022): `{handle, info, tools}`.
#[wasm_bindgen(js_name = mcpConnect)]
pub fn mcp_connect(spec: String) -> Promise {
    promise(async move { criogenai_js::mcp_connect(spec).await.map(JsValue::from) })
}

/// Lists a server's tools again.
#[wasm_bindgen(js_name = mcpRefresh)]
pub fn mcp_refresh(handle: u32) -> Promise {
    promise(async move { criogenai_js::mcp_refresh(handle).await.map(JsValue::from) })
}

/// Calls a server's tool: `{ok, text | value | content | error}`.
#[wasm_bindgen(js_name = mcpCall)]
pub fn mcp_call(handle: u32, name: String, args: String) -> Promise {
    promise(async move {
        criogenai_js::mcp_call(handle, name, args)
            .await
            .map(JsValue::from)
    })
}

/// Stops a server.
#[wasm_bindgen(js_name = mcpClose)]
pub fn mcp_close(handle: u32) -> Promise {
    promise(async move {
        criogenai_js::mcp_close(handle).await;
        Ok(JsValue::UNDEFINED)
    })
}

#[wasm_bindgen(js_name = registerFunction)]
pub fn register_function(function: Function) -> u32 {
    criogenai_js::register_function(Arc::new(WebFunction(SendWrapper::new(function))))
}

/// Forgets a model function (models built from it keep working).
#[wasm_bindgen(js_name = releaseFunction)]
pub fn release_function(handle: u32) {
    criogenai_js::release_function(handle);
}

// ---------- storage ----------

/// Storage in this tab's memory (the only kind here: a database needs a
/// server). Agents built with it share threads, memory and the audit trail.
#[wasm_bindgen]
pub struct NativeStorage {
    storage: Storage,
}

#[wasm_bindgen]
impl NativeStorage {
    /// `{audit, onBusy, busyTimeoutSecs, retention}`; a `url` or
    /// `encryptionKeys` is `unsupported`.
    #[wasm_bindgen(constructor)]
    pub fn new(spec: String) -> Result<NativeStorage, JsValue> {
        let spec = criogenai_js::parse(&spec).map_err(to_js)?;
        if spec.get("url").is_some_and(|url| !url.is_null()) {
            return Err(to_js(criogenai_js::unsupported(
                "A database (SQLite, Postgres)",
            )));
        }
        if spec
            .get("encryptionKeys")
            .is_some_and(|keys| !keys.is_null())
        {
            return Err(to_js(criogenai_js::unsupported("Encryption at rest")));
        }
        criogenai_js::configure_storage(Storage::in_memory(), &spec)
            .map(|storage| NativeStorage { storage })
            .map_err(to_js)
    }

    /// Nothing to close in memory; kept for the same interface.
    pub fn close(&self) -> Promise {
        let storage = self.storage.clone();
        promise(async move {
            storage.close().await;
            Ok(JsValue::UNDEFINED)
        })
    }

    /// A storage operation (memory, prune, audit); the result as JSON.
    pub fn op(&self, op: String, payload: String) -> Promise {
        let storage = self.storage.clone();
        promise(async move {
            let payload = criogenai_js::parse(&payload)?;
            let value = criogenai_js::storage_operation(storage, &op, payload).await?;
            Ok(JsValue::from_str(&value.to_string()))
        })
    }
}

// ---------- telemetry ----------

/// Telemetry export (Langfuse, OTLP) runs on servers: the constructor
/// throws `unsupported`. Kept so the interface matches the Node addon's.
#[wasm_bindgen]
pub struct NativeTelemetry {
    _private: (),
}

#[wasm_bindgen]
impl NativeTelemetry {
    /// Always `unsupported` here.
    #[wasm_bindgen(constructor)]
    pub fn new(_spec: String) -> Result<NativeTelemetry, JsValue> {
        Err(to_js(criogenai_js::unsupported(
            "Telemetry export (Langfuse, OTLP)",
        )))
    }

    /// Never reached (no instance exists).
    #[wasm_bindgen(getter)]
    pub fn handle(&self) -> u32 {
        0
    }

    /// Never reached.
    pub fn flush(&self) -> Promise {
        Promise::resolve(&JsValue::UNDEFINED)
    }

    /// Never reached.
    pub fn shutdown(&self) -> Promise {
        Promise::resolve(&JsValue::UNDEFINED)
    }

    /// Never reached.
    pub fn stats(&self) -> String {
        "{}".to_string()
    }
}

// ---------- agents ----------

/// An agent built from a TypeScript configuration.
#[wasm_bindgen]
pub struct NativeAgent {
    agent: Agent,
    handle: u32,
}

impl Drop for NativeAgent {
    fn drop(&mut self) {
        criogenai_js::forget_agent(self.handle);
    }
}

#[wasm_bindgen]
impl NativeAgent {
    /// Builds the agent. `tools` holds one function per tool, in the
    /// configuration's order; `hooks` one per hook.
    #[wasm_bindgen(constructor)]
    pub fn new(
        config: String,
        tools: Array,
        storage: &NativeStorage,
        hooks: Array,
    ) -> Result<NativeAgent, JsValue> {
        let config = criogenai_js::parse(&config).map_err(to_js)?;
        let telemetry = config.get("telemetry").is_some_and(|t| !t.is_null());
        let (agent, handle) = criogenai_js::build_agent(
            &config,
            functions(&tools, "tool")?,
            &storage.storage,
            functions(&hooks, "hook")?,
            |builder| {
                if telemetry {
                    return Err(criogenai_js::unsupported(
                        "Telemetry export (Langfuse, OTLP)",
                    ));
                }
                Ok(builder)
            },
        )
        .map_err(to_js)?;
        Ok(NativeAgent { agent, handle })
    }

    /// This agent's handle, for a workflow node or another agent's tool.
    #[wasm_bindgen(getter)]
    pub fn handle(&self) -> u32 {
        self.handle
    }

    /// `agent.run`: payload `{input, state}`; `cancelId` from `newCancelId`.
    pub fn run(&self, payload: String, cancel_id: u32) -> Promise {
        let agent = self.agent.clone();
        promise(async move {
            criogenai_js::agent_run(agent, payload, cancel_id)
                .await
                .map(|json| JsValue::from_str(&json))
        })
    }

    /// A thread operation: payload `{threadId, actor, …}`.
    pub fn thread(&self, op: String, payload: String, cancel_id: u32) -> Promise {
        let agent = self.agent.clone();
        promise(async move {
            criogenai_js::agent_thread(agent, op, payload, cancel_id)
                .await
                .map(|json| JsValue::from_str(&json))
        })
    }

    /// Streams a thread operation (`send`, `approve`, `reject`, `decide`,
    /// `retry`).
    pub fn stream(&self, op: String, payload: String) -> Result<NativeStream, JsValue> {
        criogenai_js::agent_stream(&self.agent, &op, &payload)
            .map(|inner| NativeStream { inner })
            .map_err(to_js)
    }
}

/// A streamed thread operation.
#[wasm_bindgen]
pub struct NativeStream {
    inner: EventStream,
}

#[wasm_bindgen]
impl NativeStream {
    /// The next event as JSON, or `null` after the last.
    pub fn next(&self) -> Promise {
        let inner = self.inner.clone();
        promise(async move { inner.next().await.map(text) })
    }

    /// Waits for the operation; its reply as JSON.
    pub fn finish(&self) -> Promise {
        let inner = self.inner.clone();
        promise(async move { inner.finish().await.map(|json| JsValue::from_str(&json)) })
    }

    /// Cancels the run (it fails with `cancelled` and is saved).
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

// ---------- the gateway (design 0018) ----------

/// Model calls without an agent, with retries and fallbacks.
#[wasm_bindgen]
pub struct NativeGateway {
    gateway: Arc<Gateway>,
}

#[wasm_bindgen]
impl NativeGateway {
    /// Config `{fallbacks, fallbackOn, retry, maxRetryAfterSecs}`.
    #[wasm_bindgen(constructor)]
    pub fn new(config: String) -> Result<NativeGateway, JsValue> {
        Gateway::new(&config)
            .map(|gateway| NativeGateway {
                gateway: Arc::new(gateway),
            })
            .map_err(to_js)
    }

    /// One model call: payload `{model, input, instructions, temperature,
    /// maxTokens, output}`.
    pub fn chat(&self, payload: String, cancel_id: u32) -> Promise {
        let gateway = self.gateway.clone();
        promise(async move {
            gateway
                .chat(payload, cancel_id)
                .await
                .map(|json| JsValue::from_str(&json))
        })
    }

    /// The same call, streamed: text chunks, then the reply.
    pub fn stream(&self, payload: String) -> Result<NativeChatStream, JsValue> {
        let (inner, task) = self.gateway.stream(&payload).map_err(to_js)?;
        // Detached: the stream's channels end it.
        drop(criogenai_rt::spawn(task));
        Ok(NativeChatStream { inner })
    }

    /// Embeddings: payload `{embedder, texts}`.
    pub fn embed(&self, payload: String, cancel_id: u32) -> Promise {
        let gateway = self.gateway.clone();
        promise(async move {
            gateway
                .embed(payload, cancel_id)
                .await
                .map(|json| JsValue::from_str(&json))
        })
    }
}

/// A streamed gateway call.
#[wasm_bindgen]
pub struct NativeChatStream {
    inner: ChatStream,
}

#[wasm_bindgen]
impl NativeChatStream {
    /// The next chunk as JSON, or `null` after the last.
    pub fn next(&self) -> Promise {
        let inner = self.inner.clone();
        promise(async move { inner.next().await.map(text) })
    }

    /// Waits for the call; its reply as JSON.
    pub fn reply(&self) -> Promise {
        let inner = self.inner.clone();
        promise(async move { inner.reply().await.map(|json| JsValue::from_str(&json)) })
    }

    /// Cancels the call.
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

// ---------- workflows (design 0015) ----------

/// A compiled workflow.
#[wasm_bindgen]
pub struct NativeWorkflow {
    workflow: Workflow,
    storage: Storage,
}

#[wasm_bindgen]
impl NativeWorkflow {
    /// Builds the graph. `nodes`: one function per name in `config.nodes`;
    /// `routers`: one per entry of `config.routes`; agents come by handle in
    /// `config.agents`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        config: String,
        nodes: Array,
        routers: Array,
        storage: &NativeStorage,
    ) -> Result<NativeWorkflow, JsValue> {
        let workflow = criogenai_js::build_workflow(
            &config,
            functions(&nodes, "node")?,
            functions(&routers, "router")?,
        )
        .map_err(to_js)?;
        Ok(NativeWorkflow {
            workflow,
            storage: storage.storage.clone(),
        })
    }

    /// Runs once: payload `{input, maxSteps, deadlineMs}`.
    pub fn run(&self, payload: String, cancel_id: u32) -> Promise {
        let workflow = self.workflow.clone();
        promise(async move {
            criogenai_js::workflow_run(workflow, payload, cancel_id)
                .await
                .map(|json| JsValue::from_str(&json))
        })
    }

    /// Continues a snapshot (no storage): payload `{snapshot, values}`.
    pub fn resume(&self, payload: String, cancel_id: u32) -> Promise {
        let workflow = self.workflow.clone();
        promise(async move {
            criogenai_js::workflow_resume(workflow, payload, cancel_id)
                .await
                .map(|json| JsValue::from_str(&json))
        })
    }

    /// A thread operation: payload `{threadId, actor, …}`.
    pub fn thread(&self, op: String, payload: String, cancel_id: u32) -> Promise {
        let (workflow, storage) = (self.workflow.clone(), self.storage.clone());
        promise(async move {
            criogenai_js::workflow_thread(workflow, storage, op, payload, cancel_id)
                .await
                .map(|json| JsValue::from_str(&json))
        })
    }
}
