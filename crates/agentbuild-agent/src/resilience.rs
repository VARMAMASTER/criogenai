//! Model resilience: retries that honor `Retry-After`, and fallback models
//! (design 0003 §5, 0006 §6).
//!
//! Order (0003 §5.3): hooks → **fallback chain → retry loop** → adapter.
//! [`ResilientModel`] is the same chain on its own, for calling a model
//! without an agent (design 0018 §3).

use std::sync::Arc;
use std::time::Duration;

use agentbuild_core::{EventSink, RetryPolicy};
use agentbuild_providers::{
    BoxFuture, Capabilities, DeltaSink, Dispatch, MessageDelta, Model, ModelError, ModelErrorKind,
    ModelRequest, ModelResponse,
};
use serde_json::json;
use tokio::time::Instant;

/// How model calls are retried and where they fall back to.
pub(crate) struct ModelChain {
    /// Primary first, then fallbacks in order.
    pub models: Vec<Arc<dyn Model>>,
    pub retry: RetryPolicy,
    /// Non-transient kinds that also move to the next model (e.g.
    /// `context_overflow` → a larger model).
    pub fallback_on: Vec<ModelErrorKind>,
    /// Longer `Retry-After` waits are not waited for.
    pub max_retry_after: Duration,
}

/// Per-call environment.
#[derive(Clone)]
pub(crate) struct CallEnv {
    pub events: EventSink,
    /// When the run's deadline passes, if it has one.
    pub deadline: Option<Instant>,
    /// Makes the backoff jitter reproducible (the task id).
    pub seed: String,
}

/// Why the chain gave up.
#[derive(Debug)]
pub(crate) enum ChainError {
    /// The last model's error.
    Model(Box<ModelError>),
    /// The next wait would cross the run's deadline (0003 E6).
    Deadline(Box<ModelError>),
}

impl ModelChain {
    pub(crate) async fn call(
        &self,
        request: &ModelRequest,
        env: &CallEnv,
        on_delta: &(dyn Fn(MessageDelta) + Send + Sync),
    ) -> Result<ModelResponse, ChainError> {
        let mut last = None;
        for (index, model) in self.models.iter().enumerate() {
            let has_fallback = index + 1 < self.models.len();
            // Only fallbacks need a changed copy of the request.
            let changed;
            let request = if index == 0 {
                request
            } else {
                changed = self.request_for(request, index);
                &changed
            };
            let mut attempt = 1;
            let error = loop {
                let mut forward = |delta: MessageDelta| on_delta(delta);
                let error = match model.stream(request, &mut forward).await {
                    Ok(response) => return Ok(response),
                    Err(error) => error,
                };
                if error.emitted_deltas {
                    // The retry starts a new message: UIs drop the partial one.
                    env.events
                        .emit("message_aborted", json!({"reason": error.kind}));
                }
                let transient = error.kind.is_transient();
                // With somewhere to go, don't sleep on a busy provider.
                let switch_now = has_fallback
                    && matches!(
                        error.kind,
                        ModelErrorKind::RateLimited | ModelErrorKind::Overloaded
                    );
                let wait = if !transient || switch_now || attempt >= self.retry.max_attempts {
                    None
                } else if error.dispatch == Dispatch::NotSent {
                    Some(Duration::ZERO) // never reached the provider: retry at once
                } else {
                    match error.retry_after() {
                        Some(asked) if asked > self.max_retry_after => None,
                        Some(asked) => Some(asked),
                        None => Some(self.retry.delay(attempt, &env.seed)),
                    }
                };
                env.events.emit(
                    "model_attempt_failed",
                    json!({
                        "provider": model.provider(),
                        "model": model.model(),
                        "attempt": attempt,
                        "kind": error.kind,
                        "dispatch": error.dispatch,
                        "retry_in_ms": wait.map(|d| d.as_millis() as u64),
                        "message": error.message,
                    }),
                );
                let Some(wait) = wait else { break error };
                if env
                    .deadline
                    .is_some_and(|deadline| Instant::now() + wait >= deadline)
                {
                    return Err(ChainError::Deadline(Box::new(error)));
                }
                tokio::time::sleep(wait).await;
                attempt += 1;
            };
            let falls_back = has_fallback
                && (error.kind.is_transient() || self.fallback_on.contains(&error.kind));
            if !falls_back {
                return Err(ChainError::Model(Box::new(error)));
            }
            let next = &self.models[index + 1];
            env.events.emit(
                "model_fallback",
                json!({
                    "from": format!("{}:{}", model.provider(), model.model()),
                    "to": format!("{}:{}", next.provider(), next.model()),
                    "kind": error.kind,
                }),
            );
            last = Some(error);
        }
        Err(ChainError::Model(Box::new(
            last.expect("at least one model"),
        )))
    }

