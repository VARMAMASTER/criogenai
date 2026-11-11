//! Configurations from TypeScript: models, embedders, retry policies,
//! limits, memory and storage.

use std::sync::Arc;
use std::time::Duration;

use criogen_agent::{Limits, Memory, OnToolException};
use criogen_core::RetryPolicy;
use criogen_providers::{
    AzureApi, AzureCredential, AzureModel, Embedder, Endpoint, FakeEmbedder, FakeModel, FakeReply,
    KaggleModel, Model, ProviderEmbedder, ProviderModel, preset,
};
use criogen_storage::{AuditQuery, Namespace, OnBusy, Retention, SearchQuery, Storage};
use serde_json::{Value, json};

use crate::{Result, bad, failure, text, to_json};

/// A model from its spec: `{"kind": "fake", replies, delayMs, …}`, a named
/// provider, `openai_compatible` / `anthropic_compatible`, `kaggle`, or
/// `function` (a JavaScript model, by handle).
pub fn model(spec: &Value) -> Result<Arc<dyn Model>> {
    let kind = text(spec, "kind").ok_or_else(|| bad("a model needs a `kind`"))?;
    if kind == "fake" {
        let delay = Duration::from_millis(spec["delayMs"].as_u64().unwrap_or(0));
        // JavaScript cannot read the request log: keep none.
        let mut fake = FakeModel::with_delay(delay, delay).without_request_log();
        if let Some(calls) = spec["failFirst"].as_u64() {
            fake = fake.flaky_first(calls as usize);
        }
        if spec["byTurn"].as_bool() == Some(true) {
            fake = fake.by_turn();
        }
        if let Some(name) = text(spec, "name") {
            fake = fake.named("fake", name);
        }
        for reply in spec["replies"].as_array().cloned().unwrap_or_default() {
            fake = fake.then(fake_reply(&reply)?);
        }
        return Ok(Arc::new(fake));
    }
    if kind == "function" {
        return crate::function_model::function_model(spec);
    }
    let name = text(spec, "model").ok_or_else(|| bad("a model needs a `model` name"))?;
    if text(spec, "kind") == Some("kaggle") {
        return kaggle_model(spec, name);
    }
    if text(spec, "kind") == Some("azure") {
        return azure_model(spec, name);
    }
    ProviderModel::new(endpoint(spec)?, name)
        .map(|m| Arc::new(m) as Arc<dyn Model>)
        .map_err(|err| failure("model_failed", err.to_string(), None))
}

/// Azure OpenAI and Foundry (design 0006 §17): `endpoint` (resource name or
/// URL, else AZURE_OPENAI_ENDPOINT); `apiKey`, `token`, `tenantId` +
/// `clientId` + `clientSecret`, or `managedIdentity`, else the environment;
/// `api` (`"responses"` or `"chat"`), `scope`; `authority` and
/// `identityEndpoint` point token requests elsewhere (tests).
fn azure_model(spec: &Value, name: &str) -> Result<Arc<dyn Model>> {
    let env = |var: &str| std::env::var(var).ok().filter(|v| !v.trim().is_empty());
    let credential = if let Some(key) = text(spec, "apiKey") {
        Some(AzureCredential::api_key(key))
    } else if let Some(token) = text(spec, "token") {
        Some(AzureCredential::token(token))
    } else if let Some(secret) = text(spec, "clientSecret") {
        let tenant = text(spec, "tenantId")
            .map(str::to_string)
            .or_else(|| env("AZURE_TENANT_ID"));
        let client = text(spec, "clientId")
            .map(str::to_string)
            .or_else(|| env("AZURE_CLIENT_ID"));
        let (Some(tenant), Some(client)) = (tenant, client) else {
            return Err(bad(
                "an Azure client secret needs `tenantId` and `clientId`",
            ));
        };
        Some(AzureCredential::client_secret(tenant, client, secret))
    } else if spec["managedIdentity"].as_bool() == Some(true) {
        Some(AzureCredential::managed_identity(
            text(spec, "clientId").map(str::to_string),
        ))
    } else {
        None
    };
    let mut model = AzureModel::from_parts(text(spec, "endpoint"), name, credential)
        .map_err(|err| failure("model_failed", err.to_string(), None))?;
    if text(spec, "api") == Some("chat") {
        model = model.with_api(AzureApi::Chat);
    }
    if let Some(scope) = text(spec, "scope") {
        model = model.with_scope(scope);
    }
    if let Some(authority) = text(spec, "authority") {
        model = model.with_authority(authority);
    }
    if let Some(url) = text(spec, "identityEndpoint") {
        model = model.with_identity_endpoint(url, text(spec, "identityHeader").map(str::to_string));
    }
    Ok(Arc::new(model))
}

