//! The OTLP exporter: a non-blocking observer, a background task that
//! batches spans, and HTTP export with retries (design 0009 §4).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use criogenai_core::{Event, EventObserver};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

use crate::otlp;
use crate::spans::{ContentPolicy, MaskFn, Span, SpanBuilder};

/// Where and how to export. Build it directly, or with
/// [`Langfuse`](crate::Langfuse).
#[derive(Clone)]
#[non_exhaustive]
pub struct OtlpConfig {
    /// Full traces URL, e.g. `http://localhost:4318/v1/traces`.
    pub endpoint: String,
    /// Extra headers (authentication).
    pub headers: Vec<(String, String)>,
    /// `service.name` of the exported resource. Default `criogenai`.
    pub service_name: String,
    /// Export messages, tool arguments and results, error messages, and the
    /// run's `user_id`, `session_id` and custom metadata. Default `false`:
    /// names, timings, usage, status and error **codes** still go. Turn it
    /// on for data that may leave your network, and set a [`mask`](Self::mask)
    /// for what must not.
    pub capture_content: bool,
    /// Applied to every captured payload before export (e.g. to remove
    /// patient identifiers).
    pub mask: Option<MaskFn>,
    /// Longer strings are cut, with a marker. Default 100,000 characters.
    pub max_attribute_chars: usize,
    /// Events waiting for the background task; when full, new ones are
    /// dropped (never blocking a run). Default 10,000.
    pub queue_capacity: usize,
    /// Spans per request. Default 200.
    pub batch_size: usize,
    /// Maximum wait before a partial batch is sent. Default 1 s.
    pub flush_interval: Duration,
    /// Time limit per export request. Default 10 s.
    pub request_timeout: Duration,
}

impl std::fmt::Debug for OtlpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Headers carry credentials: show names only.
        f.debug_struct("OtlpConfig")
            .field("endpoint", &self.endpoint)
            .field(
                "headers",
                &self.headers.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            )
            .field("service_name", &self.service_name)
            .field("capture_content", &self.capture_content)
            .field("mask", &self.mask.is_some())
            .finish_non_exhaustive()
    }
}

impl OtlpConfig {
    /// Export to `endpoint` (the full `/v1/traces` URL) with defaults.
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            headers: Vec::new(),
            service_name: "criogenai".into(),
            capture_content: false,
            mask: None,
            max_attribute_chars: 100_000,
            queue_capacity: 10_000,
            batch_size: 200,
            flush_interval: Duration::from_secs(1),
            request_timeout: Duration::from_secs(10),
        }
    }

    /// Export to the local trace viewer (`criogenai-trace`, design 0023) at
    /// `http://127.0.0.1:4318/v1/traces`. Content stays off unless
    /// [`capture_content`](Self::capture_content) is set.
    pub fn local() -> Self {
        Self::new("http://127.0.0.1:4318/v1/traces")
    }

    /// Adds a header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// Sets [`service_name`](Self::service_name).
    pub fn service_name(mut self, name: impl Into<String>) -> Self {
        self.service_name = name.into();
        self
    }

    /// Sets [`capture_content`](Self::capture_content).
    pub fn capture_content(mut self, capture: bool) -> Self {
        self.capture_content = capture;
        self
    }

    /// Sets [`mask`](Self::mask).
    pub fn mask(mut self, mask: impl Fn(Value) -> Value + Send + Sync + 'static) -> Self {
        self.mask = Some(Arc::new(mask));
        self
    }

    /// Sets [`batch_size`](Self::batch_size) and
    /// [`flush_interval`](Self::flush_interval).
    pub fn batching(mut self, batch_size: usize, flush_interval: Duration) -> Self {
        self.batch_size = batch_size.max(1);
        self.flush_interval = flush_interval;
        self
    }

    /// Sets [`queue_capacity`](Self::queue_capacity).
    pub fn queue_capacity(mut self, capacity: usize) -> Self {
        self.queue_capacity = capacity.max(1);
        self
    }
}

