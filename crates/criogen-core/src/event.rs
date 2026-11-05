//! Run events: what is happening inside a run, as it happens (design 0005 §6).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::runtime::Interrupt;

/// One thing that happened during a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Event {
    /// Position in the run's event sequence: starts at 0 and continues across
    /// suspend and resume.
    pub seq: u64,
    /// The run this event belongs to.
    pub run_id: String,
    /// The step, for step and node events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<usize>,
    /// The node, for node events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The node execution's [`task_id`](crate::NodeContext::task_id), for node
    /// events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Who emitted it: `"internal"` (the engine) or `"user"` (node code).
    pub origin: String,
    /// What happened.
    #[serde(flatten)]
    pub kind: EventKind,
    /// Unix time in milliseconds.
    pub ts_ms: u64,
}

impl Event {
    /// The event's `type` as it appears in JSON, e.g. `"node_started"`.
    pub fn type_name(&self) -> &'static str {
        self.kind.type_name()
    }
}

/// The event types. Serialized as `"type"` plus `"data"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum EventKind {
    /// A run (or a resumed segment of one) started.
    RunStarted {
        /// `true` when continuing from a snapshot.
        resumed: bool,
        /// The run's [`RunOptions::metadata`](crate::RunOptions::metadata),
        /// if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Value>,
    },
    /// A step started.
    StepStarted {
        /// The nodes it runs, in declaration order.
        nodes: Vec<String>,
    },
    /// A node started executing.
    NodeStarted {
        /// 1-based attempt number.
        attempt: u32,
    },
    /// A node returned successfully.
    NodeFinished {
        /// Wall time from start to return.
        duration_ms: f64,
        /// State keys in its update.
        update_keys: Vec<String>,
        /// Targets it chose with a `Command`, if any.
        goto: Vec<String>,
        /// `true` if it returned `Suspend`.
        suspended: bool,
        /// `true` if the node did not run: its output was saved before the
        /// step failed and is reused on resume (0005 §5.1).
        #[serde(default)]
        reused: bool,
    },
    /// A node returned an error, panicked or timed out.
    NodeFailed {
        /// Wall time from start to failure.
        duration_ms: f64,
        /// The run error code, e.g. `node_failed`, `node_panicked`,
        /// `node_timeout`.
        code: String,
        /// The error message.
        message: String,
    },
    /// A node attempt failed and will be retried (design 0005 §4.2).
    AttemptFailed {
        /// The attempt that failed (1-based).
        attempt: u32,
        /// Always `node_failed` for now.
        code: String,
        /// The error message.
        message: String,
        /// Backoff before the next attempt.
        retry_in_ms: u64,
    },
    /// A node was stopped before finishing.
    NodeCancelled {
        /// `sibling_failed`, `run_cancelled` or `deadline_exceeded`.
        reason: String,
    },
    /// A step's updates were committed.
    StepCommitted {
        /// Nodes scheduled for the next step, in declaration order.
        next: Vec<String>,
    },
    /// The run paused for input.
    Suspended {
        /// The suspended nodes and their payloads.
        interrupts: Vec<Interrupt>,
    },
    /// The run (or segment) ended. Always the last event.
    RunFinished {
        /// `completed`, `suspended` or `failed`.
        status: String,
        /// The error code, if failed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        /// Steps executed in this segment.
        steps: usize,
    },
    /// Emitted by node code through [`EventSink::emit`].
    Custom {
        /// Application-defined name.
        name: String,
        /// Application-defined data.
        payload: Value,
    },
}

impl EventKind {
    /// The `type` as it appears in JSON.
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::RunStarted { .. } => "run_started",
            Self::StepStarted { .. } => "step_started",
            Self::NodeStarted { .. } => "node_started",
            Self::NodeFinished { .. } => "node_finished",
            Self::NodeFailed { .. } => "node_failed",
            Self::AttemptFailed { .. } => "attempt_failed",
            Self::NodeCancelled { .. } => "node_cancelled",
            Self::StepCommitted { .. } => "step_committed",
            Self::Suspended { .. } => "suspended",
            Self::RunFinished { .. } => "run_finished",
            Self::Custom { .. } => "custom",
        }
    }
}

