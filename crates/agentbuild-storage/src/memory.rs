use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use agentbuild_core::BoxFuture;

use crate::checkpoint::{Checkpoint, CheckpointMeta, Checkpointer};
use crate::error::{StorageError, StorageErrorKind};

/// Keeps checkpoints in process memory. Lost when the process exits: for
/// tests, scripts and the default when no storage is configured.
#[derive(Debug, Default)]
pub struct InMemoryCheckpointer {
    threads: Mutex<HashMap<String, Thread>>,
}

#[derive(Debug, Default)]
struct Thread {
    head: Option<String>,
    version: u64,
    checkpoints: HashMap<String, Checkpoint>,
}

impl InMemoryCheckpointer {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    fn threads(&self) -> MutexGuard<'_, HashMap<String, Thread>> {
        // A panic while holding the lock cannot leave a thread half
        // written: every change below is a single insert.
        self.threads.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Checkpointer for InMemoryCheckpointer {
    fn load<'a>(
        &'a self,
        thread: &'a str,
        at: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<Checkpoint>, StorageError>> {
        let threads = self.threads();
        let found = threads.get(thread).and_then(|t| {
            let id = at.or(t.head.as_deref())?;
            t.checkpoints.get(id).cloned()
        });
        Box::pin(async move { Ok(found) })
    }

    fn commit(
        &self,
        checkpoint: Checkpoint,
        expected: u64,
    ) -> BoxFuture<'_, Result<(), StorageError>> {
        let mut threads = self.threads();
        let current = threads.get(&checkpoint.thread_id).map_or(0, |t| t.version);
        let result = if current != expected {
            Err(StorageError::new(
                StorageErrorKind::Conflict,
                format!(
                    "thread `{}` is at version {current}, not {expected}",
                    checkpoint.thread_id
                ),
            ))
        } else if checkpoint.version != expected + 1 {
            Err(StorageError::new(
                StorageErrorKind::Conflict,
                format!(
                    "checkpoint version {} does not follow {expected}",
                    checkpoint.version
                ),
            ))
        } else {
            let thread = threads.entry(checkpoint.thread_id.clone()).or_default();
            thread.head = Some(checkpoint.id.clone());
            thread.version = checkpoint.version;
            thread.checkpoints.insert(checkpoint.id.clone(), checkpoint);
            Ok(())
        };
        Box::pin(async move { result })
    }

    fn history<'a>(
        &'a self,
        thread: &'a str,
        limit: usize,
    ) -> BoxFuture<'a, Result<Vec<CheckpointMeta>, StorageError>> {
        let threads = self.threads();
        let mut list = Vec::new();
        if let Some(t) = threads.get(thread) {
            let mut next = t.head.as_deref();
            while let Some(id) = next.filter(|_| list.len() < limit) {
                let Some(checkpoint) = t.checkpoints.get(id) else {
                    break;
                };
                list.push(checkpoint.meta());
                next = checkpoint.parent_id.as_deref();
            }
        }
        Box::pin(async move { Ok(list) })
    }

    fn delete_thread<'a>(&'a self, thread: &'a str) -> BoxFuture<'a, Result<bool, StorageError>> {
        let existed = self.threads().remove(thread).is_some();
        Box::pin(async move { Ok(existed) })
    }
}