/// Why an exporter could not start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TelemetryError {
    /// A required setting is missing (e.g. an environment variable).
    #[error("missing setting: {0}")]
    Missing(String),
    /// Not called inside a Tokio runtime.
    #[error("the exporter must be started inside a Tokio runtime")]
    NoRuntime,
    /// The HTTP client could not be built.
    #[error("could not create the HTTP client: {0}")]
    Client(String),
}

/// Export counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExportStats {
    /// Spans the backend accepted.
    pub exported: u64,
    /// Events dropped because the queue was full.
    pub dropped_events: u64,
    /// Spans lost after failed export attempts.
    pub failed: u64,
}

#[derive(Default)]
struct Counters {
    exported: AtomicU64,
    dropped_events: AtomicU64,
    failed: AtomicU64,
    last_error: Mutex<Option<String>>,
}

enum Command {
    Event(Box<Event>),
    Flush(oneshot::Sender<()>),
    Shutdown(oneshot::Sender<()>),
}

/// Exports runs as OpenTelemetry traces. Attach it to runs with
/// [`observer`](Self::observer).
///
/// ```no_run
/// use criogenai_core::RunOptions;
/// use criogenai_telemetry::{Langfuse, OtlpExporter};
///
/// # async fn demo() -> Result<(), criogenai_telemetry::TelemetryError> {
/// let langfuse = OtlpExporter::start(Langfuse::from_env()?)?;
/// let options = RunOptions::default().observe(langfuse.observer());
/// // ... run workflows or agents with `options` ...
/// langfuse.flush().await; // before a short-lived program exits
/// # Ok(()) }
/// ```
#[derive(Clone)]
pub struct OtlpExporter {
    commands: mpsc::Sender<Command>,
    counters: Arc<Counters>,
    endpoint: String,
}

impl std::fmt::Debug for OtlpExporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OtlpExporter")
            .field("endpoint", &self.endpoint)
            .field("stats", &self.stats())
            .finish()
    }
}

