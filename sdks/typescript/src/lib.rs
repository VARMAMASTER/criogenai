//! The TypeScript SDK's native addon (design 0012): a bridge from Node to
//! the Rust engine. TypeScript builds the configuration (as JSON) and holds
//! the user's tools; the agent loop, validation, storage and approvals run
//! in Rust.
//!
//! The bridge itself (configurations, envelopes, results) is `criogen-js`,
//! shared with the WebAssembly module for the browser (design 0021); this
//! file adapts it to napi: async methods return Promises on napi's Tokio
//! runtime, and JavaScript functions are called through thread-safe
//! functions, each returning a Promise of a JSON envelope.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use criogen_agent::Agent;
use criogen_core::Workflow;
use criogen_js::{CallError, ChatStream, EventStream, Failure, Gateway, JsFn, JsFunction};
use criogen_providers::BoxFuture;
use criogen_storage::Storage;
use criogen_telemetry::{Langfuse, OtlpConfig, OtlpExporter};
use napi::Status;
use napi::bindgen_prelude::{Promise, block_on, within_runtime_if_available};
use napi::threadsafe_function::ThreadsafeFunction;
use napi_derive::napi;
use serde_json::json;

/// A JavaScript tool: `(request_json) => Promise<envelope_json>`. Weak, so
/// an idle agent does not keep Node running.
type JsTool = ThreadsafeFunction<String, Promise<String>, String, Status, false, true>;

/// A JavaScript function the engine calls through napi.
struct NodeFunction(Arc<JsTool>);

impl JsFunction for NodeFunction {
    fn call(&self, request: String) -> BoxFuture<'static, Result<String, CallError>> {
        let function = self.0.clone();
        Box::pin(async move {
            let promise = function
                .call_async(request)
                .await
                .map_err(|err| CallError::Call(err.to_string()))?;
            promise.await.map_err(|err| CallError::Rejected(err.reason))
        })
    }
}

fn functions(list: Vec<JsTool>) -> Vec<JsFn> {
    list.into_iter()
        .map(|f| Arc::new(NodeFunction(Arc::new(f))) as JsFn)
        .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A failure on its way to JavaScript: the error's reason is
/// `{"code", "message", "details"}`, which the TypeScript side turns into
/// an `CriogenError`.
fn to_napi(failure: Failure) -> napi::Error {
    napi::Error::new(Status::GenericFailure, failure.to_json())
}

fn bad(message: impl Into<String>) -> napi::Error {
    to_napi(criogen_js::failure("invalid_value", message, None))
}

// ---------- cancellation and presets ----------

/// A new cancellation id, registered at once so an abort that comes before
/// the call starts is not lost.
#[napi]
pub fn new_cancel_id() -> u32 {
    criogen_js::new_cancel_id()
}

/// Cancels the call registered under `id` (no-op once it ended).
#[napi]
pub fn cancel_call(id: u32) {
    criogen_js::cancel_call(id);
}

/// The named providers (design 0006 §4.1) as JSON: `[{"name", "title",
/// "baseUrl", "keyEnv", "anthropic"}]`. The SDK's own key table is checked
/// against it in the tests.
#[napi]
pub fn presets() -> String {
    criogen_js::presets()
}

/// Forgets an id whose call never started.
#[napi]
pub fn release_cancel_id(id: u32) {
    criogen_js::release_cancel_id(id);
}

/// Registers a JavaScript model function (design 0021 §7); the handle
/// names it in model specs.
#[napi]
pub fn register_function(function: JsTool) -> u32 {
    criogen_js::register_function(Arc::new(NodeFunction(Arc::new(function))))
}

/// Forgets a model function (models built from it keep working).
#[napi]
pub fn release_function(handle: u32) {
    criogen_js::release_function(handle);
}

// ---------- storage ----------

/// Storage built from a TypeScript configuration; agents built with it
/// share it (threads, memory, leases, audit trail).
#[napi]
pub struct NativeStorage {
    storage: Storage,
}

#[napi]
impl NativeStorage {
    /// Connects (blocks briefly: SQLite opens a file, Postgres connects).
    #[napi(constructor)]
    pub fn new(spec: String) -> napi::Result<Self> {
        let spec = criogen_js::parse(&spec).map_err(to_napi)?;
        let storage = block_on(criogen_js::open_storage(&spec)).map_err(to_napi)?;
        Ok(Self { storage })
    }

    /// Closes the database's connections.
    #[napi]
    pub async fn close(&self) {
        self.storage.close().await;
    }

    /// A storage operation (memory, prune, audit); the result as JSON.
    #[napi]
    pub async fn op(&self, op: String, payload: String) -> napi::Result<String> {
        let payload = criogen_js::parse(&payload).map_err(to_napi)?;
        criogen_js::storage_operation(self.storage.clone(), &op, payload)
            .await
            .map(|v| v.to_string())
            .map_err(to_napi)
    }
}

// ---------- telemetry (design 0017 §4) ----------

/// Exporters by handle, so an agent's configuration can name one.
fn exporters() -> &'static Mutex<HashMap<u32, Arc<OtlpExporter>>> {
    static EXPORTERS: OnceLock<Mutex<HashMap<u32, Arc<OtlpExporter>>>> = OnceLock::new();
    EXPORTERS.get_or_init(Mutex::default)
}