/// Creates a channel for one run's events. Pass the sender to
/// [`RunOptions::events`](crate::RunOptions::events) and read the receiver
/// **while the run executes**: once `capacity` events are waiting, the run
/// pauses at the next step boundary until they are read. Dropping the receiver
/// stops delivery; the run continues.
///
/// # Panics
///
/// If `capacity` is 0.
pub fn event_channel(capacity: usize) -> (EventSender, EventReceiver) {
    let (tx, rx) = mpsc::channel(capacity);
    (EventSender(tx), EventReceiver(rx))
}

/// The sending half of [`event_channel`].
#[derive(Debug, Clone)]
pub struct EventSender(pub(crate) mpsc::Sender<Event>);

/// The receiving half of [`event_channel`].
#[derive(Debug)]
pub struct EventReceiver(mpsc::Receiver<Event>);

impl EventReceiver {
    /// The next event, or `None` once the run has finished and every event
    /// has been received.
    pub async fn recv(&mut self) -> Option<Event> {
        self.0.recv().await
    }
}

/// Receives every event of a run as it is emitted, alongside any
/// [`event_channel`] subscriber. Attach one with
/// [`RunOptions::observe`](crate::RunOptions::observe).
///
/// `on_event` is called synchronously, in `seq` order, while the event is
/// being emitted, so it **must not block** and must not emit events itself.
/// Exporters push the event into their own queue and return.
pub trait EventObserver: Send + Sync + 'static {
    /// Called once per event.
    fn on_event(&self, event: &Event);
}

impl<F> EventObserver for F
where
    F: Fn(&Event) + Send + Sync + 'static,
{
    fn on_event(&self, event: &Event) {
        self(event);
    }
}

/// Lets node code emit [`EventKind::Custom`] events. Obtained from
/// [`NodeContext::events`](crate::NodeContext::events).
#[derive(Debug, Clone, Default)]
pub struct EventSink {
    hub: Option<Arc<Hub>>,
    scope: Scope,
}

impl EventSink {
    pub(crate) fn new(hub: Option<Arc<Hub>>, scope: Scope) -> Self {
        Self { hub, scope }
    }

    /// `true` if someone is listening. Use it to skip building expensive
    /// payloads.
    pub fn is_enabled(&self) -> bool {
        self.hub.as_ref().is_some_and(|hub| hub.is_enabled())
    }

    /// Emits a custom event. Never blocks; does nothing without a subscriber.
    pub fn emit(&self, name: impl Into<String>, payload: impl Into<Value>) {
        if let Some(hub) = &self.hub
            && hub.is_enabled()
        {
            hub.emit(
                &self.scope,
                "user",
                EventKind::Custom {
                    name: name.into(),
                    payload: payload.into(),
                },
            );
        }
    }
}

/// Where an event happened.
#[derive(Debug, Clone, Default)]
pub(crate) struct Scope {
    pub(crate) step: Option<usize>,
    pub(crate) node: Option<Arc<str>>,
}

/// Delivers one run's events to its subscriber, in `seq` order.
///
/// `emit` is synchronous and never waits: an event goes straight into the
/// channel when there is room and nothing is queued ahead of it; otherwise it
/// joins the backlog. `flush` (called by the run loop at step boundaries)
/// waits until the backlog is delivered. That is the only place the run
/// experiences backpressure.
pub(crate) struct Hub {
    run_id: Arc<str>,
    enabled: AtomicBool,
    observers: Vec<Arc<dyn EventObserver>>,
    inner: Mutex<Inner>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub")
            .field("run_id", &self.run_id)
            .field("observers", &self.observers.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Inner {
    next_seq: u64,
    tx: Option<mpsc::Sender<Event>>,
    backlog: VecDeque<Event>,
    /// `flush` has taken an event off the backlog but not delivered it yet;
    /// new events must queue behind it.
    flushing: bool,
}

impl Hub {
    pub(crate) fn new(
        run_id: Arc<str>,
        sender: Option<EventSender>,
        observers: Vec<Arc<dyn EventObserver>>,
        first_seq: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            run_id,
            enabled: AtomicBool::new(sender.is_some() || !observers.is_empty()),
            observers,
            inner: Mutex::new(Inner {
                next_seq: first_seq,
                tx: sender.map(|s| s.0),
                backlog: VecDeque::new(),
                flushing: false,
            }),
        })
    }

    pub(crate) fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// The `seq` the next event will get.
    pub(crate) fn next_seq(&self) -> u64 {
        self.lock().next_seq
    }