/// Kaggle's Model Proxy (design 0020): `apiKey` (a Kaggle API token), or
/// `proxyUrl` and `proxyToken` (a pre-minted proxy token), else the
/// environment; `apiBase` points the token minting elsewhere (tests).
fn kaggle_model(spec: &Value, name: &str) -> Result<Arc<dyn Model>> {
    let built = match (
        text(spec, "proxyUrl"),
        text(spec, "proxyToken"),
        text(spec, "apiKey"),
    ) {
        (Some(url), Some(token), _) => KaggleModel::from_proxy(url, token, name),
        (_, _, Some(key)) => Ok(KaggleModel::new(key, name)),
        _ => KaggleModel::from_env(name),
    };
    let mut model = built.map_err(|err| failure("model_failed", err.to_string(), None))?;
    if let Some(base) = text(spec, "apiBase") {
        model = model.with_api_base(base);
    }
    Ok(Arc::new(model))
}

/// The provider endpoint of a model or embedder spec (`kind`, `apiKey`,
/// `baseUrl`).
fn endpoint(spec: &Value) -> Result<Endpoint> {
    let kind = text(spec, "kind").ok_or_else(|| bad("a model needs a `kind`"))?;
    let key = text(spec, "apiKey").map(str::to_string);
    let mut endpoint = match kind {
        "openai_compatible" => Endpoint::openai_compatible(
            text(spec, "name").unwrap_or("custom"),
            text(spec, "baseUrl").ok_or_else(|| bad("`openai_compatible` needs a `baseUrl`"))?,
            key.clone(),
        ),
        "anthropic_compatible" => Endpoint::anthropic_compatible(
            text(spec, "name").unwrap_or("custom"),
            text(spec, "baseUrl").ok_or_else(|| bad("`anthropic_compatible` needs a `baseUrl`"))?,
            key.clone(),
        ),
        // Every named provider (design 0006 §4.1): one table in criogen-providers.
        named => match preset(named) {
            Some(p) => p.endpoint(key.clone()).map_err(|err| bad(err.message))?,
            None => return Err(bad(format!("unknown model kind `{named}`"))),
        },
    };
    if let Some(base) = text(spec, "baseUrl")
        && !kind.ends_with("_compatible")
    {
        endpoint = endpoint.with_base_url(base);
    }
    if spec["allowInsecureHttp"].as_bool() == Some(true) {
        endpoint = endpoint.allow_insecure_http();
    }
    Ok(endpoint)
}

/// An embedder (design 0017 §3): a provider's `/embeddings`, or the fake
/// one for tests (`{"kind": "fake", "synonyms": [[word, sameAs]],
/// "failing": bool}`).
pub fn embedder(spec: &Value) -> Result<Arc<dyn Embedder>> {
    if text(spec, "kind") == Some("fake") {
        if spec["failing"].as_bool() == Some(true) {
            return Ok(Arc::new(FakeEmbedder::failing()));
        }
        let mut fake = FakeEmbedder::new();
        for pair in spec["synonyms"].as_array().cloned().unwrap_or_default() {
            fake = fake.synonym(
                pair[0].as_str().unwrap_or_default(),
                pair[1].as_str().unwrap_or_default(),
            );
        }
        return Ok(Arc::new(fake));
    }
    let name = text(spec, "model").ok_or_else(|| bad("an embedder needs a `model` name"))?;
    let mut embedder = ProviderEmbedder::new(endpoint(spec)?, name)
        .map_err(|err| failure("model_failed", err.to_string(), None))?;
    if let Some(n) = spec["dimensions"].as_u64() {
        embedder = embedder.dimensions(n as u32);
    }
    Ok(Arc::new(embedder))
}

fn fake_reply(reply: &Value) -> Result<FakeReply> {
    if let Some(text) = text(reply, "text") {
        return Ok(FakeReply::text(text));
    }
    if let Some(calls) = reply.get("toolCalls").and_then(Value::as_array) {
        let calls: Vec<(String, Value)> = calls
            .iter()
            .map(|c| {
                (
                    c["name"].as_str().unwrap_or_default().to_string(),
                    c["args"].clone(),
                )
            })
            .collect();
        return Ok(FakeReply::tool_calls(calls));
    }
    Err(bad("a fake reply is {text} or {toolCalls: […]}"))
}