    /// The request for model `index`: provider-specific fields of earlier
    /// models in the chain are removed before a fallback (0003 §5.3).
    fn request_for(&self, request: &ModelRequest, index: usize) -> ModelRequest {
        let mut request = request.clone();
        if index > 0 {
            let current = self.models[index].provider();
            for earlier in &self.models[..index] {
                if earlier.provider() != current {
                    request.extras.remove(earlier.provider());
                }
            }
        }
        request
    }
}

/// A model with retries and fallback models: the chain an agent puts
/// around its model calls (design 0003 §5), for calling a model directly
/// (design 0018 §3). It is a [`Model`] itself.
///
/// Transient errors are retried with backoff (honoring `Retry-After` up to
/// [`max_retry_after`](Self::max_retry_after); a request that never reached
/// the provider is retried at once); then the next fallback is tried, also
/// for the kinds given to [`fallback_on`](Self::fallback_on). A busy
/// provider (rate limited, overloaded) moves to the fallback without
/// waiting. Each attempt of [`stream`](Model::stream) builds a new message,
/// with a new message id.
///
/// ```
/// use std::sync::Arc;
/// use agentbuild_agent::ResilientModel;
/// use agentbuild_core::RetryPolicy;
/// use agentbuild_providers::{FakeModel, FakeReply, Message, Model, ModelRequest};
///
/// # tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap().block_on(async {
/// let flaky = FakeModel::new().flaky_first(1).then(FakeReply::text("hello"));
/// let model = ResilientModel::new(Arc::new(flaky)).retry(RetryPolicy::new().max_attempts(2));
/// let response = model
///     .invoke(&ModelRequest::new(vec![Message::user("hi")]))
///     .await
///     .unwrap();
/// assert_eq!(response.message.text(), "hello");
/// # });
/// ```
pub struct ResilientModel {
    chain: ModelChain,
}

impl std::fmt::Debug for ResilientModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let models: Vec<String> = self
            .chain
            .models
            .iter()
            .map(|m| format!("{}:{}", m.provider(), m.model()))
            .collect();
        f.debug_struct("ResilientModel")
            .field("models", &models)
            .field("retry", &self.chain.retry)
            .field("fallback_on", &self.chain.fallback_on)
            .field("max_retry_after", &self.chain.max_retry_after)
            .finish()
    }
}

impl ResilientModel {
    /// `model` with the agent's defaults: 3 attempts from 500 ms backoff,
    /// no fallbacks, `Retry-After` honored up to 60 s.
    pub fn new(model: Arc<dyn Model>) -> Self {
        Self {
            chain: ModelChain {
                models: vec![model],
                retry: RetryPolicy::new(),
                fallback_on: Vec::new(),
                max_retry_after: Duration::from_secs(60),
            },
        }
    }

    /// Adds a fallback model, tried after the ones before it.
    pub fn fallback(mut self, model: Arc<dyn Model>) -> Self {
        self.chain.models.push(model);
        self
    }

    /// How each model's transient errors are retried.
    pub fn retry(mut self, retry: RetryPolicy) -> Self {
        self.chain.retry = retry;
        self
    }

    /// A non-transient error kind that also moves to the next model (e.g.
    /// `ContextOverflow` to a model with a larger context).
    pub fn fallback_on(mut self, kind: ModelErrorKind) -> Self {
        self.chain.fallback_on.push(kind);
        self
    }

    /// The longest `Retry-After` that is waited for; a longer one ends the
    /// retries of that model.
    pub fn max_retry_after(mut self, wait: Duration) -> Self {
        self.chain.max_retry_after = wait;
        self
    }

    // The `Model` trait's own error type; boxing it here would only unbox it again.
    #[allow(clippy::result_large_err)]
    async fn call(
        &self,
        request: &ModelRequest,
        on_delta: &(dyn Fn(MessageDelta) + Send + Sync),
    ) -> Result<ModelResponse, ModelError> {
        let env = CallEnv {
            events: EventSink::default(),
            deadline: None,
            // Backoff jitter differs per conversation.
            seed: request
                .messages
                .last()
                .map(|m| m.id.clone())
                .unwrap_or_default(),
        };
        match self.chain.call(request, &env, on_delta).await {
            Ok(response) => Ok(response),
            Err(ChainError::Model(error) | ChainError::Deadline(error)) => Err(*error),
        }
    }
}

impl Model for ResilientModel {
    /// The primary model's provider.
    fn provider(&self) -> &str {
        self.chain.models[0].provider()
    }

    /// The primary model's name (the message metadata names the model that
    /// answered).
    fn model(&self) -> &str {
        self.chain.models[0].model()
    }

    fn capabilities(&self) -> Capabilities {
        self.chain.models[0].capabilities()
    }

    fn invoke<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move { self.call(request, &|_| {}).await })
    }

    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move {
            let sink = std::sync::Mutex::new(on_delta);
            let forward = |delta: MessageDelta| {
                let mut sink = sink.lock().unwrap_or_else(|e| e.into_inner());
                (*sink)(delta);
            };
            self.call(request, &forward).await
        })
    }
}
