use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use agentbuild_core::CancelToken;
use tokio::sync::OwnedMutexGuard;

use crate::checkpoint::Checkpointer;
use crate::error::{StorageError, StorageErrorKind};
use crate::memory::InMemoryCheckpointer;

/// What a second operation on a busy thread does (design 0008 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum OnBusy {
    /// Wait for the active run to finish (FIFO), at most
    /// [`Storage::busy_timeout`]; then `thread_busy`.
    #[default]
    Wait,
    /// Fail at once with `thread_busy`.
    Reject,
    /// Cancel the active run, then wait for it to end.
    Interrupt,
}

/// A checkpointer plus the rules for using threads. Cheap to clone; clones
/// share the checkpointer and the thread leases.
#[derive(Clone)]
pub struct Storage {
    checkpointer: Arc<dyn Checkpointer>,
    leases: Arc<Leases>,
    on_busy: OnBusy,
    busy_timeout: Duration,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage")
            .field("on_busy", &self.on_busy)
            .field("busy_timeout", &self.busy_timeout)
            .finish_non_exhaustive()
    }
}

impl Default for Storage {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl Storage {
    /// Storage in process memory ([`InMemoryCheckpointer`]).
    pub fn in_memory() -> Self {
        Self::new(InMemoryCheckpointer::new())
    }

    /// Storage on `checkpointer`.
    pub fn new(checkpointer: impl Checkpointer) -> Self {
        Self::shared(Arc::new(checkpointer))
    }

    /// [`new`](Self::new) with a shared checkpointer.
    pub fn shared(checkpointer: Arc<dyn Checkpointer>) -> Self {
        Self {
            checkpointer,
            leases: Arc::default(),
            on_busy: OnBusy::default(),
            busy_timeout: Duration::from_secs(60),
        }
    }

    /// Sets what an operation on a busy thread does. Default [`OnBusy::Wait`].
    pub fn on_busy(mut self, on_busy: OnBusy) -> Self {
        self.on_busy = on_busy;
        self
    }

    /// How long [`OnBusy::Wait`] and [`OnBusy::Interrupt`] wait. Default 60 s.
    pub fn busy_timeout(mut self, timeout: Duration) -> Self {
        self.busy_timeout = timeout;
        self
    }

    /// The checkpointer.
    pub fn checkpointer(&self) -> &Arc<dyn Checkpointer> {
        &self.checkpointer
    }

    /// Takes the thread for one operation, following
    /// [`on_busy`](Self::on_busy). Released when the lease is dropped.
    ///
    /// Leases are per process (shared by clones of this `Storage`); across
    /// processes, [`Checkpointer::commit`]'s compare-and-set keeps a thread
    /// from forking.
    pub async fn lease(&self, thread: &str) -> Result<ThreadLease, StorageError> {
        let slot = self.leases.slot(thread);
        let lock = slot.lock.clone();
        let guard = match (lock.clone().try_lock_owned().ok(), self.on_busy) {
            (Some(guard), _) => guard,
            (None, OnBusy::Reject) => {
                drop(slot);
                self.leases.release(thread);
                return Err(busy(thread, "another run is active"));
            }
            (None, on_busy) => {
                if on_busy == OnBusy::Interrupt
                    && let Some(token) = lock_slot(&slot.active).as_ref()
                {
                    token.cancel();
                }
                match tokio::time::timeout(self.busy_timeout, lock.lock_owned()).await {
                    Ok(guard) => guard,
                    Err(_) => {
                        drop(slot);
                        self.leases.release(thread);
                        return Err(busy(
                            thread,
                            format!(
                                "another run is still active after {} ms",
                                self.busy_timeout.as_millis()
                            ),
                        ));
                    }
                }
            }
        };
        Ok(ThreadLease {
            thread: thread.to_string(),
            guard: Some(guard),
            slot: Some(slot),
            leases: self.leases.clone(),
        })
    }

    /// Cancels the run that holds `thread` in this process, if any. `true`
    /// if one was running.
    pub fn cancel(&self, thread: &str) -> bool {
        let slot = self.leases.existing(thread);
        let token = slot
            .as_ref()
            .and_then(|slot| lock_slot(&slot.active).clone());
        if let Some(token) = &token {
            token.cancel();
        }
        let cancelled = token.is_some();
        drop(slot);
        self.leases.release(thread);
        cancelled
    }
}

fn busy(thread: &str, why: impl std::fmt::Display) -> StorageError {
    StorageError::new(
        StorageErrorKind::ThreadBusy,
        format!("thread `{thread}` is busy: {why}"),
    )
}

/// Exclusive use of a thread in this process, from [`Storage::lease`].
pub struct ThreadLease {
    thread: String,
    guard: Option<OwnedMutexGuard<()>>,
    slot: Option<Arc<Slot>>,
    leases: Arc<Leases>,
}

impl std::fmt::Debug for ThreadLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadLease")
            .field("thread", &self.thread)
            .finish_non_exhaustive()
    }
}

impl ThreadLease {
    /// The thread.
    pub fn thread(&self) -> &str {
        &self.thread
    }

    /// Registers the token of the run now using the thread, for
    /// [`Storage::cancel`] and [`OnBusy::Interrupt`].
    pub fn set_cancel(&self, token: CancelToken) {
        if let Some(slot) = &self.slot {
            *lock_slot(&slot.active) = Some(token);
        }
    }
}

impl Drop for ThreadLease {
    fn drop(&mut self) {
        if let Some(slot) = &self.slot {
            *lock_slot(&slot.active) = None;
        }
        self.guard = None;
        self.slot = None;
        self.leases.release(&self.thread);
    }
}

/// One entry per thread in use or waited for.
#[derive(Debug, Default)]
struct Leases(Mutex<HashMap<String, Arc<Slot>>>);

#[derive(Debug, Default)]
struct Slot {
    lock: Arc<tokio::sync::Mutex<()>>,
    active: Mutex<Option<CancelToken>>,
}

impl Leases {
    fn map(&self) -> MutexGuard<'_, HashMap<String, Arc<Slot>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn slot(&self, thread: &str) -> Arc<Slot> {
        self.map().entry(thread.to_string()).or_default().clone()
    }

    fn existing(&self, thread: &str) -> Option<Arc<Slot>> {
        self.map().get(thread).cloned()
    }

    /// Drops the thread's entry once only the table refers to it. Callers
    /// drop their own `Arc<Slot>` first.
    fn release(&self, thread: &str) {
        let mut map = self.map();
        if map
            .get(thread)
            .is_some_and(|slot| Arc::strong_count(slot) == 1)
        {
            map.remove(thread);
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.map().len()
    }
}

fn lock_slot<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn the_lease_table_forgets_idle_threads() {
        let storage = Storage::in_memory();
        let first = storage.lease("t").await.unwrap();
        let waiter = {
            let storage = storage.clone();
            tokio::spawn(async move { storage.lease("t").await.map(drop) })
        };
        tokio::task::yield_now().await;
        assert_eq!(storage.leases.len(), 1);
        drop(first);
        waiter.await.unwrap().unwrap();
        assert_eq!(storage.leases.len(), 0);
        assert!(!storage.cancel("t"));
        assert_eq!(storage.leases.len(), 0);

        let rejecting = storage.clone().on_busy(OnBusy::Reject);
        let held = storage.lease("t").await.unwrap();
        assert_eq!(
            rejecting.lease("t").await.unwrap_err().code(),
            "thread_busy"
        );
        drop(held);
        assert_eq!(storage.leases.len(), 0);
    }
}
