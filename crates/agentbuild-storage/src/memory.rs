use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use agentbuild_core::{BoxFuture, PendingWrite};

use crate::checkpoint::{
    Checkpoint, CheckpointMeta, Checkpointer, ThreadInfo, ThreadQuery, merge_pending, now_ms,
    prune_plan,
};
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
    created_at_ms: u64,
    updated_at_ms: u64,
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
            let now = now_ms();
            let thread = threads
                .entry(checkpoint.thread_id.clone())
                .or_insert_with(|| Thread {
                    created_at_ms: now,
                    ..Thread::default()
                });
            thread.updated_at_ms = now;
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

    fn put_pending<'a>(
        &'a self,
        thread: &'a str,
        checkpoint: &'a str,
        node: &'a str,
        write: PendingWrite,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        let mut threads = self.threads();
        let snapshot = threads
            .get_mut(thread)
            .and_then(|t| t.checkpoints.get_mut(checkpoint))
            .and_then(|c| c.snapshot.as_mut());
        let result = match snapshot {
            Some(snapshot) => {
                merge_pending(snapshot, node, write);
                Ok(())
            }
            None => Err(StorageError::new(
                StorageErrorKind::CheckpointNotFound,
                format!("thread `{thread}` has no checkpoint `{checkpoint}` with a run to add to"),
            )),
        };
        Box::pin(async move { result })
    }

    fn threads<'a>(
        &'a self,
        query: &'a ThreadQuery,
    ) -> BoxFuture<'a, Result<Vec<ThreadInfo>, StorageError>> {
        let threads = self.threads();
        let mut found: Vec<ThreadInfo> = threads
            .iter()
            .filter(|(id, t)| {
                query
                    .after
                    .as_deref()
                    .is_none_or(|after| id.as_str() > after)
                    && query.updated_before.is_none_or(|ms| t.updated_at_ms < ms)
                    && query.updated_after.is_none_or(|ms| t.updated_at_ms >= ms)
            })
            .map(|(id, t)| ThreadInfo {
                thread_id: id.clone(),
                version: t.version,
                created_at_ms: t.created_at_ms,
                updated_at_ms: t.updated_at_ms,
            })
            .collect();
        found.sort_by(|a, b| a.thread_id.cmp(&b.thread_id));
        found.truncate(query.limit);
        Box::pin(async move { Ok(found) })
    }

    fn prune_thread<'a>(
        &'a self,
        thread: &'a str,
        keep: Option<usize>,
    ) -> BoxFuture<'a, Result<u64, StorageError>> {
        let mut threads = self.threads();
        let mut removed = 0;
        if let Some(t) = threads.get_mut(thread)
            && let Some(head) = t.head.clone()
        {
            let links: Vec<(String, Option<String>)> = t
                .checkpoints
                .values()
                .map(|c| (c.id.clone(), c.parent_id.clone()))
                .collect();
            let (kept, deleted) = prune_plan(&head, &links, keep);
            if !deleted.is_empty() {
                for id in &deleted {
                    t.checkpoints.remove(id);
                }
                // History ends at the oldest kept checkpoint.
                if let Some(oldest) = kept.last().and_then(|id| t.checkpoints.get_mut(id))
                    && oldest
                        .parent_id
                        .as_ref()
                        .is_some_and(|parent| deleted.contains(parent))
                {
                    oldest.parent_id = None;
                }
                removed = deleted.len() as u64;
            }
        }
        Box::pin(async move { Ok(removed) })
    }
}