/// A telemetry exporter: OTLP over HTTP, or Langfuse. Agents built with
/// its handle send every run to it.
#[napi]
pub struct NativeTelemetry {
    exporter: Arc<OtlpExporter>,
    handle: u32,
}

impl Drop for NativeTelemetry {
    fn drop(&mut self) {
        lock(exporters()).remove(&self.handle);
    }
}

#[napi]
impl NativeTelemetry {
    /// `{"kind": "langfuse", publicKey?, secretKey?, baseUrl?}` (missing keys
    /// from the LANGFUSE_* variables) or `{"kind": "otlp", endpoint,
    /// headers}`; both take `serviceName` and `captureContent`.
    #[napi(constructor)]
    pub fn new(spec: String) -> napi::Result<Self> {
        let spec = criogen_js::parse(&spec).map_err(to_napi)?;
        let text = |key: &str| spec.get(key).and_then(serde_json::Value::as_str);
        let mut config = match text("kind") {
            Some("langfuse") => match (text("publicKey"), text("secretKey")) {
                (Some(public), Some(secret)) => Langfuse::config(
                    public,
                    secret,
                    text("baseUrl").unwrap_or(criogen_telemetry::DEFAULT_BASE_URL),
                ),
                _ => Langfuse::from_env().map_err(|err| bad(err.to_string()))?,
            },
            Some("otlp") => {
                let mut config =
                    OtlpConfig::new(text("endpoint").ok_or_else(|| bad("otlp needs an endpoint"))?);
                for (name, value) in spec["headers"].as_object().cloned().unwrap_or_default() {
                    config = config.header(name, value.as_str().unwrap_or_default());
                }
                config
            }
            _ => return Err(bad("telemetry is \"langfuse\" or \"otlp\"")),
        };
        if let Some(name) = text("serviceName") {
            config = config.service_name(name);
        }
        if let Some(capture) = spec["captureContent"].as_bool() {
            config = config.capture_content(capture);
        }
        let exporter = within_runtime_if_available(|| OtlpExporter::start(config))
            .map_err(|err| bad(err.to_string()))?;
        let exporter = Arc::new(exporter);
        static NEXT: AtomicU32 = AtomicU32::new(1);
        let handle = NEXT.fetch_add(1, Ordering::Relaxed);
        lock(exporters()).insert(handle, exporter.clone());
        Ok(Self { exporter, handle })
    }

    /// This exporter's handle, for an agent's configuration.
    #[napi(getter)]
    pub fn handle(&self) -> u32 {
        self.handle
    }

    /// Waits until everything sent so far is exported.
    #[napi]
    pub async fn flush(&self) {
        self.exporter.flush().await;
    }

    /// Exports what is left and stops.
    #[napi]
    pub async fn shutdown(&self) {
        self.exporter.shutdown().await;
    }

    /// `{"exported", "failed", "dropped", "lastError"}`.
    #[napi]
    pub fn stats(&self) -> String {
        let stats = self.exporter.stats();
        json!({
            "exported": stats.exported,
            "failed": stats.failed,
            "dropped": stats.dropped_events,
            "lastError": self.exporter.last_error(),
        })
        .to_string()
    }
}