/// Storage from a TypeScript spec: `url` (SQLite or Postgres; in memory
/// without one), `encryptionKeys`, then [`configure_storage`]'s options.
pub async fn open_storage(spec: &Value) -> Result<Storage> {
    let storage = match text(spec, "url") {
        None => Storage::in_memory(),
        Some(url) => match text(spec, "encryptionKeys") {
            #[cfg(feature = "encryption")]
            Some(keys) => {
                let keys = criogen_storage::Keyring::parse(keys).map_err(storage_error)?;
                Storage::connect_encrypted(url, keys)
                    .await
                    .map_err(storage_error)?
            }
            #[cfg(not(feature = "encryption"))]
            Some(_) => return Err(crate::unsupported("encryption at rest")),
            None => Storage::connect(url).await.map_err(storage_error)?,
        },
    };
    configure_storage(storage, spec)
}

/// The options every storage takes: `audit`, `onBusy`, `busyTimeoutSecs`,
/// `retention`.
pub fn configure_storage(mut storage: Storage, spec: &Value) -> Result<Storage> {
    if spec["audit"].as_bool() == Some(true) {
        storage = storage.with_audit();
    }
    if let Some(on_busy) = text(spec, "onBusy") {
        storage = storage.on_busy(match on_busy {
            "wait" => OnBusy::Wait,
            "reject" => OnBusy::Reject,
            "interrupt" => OnBusy::Interrupt,
            other => return Err(bad(format!("unknown onBusy `{other}`"))),
        });
    }
    if let Some(secs) = spec["busyTimeoutSecs"].as_f64() {
        storage = storage.busy_timeout(Duration::from_secs_f64(secs));
    }
    if let Some(retention) = spec.get("retention").filter(|r| r.is_object()) {
        let mut rules = Retention::new();
        if let Some(secs) = retention["threadTtlSecs"].as_f64() {
            rules = rules.thread_ttl(Duration::from_secs_f64(secs));
        }
        if let Some(n) = retention["keepCheckpoints"].as_u64() {
            rules = rules.keep_checkpoints(n as usize);
        }
        if retention["dropBranches"].as_bool() == Some(true) {
            rules = rules.drop_branches();
        }
        storage = storage.retention(rules);
    }
    Ok(storage)
}

pub(crate) fn memory(spec: &Value) -> Result<Memory> {
    let namespace = |path: &str| Namespace::parse(path).map_err(storage_error);
    let mut memory = if let Some(key) = text(spec, "byState") {
        Memory::by_state(key, namespace(text(spec, "prefix").unwrap_or(""))?)
    } else if let Some(shared) = text(spec, "shared") {
        Memory::shared(namespace(shared)?)
    } else {
        return Err(bad("memory needs `byState` or `shared`"));
    };
    if let Some(n) = spec["recall"].as_u64() {
        memory = memory.recall(n as usize);
    }
    if let Some(on) = spec["tools"].as_bool() {
        memory = memory.tools(on);
    }
    if let Some(secs) = spec["ttlSecs"].as_f64() {
        memory = memory.ttl(Duration::from_secs_f64(secs));
    }
    if let Some(spec) = spec.get("embedder").filter(|e| e.is_object()) {
        memory = memory.semantic_shared(embedder(spec)?);
    }
    Ok(memory)
}

/// A retry policy from `{maxAttempts, initialBackoffSecs, factor,
/// maxBackoffSecs, jitter}` (missing keys keep Rust's defaults).
pub(crate) fn retry_policy(spec: &Value) -> RetryPolicy {
    let defaults = RetryPolicy::default();
    let mut policy = RetryPolicy::new();
    if let Some(n) = spec["maxAttempts"].as_u64() {
        policy = policy.max_attempts(n as u32);
    }
    let initial = spec["initialBackoffSecs"].as_f64().map_or(
        Duration::from_millis(defaults.initial_backoff_ms),
        Duration::from_secs_f64,
    );
    let factor = spec["factor"].as_f64().unwrap_or(defaults.backoff_factor);
    let max = spec["maxBackoffSecs"].as_f64().map_or(
        Duration::from_millis(defaults.max_backoff_ms),
        Duration::from_secs_f64,
    );
    policy = policy.backoff(initial, factor, max);
    if let Some(jitter) = spec["jitter"].as_bool() {
        policy = policy.jitter(jitter);
    }
    policy
}