impl OtlpExporter {
    /// Starts the background task on the current Tokio runtime.
    pub fn start(config: OtlpConfig) -> Result<Self, TelemetryError> {
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| TelemetryError::NoRuntime)?;
        let client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            .build()
            .map_err(|err| TelemetryError::Client(err.to_string()))?;
        let (commands, receiver) = mpsc::channel(config.queue_capacity);
        let counters = Arc::new(Counters::default());
        let endpoint = config.endpoint.clone();
        let task = Task {
            builder: SpanBuilder::new(ContentPolicy {
                capture: config.capture_content,
                mask: config.mask.clone(),
                max_chars: config.max_attribute_chars,
            }),
            batch: Vec::new(),
            client,
            config,
            counters: counters.clone(),
        };
        runtime.spawn(task.run(receiver));
        Ok(Self {
            commands,
            counters,
            endpoint,
        })
    }

    /// The observer to attach with
    /// [`RunOptions::observe`](criogenai_core::RunOptions::observe). One
    /// exporter can observe any number of runs at once.
    pub fn observer(&self) -> impl EventObserver + Clone + use<> {
        ExportObserver {
            commands: self.commands.clone(),
            counters: self.counters.clone(),
        }
    }

    /// Waits until every event received so far is exported (or has failed).
    pub async fn flush(&self) {
        let (done, wait) = oneshot::channel();
        if self.commands.send(Command::Flush(done)).await.is_ok() {
            let _ = wait.await;
        }
    }

    /// Flushes, closes spans of unfinished runs as incomplete, and stops the
    /// background task.
    pub async fn shutdown(&self) {
        let (done, wait) = oneshot::channel();
        if self.commands.send(Command::Shutdown(done)).await.is_ok() {
            let _ = wait.await;
        }
    }

    /// Counters so far.
    pub fn stats(&self) -> ExportStats {
        ExportStats {
            exported: self.counters.exported.load(Ordering::Relaxed),
            dropped_events: self.counters.dropped_events.load(Ordering::Relaxed),
            failed: self.counters.failed.load(Ordering::Relaxed),
        }
    }

    /// The last export error, if any (for diagnostics).
    pub fn last_error(&self) -> Option<String> {
        self.counters
            .last_error
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

#[derive(Clone)]
struct ExportObserver {
    commands: mpsc::Sender<Command>,
    counters: Arc<Counters>,
}

impl EventObserver for ExportObserver {
    fn on_event(&self, event: &Event) {
        // Never blocks a run: a full queue drops the event (design 0009 T2).
        if self
            .commands
            .try_send(Command::Event(Box::new(event.clone())))
            .is_err()
        {
            self.counters.dropped_events.fetch_add(1, Ordering::Relaxed);
        }
    }
}

struct Task {
    builder: SpanBuilder,
    batch: Vec<Span>,
    client: reqwest::Client,
    config: OtlpConfig,
    counters: Arc<Counters>,
}

impl Task {
    async fn run(mut self, mut receiver: mpsc::Receiver<Command>) {
        let mut ticker = tokio::time::interval(self.config.flush_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                command = receiver.recv() => match command {
                    Some(Command::Event(event)) => {
                        let finished = self.builder.on_event(&event);
                        self.batch.extend(finished);
                        if self.batch.len() >= self.config.batch_size {
                            self.send_batch().await;
                        }
                    }
                    Some(Command::Flush(done)) => {
                        self.send_all().await;
                        let _ = done.send(());
                    }
                    Some(Command::Shutdown(done)) => {
                        let open = self.builder.close_all(now_ms());
                        self.batch.extend(open);
                        self.send_all().await;
                        let _ = done.send(());
                        return;
                    }
                    // Every exporter handle and observer is gone.
                    None => {
                        let open = self.builder.close_all(now_ms());
                        self.batch.extend(open);
                        self.send_all().await;
                        return;
                    }
                },
                _ = ticker.tick() => {
                    if !self.batch.is_empty() {
                        self.send_batch().await;
                    }
                }
            }
        }
    }

    async fn send_all(&mut self) {
        while !self.batch.is_empty() {
            self.send_batch().await;
        }
    }

    async fn send_batch(&mut self) {
        let count = self.batch.len().min(self.config.batch_size);
        let spans: Vec<Span> = self.batch.drain(..count).collect();
        let body = otlp::encode(&spans, &self.config.service_name).to_string();
        let mut delay = Duration::from_millis(500);
        for attempt in 1..=3 {
            match self.post(&body).await {
                Ok(()) => {
                    self.counters
                        .exported
                        .fetch_add(spans.len() as u64, Ordering::Relaxed);
                    return;
                }
                Err((retryable, message)) => {
                    *self
                        .counters
                        .last_error
                        .lock()
                        .unwrap_or_else(|p| p.into_inner()) = Some(message);
                    if !retryable || attempt == 3 {
                        break;
                    }
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        }
        self.counters
            .failed
            .fetch_add(spans.len() as u64, Ordering::Relaxed);
    }

    /// `Err((retryable, message))` on failure.
    async fn post(&self, body: &str) -> Result<(), (bool, String)> {
        let mut request = self
            .client
            .post(&self.config.endpoint)
            .header("content-type", "application/json")
            .body(body.to_string());
        for (name, value) in &self.config.headers {
            request = request.header(name, value);
        }
        match request.send().await {
            Ok(response) if response.status().is_success() => Ok(()),
            Ok(response) => {
                let status = response.status().as_u16();
                let text = response.text().await.unwrap_or_default();
                let retryable = status == 429 || status >= 500;
                let text: String = text.chars().take(300).collect();
                Err((retryable, format!("HTTP {status}: {text}")))
            }
            Err(err) => Err((true, err.without_url().to_string())),
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