// ---------- agents ----------

/// An agent built from a TypeScript configuration.
#[napi]
pub struct NativeAgent {
    agent: Agent,
    handle: u32,
}

impl Drop for NativeAgent {
    fn drop(&mut self) {
        criogen_js::forget_agent(self.handle);
    }
}

#[napi]
impl NativeAgent {
    /// Builds the agent. `tools` holds one function per tool, in the
    /// configuration's order.
    #[napi(constructor)]
    pub fn new(
        config: String,
        tools: Vec<JsTool>,
        storage: &NativeStorage,
        hooks: Vec<JsTool>,
    ) -> napi::Result<Self> {
        let config = criogen_js::parse(&config).map_err(to_napi)?;
        let telemetry = config["telemetry"].as_u64();
        let (agent, handle) = criogen_js::build_agent(
            &config,
            functions(tools),
            &storage.storage,
            functions(hooks),
            // Every run of this agent goes to the exporter (design 0017 §4).
            |builder| match telemetry {
                Some(handle) => {
                    let exporter = lock(exporters())
                        .get(&(handle as u32))
                        .cloned()
                        .ok_or_else(|| {
                            criogen_js::failure(
                                "invalid_value",
                                "the telemetry exporter is gone",
                                None,
                            )
                        })?;
                    Ok(builder.observe(exporter.observer()))
                }
                None => Ok(builder),
            },
        )
        .map_err(to_napi)?;
        Ok(Self { agent, handle })
    }

    /// This agent's handle, for a workflow node.
    #[napi(getter)]
    pub fn handle(&self) -> u32 {
        self.handle
    }

    /// `agent.run`: payload `{input, state}`; `cancelId` from
    /// [`new_cancel_id`].
    #[napi]
    pub async fn run(&self, payload: String, cancel_id: u32) -> napi::Result<String> {
        criogen_js::agent_run(self.agent.clone(), payload, cancel_id)
            .await
            .map_err(to_napi)
    }

    /// A thread operation: payload `{threadId, actor, …}`.
    #[napi]
    pub async fn thread(
        &self,
        op: String,
        payload: String,
        cancel_id: u32,
    ) -> napi::Result<String> {
        criogen_js::agent_thread(self.agent.clone(), op, payload, cancel_id)
            .await
            .map_err(to_napi)
    }

    /// Streams a thread operation (`send`, `approve`, `reject`, `decide`,
    /// `retry`).
    #[napi]
    pub fn stream(&self, op: String, payload: String) -> napi::Result<NativeStream> {
        within_runtime_if_available(|| criogen_js::agent_stream(&self.agent, &op, &payload))
            .map(|inner| NativeStream { inner })
            .map_err(to_napi)
    }
}

/// A streamed thread operation.
#[napi]
pub struct NativeStream {
    inner: EventStream,
}

#[napi]
impl NativeStream {
    /// The next event as JSON, or `null` after the last.
    #[napi]
    pub async fn next(&self) -> napi::Result<Option<String>> {
        self.inner.next().await.map_err(to_napi)
    }

    /// Waits for the operation; its reply as JSON.
    #[napi]
    pub async fn finish(&self) -> napi::Result<String> {
        self.inner.finish().await.map_err(to_napi)
    }

    /// Cancels the run (it fails with `cancelled` and is saved).
    #[napi]
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

// ---------- MCP servers (design 0022) ----------

/// Starts or reaches an MCP server: `{handle, info, tools}`.
#[napi]
pub async fn mcp_connect(spec: String) -> napi::Result<String> {
    criogen_js::mcp_connect(spec).await.map_err(to_napi)
}

/// Lists a server's tools again.
#[napi]
pub async fn mcp_refresh(handle: u32) -> napi::Result<String> {
    criogen_js::mcp_refresh(handle).await.map_err(to_napi)
}

/// Calls a server's tool: `{ok, text | value | content | error}`.
#[napi]
pub async fn mcp_call(handle: u32, name: String, args: String) -> napi::Result<String> {
    criogen_js::mcp_call(handle, name, args)
        .await
        .map_err(to_napi)
}

/// Stops a server.
#[napi]
pub async fn mcp_close(handle: u32) {
    criogen_js::mcp_close(handle).await;
}

// ---------- the gateway (design 0018) ----------

/// Model calls without an agent: the agent's retries and fallbacks
/// (`ResilientModel`) around the model each call names.
#[napi]
pub struct NativeGateway {
    gateway: Arc<Gateway>,
}

#[napi]
impl NativeGateway {
    /// Config `{fallbacks, fallbackOn, retry, maxRetryAfterSecs}`.
    #[napi(constructor)]
    pub fn new(config: String) -> napi::Result<Self> {
        Gateway::new(&config)
            .map(|gateway| Self {
                gateway: Arc::new(gateway),
            })
            .map_err(to_napi)
    }