pub(crate) fn limits(spec: &Value) -> Result<Limits> {
    let mut limits = Limits::default();
    if let Some(n) = spec["modelCalls"].as_u64() {
        limits = limits.model_calls(n as u32);
    }
    if let Some(n) = spec["toolCalls"].as_u64() {
        limits = limits.tool_calls(n as u32);
    }
    if let Some(n) = spec["tokens"].as_u64() {
        limits = limits.tokens(n);
    }
    if let Some(secs) = spec["toolTimeoutSecs"].as_f64() {
        limits = limits.tool_timeout(Duration::from_secs_f64(secs));
    }
    if let Some(policy) = spec.get("onToolException").filter(|p| !p.is_null()) {
        limits = limits.on_tool_exception(match policy.as_str() {
            Some("returnToModel") => OnToolException::ReturnToModel,
            Some("returnGeneric") => OnToolException::ReturnGeneric,
            Some("fail") => OnToolException::Fail,
            _ => {
                return Err(bad(
                    "onToolException is \"returnToModel\", \"returnGeneric\" or \"fail\"",
                ));
            }
        });
    }
    Ok(limits)
}

/// A storage operation (memory, prune, audit) by name; its result as JSON.
pub async fn storage_operation(storage: Storage, op: &str, payload: Value) -> Result<Value> {
    let namespace = |key: &str| {
        Namespace::parse(text(&payload, key).unwrap_or_default()).map_err(storage_error)
    };
    let audit = || {
        storage
            .audit()
            .cloned()
            .ok_or_else(|| bad("the audit trail is off: new Storage({ audit: true })"))
    };
    if op.starts_with("collection_") {
        return crate::collections::operation(storage, op, payload).await;
    }
    match op {
        "memory_put" => {
            let ttl = payload["ttlSecs"].as_f64().map(Duration::from_secs_f64);
            storage
                .memory()
                .put(
                    &namespace("namespace")?,
                    text(&payload, "key").unwrap_or_default(),
                    payload["value"].clone(),
                    ttl,
                )
                .await
                .map_err(storage_error)?;
            Ok(Value::Null)
        }
        "memory_get" => Ok(to_json(
            &storage
                .memory()
                .get(
                    &namespace("namespace")?,
                    text(&payload, "key").unwrap_or_default(),
                )
                .await
                .map_err(storage_error)?,
        )),
        "memory_search" => {
            let mut query =
                SearchQuery::new().limit(payload["limit"].as_u64().unwrap_or(10) as usize);
            if let Some(words) = text(&payload, "text") {
                query = query.text(words);
            }
            Ok(to_json(
                &storage
                    .memory()
                    .search(&namespace("namespace")?, &query)
                    .await
                    .map_err(storage_error)?,
            ))
        }
        "memory_delete_namespace" => Ok(json!(
            storage
                .memory()
                .delete_namespace(&namespace("namespace")?)
                .await
                .map_err(storage_error)?
        )),
        "prune" => {
            let report = storage.prune().await.map_err(storage_error)?;
            Ok(json!({
                "threadsDeleted": report.threads_deleted,
                "threadsBusy": report.threads_busy,
                "checkpointsDeleted": report.checkpoints_deleted,
                "memoryItemsDeleted": report.memory_items_deleted,
            }))
        }
        "audit_query" => {
            let mut query =
                AuditQuery::new().limit(payload["limit"].as_u64().unwrap_or(100) as usize);
            if let Some(thread) = text(&payload, "threadId") {
                query = query.thread(thread);
            }
            if let Some(action) = text(&payload, "action") {
                query = query.action(action);
            }
            if let Some(after) = payload["afterSeq"].as_u64() {
                query = query.after_seq(after);
            }
            Ok(to_json(
                &audit()?.query(&query).await.map_err(storage_error)?,
            ))
        }
        "audit_verify" => Ok(to_json(&audit()?.verify().await.map_err(storage_error)?)),
        other => Err(bad(format!("unknown storage operation `{other}`"))),
    }
}

/// A storage error as a [`Failure`](crate::Failure).
pub fn storage_error(error: criogen_storage::StorageError) -> crate::Failure {
    failure(error.code(), error.message, None)
}
