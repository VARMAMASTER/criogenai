//! The `Model` trait: what the agent runtime calls (design 0006 §3).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::ModelError;
use crate::request::{Capabilities, ModelRequest, ModelResponse};

/// A boxed, `Send` future (keeps [`Model`] object-safe).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A chat model. Implemented by provider adapters, by [`FakeModel`](crate::FakeModel)
/// for tests, and by models written in Python or TypeScript.
///
/// Implementations classify failures into [`ModelError`] and never retry:
/// retries and fallbacks are applied once, by the caller (0003 §5).
pub trait Model: Send + Sync + 'static {
    /// Provider name, e.g. `anthropic`.
    fn provider(&self) -> &str;

    /// Model name, e.g. `claude-…`.
    fn model(&self) -> &str;

    /// What the model supports.
    fn capabilities(&self) -> Capabilities;

    /// Sends one request and returns the complete answer.
    fn invoke<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>>;
}

impl<M: Model + ?Sized> Model for Arc<M> {
    fn provider(&self) -> &str {
        (**self).provider()
    }

    fn model(&self) -> &str {
        (**self).model()
    }

    fn capabilities(&self) -> Capabilities {
        (**self).capabilities()
    }

    fn invoke<'a>(
        &'a self,
        request: &'a ModelRequest,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        (**self).invoke(request)
    }
}
