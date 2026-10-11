//! Model resilience: retries that honor `Retry-After`, and fallback models
//! (design 0003 §5, 0006 §6).
//!
//! Order (0003 §5.3): hooks → **fallback chain → retry loop** → adapter.

use std::sync::Arc;
use std::time::Duration;

use agentbuild_core::{EventSink, RetryPolicy};
use agentbuild_providers::{
    Dispatch, MessageDelta, Model, ModelError, ModelErrorKind, ModelRequest, ModelResponse,
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
        on_delta: &Arc<dyn Fn(MessageDelta) + Send + Sync>,
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
                let sink = on_delta.clone();
                let mut forward = move |delta: MessageDelta| sink(delta);
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
