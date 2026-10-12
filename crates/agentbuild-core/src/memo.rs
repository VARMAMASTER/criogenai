//! Task-scoped saved results (design 0005 §5.1).

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::error::NodeError;
use crate::persist::Persister;
use crate::runtime::PendingWrite;

/// Results a node saves while it runs, so work that already finished is not
/// repeated when the same task executes again: on a retry, or when a failed
/// or suspended step is resumed (also from another process, through the
/// [`Snapshot`](crate::Snapshot)).
///
/// Read it with [`NodeContext::memo`](crate::NodeContext::memo). Clones share
/// the same entries. Values are JSON and should be small (ids, receipts, tool
/// results), not documents.
#[derive(Debug, Clone, Default)]
pub struct TaskMemo {
    entries: Arc<Mutex<BTreeMap<String, Value>>>,
    /// Where [`save`](Self::save) writes through to, if the run is persisted.
    sink: Option<Sink>,
}

/// The run's persister, and which task this memo belongs to.
#[derive(Clone)]
struct Sink {
    persister: Arc<dyn Persister>,
    step: usize,
    node: Arc<str>,
}

impl std::fmt::Debug for Sink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sink({}:{})", self.step, self.node)
    }
}

impl TaskMemo {
    pub(crate) fn from_entries(entries: BTreeMap<String, Value>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(entries)),
            sink: None,
        }
    }

    /// A handle on the same entries whose [`save`](Self::save) also writes
    /// to `persister`.
    pub(crate) fn persisted(
        &self,
        persister: Arc<dyn Persister>,
        step: usize,
        node: Arc<str>,
    ) -> Self {
        Self {
            entries: self.entries.clone(),
            sink: Some(Sink {
                persister,
                step,
                node,
            }),
        }
    }

    /// A copy of every entry.
    pub(crate) fn entries(&self) -> BTreeMap<String, Value> {
        self.lock().clone()
    }

    /// The value saved under `key` by an earlier attempt or execution of this
    /// task.
    pub fn get(&self, key: &str) -> Option<Value> {
        self.lock().get(key).cloned()
    }

    /// The saved keys, sorted.
    pub fn keys(&self) -> Vec<String> {
        self.lock().keys().cloned().collect()
    }

    /// Saves `value` under `key` in memory, replacing any earlier value. It
    /// survives retries and failures in this process, and reaches storage
    /// with the step's failure or suspension. Use [`save`](Self::save) for
    /// results that must survive a crash.
    pub fn put(&self, key: impl Into<String>, value: impl Into<Value>) {
        self.lock().insert(key.into(), value.into());
    }

    /// [`put`](Self::put), then waits until the run's storage has the entry
    /// (design 0008 §5.1), so even a crash does not repeat the work. Without
    /// persisted storage it is the same as `put`.
    ///
    /// On a storage error the entry is still kept in memory and the error
    /// (kind `storage_failed`) is returned: stop doing side effects, the
    /// run cannot be saved.
    pub async fn save(
        &self,
        key: impl Into<String>,
        value: impl Into<Value>,
    ) -> Result<(), NodeError> {
        let (key, value) = (key.into(), value.into());
        self.put(key.clone(), value.clone());
        let Some(sink) = &self.sink else {
            return Ok(());
        };
        let mut write = PendingWrite::default();
        write.memo.insert(key, value);
        sink.persister
            .save_task(sink.step, sink.node.to_string(), write)
            .await
            .map_err(|err| {
                NodeError::new(format!("could not save memo entry: {err}"))
                    .with_kind("storage_failed")
            })
    }

    /// Runs `f` once per task: returns the value saved under `key` if there is
    /// one, otherwise runs `f` and [`save`](Self::save)s its `Ok` result.
    /// Errors are not saved, so a failed call runs again next time.
    ///
    /// ```
    /// # use agentbuild_core::{NodeError, TaskMemo};
    /// # async fn send_email() -> Result<String, NodeError> { Ok("msg-1".into()) }
    /// # async fn demo(memo: &TaskMemo) -> Result<(), NodeError> {
    /// // Sent at most once, even if the node is retried or the step resumed.
    /// let receipt: String = memo.once("send_email", send_email).await?;
    /// # Ok(()) }
    /// ```
    ///
    /// This is not a lock: two concurrent calls with the same key inside one
    /// node both run `f`.
    pub async fn once<T, F, Fut>(&self, key: &str, f: F) -> Result<T, NodeError>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, NodeError>>,
    {
        if let Some(saved) = self.get(key) {
            return serde_json::from_value(saved).map_err(|err| {
                NodeError::new(format!("memo entry `{key}` has an unexpected shape: {err}"))
                    .with_kind("memo_type_mismatch")
            });
        }
        let value = f().await?;
        let json = serde_json::to_value(&value).map_err(|err| {
            NodeError::new(format!(
                "memo entry `{key}` is not JSON-serializable: {err}"
            ))
            .with_kind("memo_type_mismatch")
        })?;
        self.save(key, json).await?;
        Ok(value)
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<String, Value>> {
        // The map stays valid even if a holder panicked.
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn once_runs_the_function_only_once() {
        let memo = TaskMemo::default();
        let calls = AtomicU32::new(0);
        let call = || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, NodeError>(json!({"id": 7}))
        };
        assert_eq!(memo.once("k", call).await.unwrap(), json!({"id": 7}));
        assert_eq!(memo.once("k", call).await.unwrap(), json!({"id": 7}));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn errors_are_not_saved() {
        let memo = TaskMemo::default();
        let failed: Result<u32, _> = memo.once("k", || async { Err(NodeError::new("x")) }).await;
        assert!(failed.is_err());
        assert_eq!(memo.get("k"), None);
        assert_eq!(memo.once("k", || async { Ok(3_u32) }).await.unwrap(), 3);
    }

    #[tokio::test]
    async fn wrong_saved_shape_is_an_error() {
        let memo = TaskMemo::default();
        memo.put("k", "text");
        let err = memo.once("k", || async { Ok(1_u32) }).await.unwrap_err();
        assert_eq!(err.kind.as_deref(), Some("memo_type_mismatch"));
    }

    #[test]
    fn clones_share_entries() {
        let memo = TaskMemo::default();
        memo.clone().put("a", 1);
        memo.put("a", 2);
        assert_eq!(memo.get("a"), Some(json!(2)));
        assert_eq!(memo.entries().len(), 1);
    }
}