    pub(crate) fn emit(&self, scope: &Scope, origin: &str, kind: EventKind) {
        let mut inner = self.lock();
        if inner.tx.is_none() && self.observers.is_empty() {
            return;
        }
        let task_id = match (scope.step, &scope.node) {
            (Some(step), Some(node)) => Some(format!("{}:{step}:{node}", self.run_id)),
            _ => None,
        };
        let event = Event {
            seq: inner.next_seq,
            run_id: self.run_id.to_string(),
            step: scope.step,
            node: scope.node.as_deref().map(str::to_string),
            task_id,
            origin: origin.to_string(),
            kind,
            ts_ms: now_ms(),
        };
        inner.next_seq += 1;

        // Observers see every event, in seq order (the lock is held).
        for observer in &self.observers {
            observer.on_event(&event);
        }
        let Some(tx) = inner.tx.clone() else {
            return;
        };
        if inner.flushing || !inner.backlog.is_empty() {
            inner.backlog.push_back(event);
            return;
        }
        match tx.try_send(event) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(event)) => inner.backlog.push_back(event),
            Err(mpsc::error::TrySendError::Closed(_)) => self.disconnect(&mut inner),
        }
    }

    /// Delivers the backlog, waiting for the subscriber as needed.
    pub(crate) async fn flush(&self) {
        loop {
            let (tx, event) = {
                let mut inner = self.lock();
                let (Some(tx), Some(event)) = (inner.tx.clone(), inner.backlog.pop_front()) else {
                    inner.flushing = false;
                    return;
                };
                inner.flushing = true;
                (tx, event)
            };
            if tx.send(event).await.is_err() {
                let mut inner = self.lock();
                self.disconnect(&mut inner);
                inner.flushing = false;
                return;
            }
        }
    }

    fn disconnect(&self, inner: &mut Inner) {
        inner.tx = None;
        inner.backlog.clear();
        // Observers keep receiving events.
        self.enabled
            .store(!self.observers.is_empty(), Ordering::Relaxed);
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A panic while holding this lock cannot leave `Inner` inconsistent,
        // so a poisoned lock is safe to reuse.
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn now_ms() -> u64 {
    criogen_rt::system_now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hub(capacity: usize) -> (Arc<Hub>, EventReceiver) {
        let (tx, rx) = event_channel(capacity);
        (Hub::new("run".into(), Some(tx), Vec::new(), 0), rx)
    }

    fn custom(n: u64) -> EventKind {
        EventKind::Custom {
            name: "n".into(),
            payload: n.into(),
        }
    }

    #[tokio::test]
    async fn full_channel_queues_in_order_and_flush_delivers() {
        let (hub, mut rx) = hub(2);
        for n in 0..5 {
            hub.emit(&Scope::default(), "user", custom(n));
        }
        let reader = tokio::spawn(async move {
            let mut seqs = Vec::new();
            while let Some(event) = rx.recv().await {
                seqs.push(event.seq);
            }
            seqs
        });
        hub.flush().await;
        // Emitting after the backlog drained goes straight to the channel.
        hub.emit(&Scope::default(), "user", custom(5));
        drop(hub);
        assert_eq!(reader.await.unwrap(), vec![0, 1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn dropped_receiver_disables_the_hub() {
        let (hub, rx) = hub(1);
        drop(rx);
        hub.emit(&Scope::default(), "user", custom(0));
        assert!(!hub.is_enabled());
        hub.flush().await; // returns immediately
    }

    #[tokio::test]
    async fn flush_returns_when_receiver_drops_mid_flush() {
        let (hub, rx) = hub(1);
        for n in 0..3 {
            hub.emit(&Scope::default(), "user", custom(n));
        }
        let flush = tokio::spawn({
            let hub = hub.clone();
            async move { hub.flush().await }
        });
        tokio::task::yield_now().await;
        drop(rx);
        flush.await.unwrap();
        assert!(!hub.is_enabled());
    }

    #[test]
    fn event_json_shape() {
        let event = Event {
            seq: 3,
            run_id: "r".into(),
            step: Some(1),
            node: Some("a".into()),
            task_id: Some("r:1:a".into()),
            origin: "internal".into(),
            kind: EventKind::NodeStarted { attempt: 1 },
            ts_ms: 7,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "seq": 3, "run_id": "r", "step": 1, "node": "a", "task_id": "r:1:a",
                "origin": "internal", "type": "node_started", "data": {"attempt": 1},
                "ts_ms": 7
            })
        );
        let back: Event = serde_json::from_value(json).unwrap();
        assert_eq!(back, event);
        assert_eq!(back.type_name(), "node_started");
    }
}
