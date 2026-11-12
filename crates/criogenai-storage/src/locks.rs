use std::sync::Arc;
use std::time::Duration;

use criogenai_core::BoxFuture;

use crate::error::StorageError;

/// Called when another process asks the run holding a thread to stop
/// ([`OnBusy::Interrupt`](crate::OnBusy::Interrupt)).
pub type InterruptHandler = Arc<dyn Fn() + Send + Sync>;

/// Locks on threads shared by every process using the same database, so
/// [`OnBusy`](crate::OnBusy) holds across processes, not only inside one
/// (design 0008 §5.2). [`Storage`](crate::Storage) takes the lock after
/// its in-process lease and drops it with the lease.
///
/// Without thread locks, a second process is still kept from forking a
/// thread by [`Checkpointer::commit`](crate::Checkpointer::commit)'s
/// compare-and-set, but it fails with `storage_conflict` at its first save
/// instead of waiting its turn.
pub trait ThreadLocks: Send + Sync + 'static {
    /// Takes `thread`'s lock. `wait = None` tries once; `Some(limit)` waits
    /// at most `limit`. `Ok(None)` if another holder kept it. While the
    /// lock is held, `on_interrupt` runs whenever
    /// [`interrupt`](Self::interrupt) is called for the thread.
    fn lock<'a>(
        &'a self,
        thread: &'a str,
        wait: Option<Duration>,
        on_interrupt: InterruptHandler,
    ) -> BoxFuture<'a, Result<Option<HeldLock>, StorageError>>;

    /// Asks whoever holds `thread`'s lock, in any process, to stop its run.
    fn interrupt<'a>(&'a self, thread: &'a str) -> BoxFuture<'a, Result<(), StorageError>>;
}

/// A thread lock from [`ThreadLocks::lock`]; dropping it releases the lock.
pub struct HeldLock(#[allow(dead_code)] Box<dyn Send + Sync>);

impl HeldLock {
    /// Wraps `guard`, whose drop releases the lock.
    pub fn new(guard: impl Send + Sync + 'static) -> Self {
        Self(Box::new(guard))
    }
}

impl std::fmt::Debug for HeldLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeldLock").finish_non_exhaustive()
    }
}