    /// One model call: payload `{model, input, instructions, temperature,
    /// maxTokens, output}`.
    #[napi]
    pub async fn chat(&self, payload: String, cancel_id: u32) -> napi::Result<String> {
        self.gateway.chat(payload, cancel_id).await.map_err(to_napi)
    }

    /// The same call, streamed: text chunks, then the reply.
    #[napi]
    pub fn stream(&self, payload: String) -> napi::Result<NativeChatStream> {
        let (inner, task) = self.gateway.stream(&payload).map_err(to_napi)?;
        napi::bindgen_prelude::spawn(task);
        Ok(NativeChatStream { inner })
    }

    /// Embeddings: payload `{embedder, texts}`.
    #[napi]
    pub async fn embed(&self, payload: String, cancel_id: u32) -> napi::Result<String> {
        self.gateway
            .embed(payload, cancel_id)
            .await
            .map_err(to_napi)
    }
}

/// A streamed gateway call.
#[napi]
pub struct NativeChatStream {
    inner: ChatStream,
}

#[napi]
impl NativeChatStream {
    /// The next chunk as JSON, or `null` after the last.
    #[napi]
    pub async fn next(&self) -> napi::Result<Option<String>> {
        self.inner.next().await.map_err(to_napi)
    }

    /// Waits for the call; its reply as JSON.
    #[napi]
    pub async fn reply(&self) -> napi::Result<String> {
        self.inner.reply().await.map_err(to_napi)
    }

    /// Cancels the call.
    #[napi]
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

// ---------- workflows (design 0015) ----------

/// A compiled workflow (design 0015).
#[napi]
pub struct NativeWorkflow {
    workflow: Workflow,
    storage: Storage,
}

#[napi]
impl NativeWorkflow {
    /// Builds the graph. `nodes`: one function per name in `config.nodes`;
    /// `routers`: one per entry of `config.routes`; agents come by handle in
    /// `config.agents`.
    #[napi(constructor)]
    pub fn new(
        config: String,
        nodes: Vec<JsTool>,
        routers: Vec<JsTool>,
        storage: &NativeStorage,
    ) -> napi::Result<Self> {
        let workflow = criogen_js::build_workflow(&config, functions(nodes), functions(routers))
            .map_err(to_napi)?;
        Ok(Self {
            workflow,
            storage: storage.storage.clone(),
        })
    }

    /// Runs once: payload `{input, maxSteps, deadlineMs}`.
    #[napi]
    pub async fn run(&self, payload: String, cancel_id: u32) -> napi::Result<String> {
        criogen_js::workflow_run(self.workflow.clone(), payload, cancel_id)
            .await
            .map_err(to_napi)
    }

    /// Continues a snapshot (no storage): payload `{snapshot, values}`.
    #[napi]
    pub async fn resume(&self, payload: String, cancel_id: u32) -> napi::Result<String> {
        criogen_js::workflow_resume(self.workflow.clone(), payload, cancel_id)
            .await
            .map_err(to_napi)
    }

    /// A thread operation: payload `{threadId, actor, …}`.
    #[napi]
    pub async fn thread(
        &self,
        op: String,
        payload: String,
        cancel_id: u32,
    ) -> napi::Result<String> {
        criogen_js::workflow_thread(
            self.workflow.clone(),
            self.storage.clone(),
            op,
            payload,
            cancel_id,
        )
        .await
        .map_err(to_napi)
    }
}
