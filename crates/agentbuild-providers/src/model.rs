//! The `Model` trait: what the agent runtime calls (design 0006 §3).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::error::ModelError;
use crate::message::ContentBlock;
use crate::request::{Capabilities, ModelRequest, ModelResponse};
use crate::stream::{BlockDelta, MessageDelta};

/// Receives `message_delta` events while a response streams.
pub type DeltaSink<'a> = &'a mut (dyn FnMut(MessageDelta) + Send);

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

    /// Sends one request, passing each [`MessageDelta`] to `on_delta` as it
    /// arrives, and returns the complete answer (the same message the deltas
    /// built).
    ///
    /// The default calls [`invoke`](Self::invoke) and reports each text
    /// block as one delta, so every model can be used where streaming is
    /// expected.
    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        Box::pin(async move {
            let response = self.invoke(request).await?;
            for (index, block) in response.message.content.iter().enumerate() {
                if let ContentBlock::Text { text, .. } = block {
                    on_delta(MessageDelta {
                        message_id: response.message.id.clone(),
                        index: u32::try_from(index).unwrap_or(u32::MAX),
                        delta: BlockDelta::Text { text: text.clone() },
                    });
                }
            }
            Ok(response)
        })
    }
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

    fn stream<'a>(
        &'a self,
        request: &'a ModelRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<ModelResponse, ModelError>> {
        (**self).stream(request, on_delta)
    }
}
