//! Observability for CriogenAI: export runs as OpenTelemetry traces, with a
//! preset for [Langfuse](https://langfuse.com) (design 0009).
//!
//! Every run becomes a trace (the trace id is the run id, so suspended and
//! resumed segments share it): workflow nodes are spans, model calls are
//! *generations* (model, input, output, token usage), tool calls are *tool*
//! observations, and failures are marked as errors.
//!
//! Exporting never slows or breaks a run: the observer only queues events;
//! a background task batches and sends them over OTLP/HTTP (JSON). With
//! [`OtlpConfig::capture_content`] off, or a [`OtlpConfig::mask`],
//! sensitive content never leaves the process.
//!
//! ```no_run
//! use criogenai_core::RunOptions;
//! use criogenai_telemetry::{Langfuse, OtlpExporter};
//!
//! # async fn demo() -> Result<(), criogenai_telemetry::TelemetryError> {
//! // LANGFUSE_PUBLIC_KEY, LANGFUSE_SECRET_KEY, LANGFUSE_BASE_URL
//! let langfuse = OtlpExporter::start(Langfuse::from_env()?)?;
//! let options = RunOptions::default()
//!     .observe(langfuse.observer())
//!     .metadata(serde_json::json!({"user_id": "u-42", "session_id": "chat-7"}));
//! // workflow.run_with(input, options) or agent.run_with(input, options)
//! langfuse.flush().await;
//! # Ok(()) }
//! ```

// Compiles the README's examples with the doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

mod exporter;
mod langfuse;
mod otlp;
mod spans;

pub use exporter::{ExportStats, OtlpConfig, OtlpExporter, TelemetryError};
pub use langfuse::{DEFAULT_BASE_URL, Langfuse};
pub use spans::MaskFn;
