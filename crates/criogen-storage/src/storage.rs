use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use criogen_core::CancelToken;
use tokio::sync::OwnedMutexGuard;

use crate::audit::{AuditEvent, AuditLog, InMemoryAuditLog};
use crate::checkpoint::Checkpointer;
use crate::error::{StorageError, StorageErrorKind};
use crate::locks::{HeldLock, InterruptHandler, ThreadLocks};
use crate::memory::InMemoryCheckpointer;
use crate::retention::Retention;
use crate::store::{InMemoryStore, MemoryStore};

#[cfg(feature = "encryption")]
type Keyring = crate::Keyring;
/// Without the `encryption` feature there are never keys.
#[cfg(not(feature = "encryption"))]
type Keyring = std::convert::Infallible;

/// A backend that can rewrite its rows with the active key
/// ([`Storage::reencrypt`]).
#[allow(dead_code)]
/// The database behind a [`Storage`] from [`Storage::connect`]: what
/// only the backend can do.
pub(crate) trait Backend: Send + Sync {
    /// Closes every connection (checkpoints, memory, locks, audit).
    fn close(&self) -> criogen_core::BoxFuture<'_, ()>;

    #[cfg(feature = "encryption")]
    fn reencrypt(
        &self,
    ) -> criogen_core::BoxFuture<'_, Result<crate::ReencryptReport, StorageError>>;
}

/// What a second operation on a busy thread does (design 0008 §5.2). Holds
/// across processes when the storage has [`ThreadLocks`] (Postgres);
/// otherwise within this process.
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

/// When a thread's run is written to storage (design 0008 §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Durability {
    /// A checkpoint before the first step and after every step, and each
    /// finished task (tool call) as it happens. A crash loses at most the
    /// unfinished tasks of one step.
    #[default]
    Step,
    /// One checkpoint per call, when the run completes, suspends or fails.
    /// A crash loses the whole call.
    Exit,
}

/// A checkpointer and a memory store, plus the rules for using threads.
/// Cheap to clone; clones share the backends and the thread leases.
#[derive(Clone)]
pub struct Storage {
    pub(crate) checkpointer: Arc<dyn Checkpointer>,
    pub(crate) memory: Arc<dyn MemoryStore>,
    locks: Option<Arc<dyn ThreadLocks>>,
    leases: Arc<Leases>,
    on_busy: OnBusy,
    busy_timeout: Duration,
    durability: Durability,
    pub(crate) retention: Retention,
    /// The database, for `close` and `reencrypt`.
    backend: Option<Arc<dyn Backend>>,
    /// Where actions are recorded, once turned on (design 0010).
    audit: Option<Arc<dyn AuditLog>>,
    /// The database's own trail, for [`Storage::with_audit`].
    database_audit: Option<Arc<dyn AuditLog>>,
}

impl std::fmt::Debug for Storage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Storage")
            .field("on_busy", &self.on_busy)
            .field("busy_timeout", &self.busy_timeout)
            .field("durability", &self.durability)
            .field("across_processes", &self.locks.is_some())
            .field("retention", &self.retention)
            .field("audited", &self.audit.is_some())
            .finish_non_exhaustive()
    }
}

impl Default for Storage {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl Storage {
    /// Storage in process memory ([`InMemoryCheckpointer`],
    /// [`InMemoryStore`]).
    pub fn in_memory() -> Self {
        Self::new(InMemoryCheckpointer::new())
    }

    /// Storage on `checkpointer`, with an in-memory memory store (replace
    /// it with [`with_memory`](Self::with_memory)).
    pub fn new(checkpointer: impl Checkpointer) -> Self {
        Self::shared(Arc::new(checkpointer))
    }

    /// Storage at `url`: `memory:` (in process memory); with the `sqlite`
    /// feature `sqlite://path/to/file.db` / `sqlite::memory:`; with the
    /// `postgres` feature `postgres://user:password@host/database` (thread
    /// locks across processes included). Creates the tables if needed.
    /// Threads and memory share the database.
    pub async fn connect(url: &str) -> Result<Self, StorageError> {
        Self::open(url, None).await
    }

    /// [`connect`](Self::connect) with encryption at rest (feature
    /// `encryption`, design 0008 §15.2): content in the database is
    /// encrypted with the active key of `keys`. `memory:` keeps nothing at
    /// rest and encrypts nothing.
    #[cfg(feature = "encryption")]
    pub async fn connect_encrypted(url: &str, keys: crate::Keyring) -> Result<Self, StorageError> {
        Self::open(url, Some(keys)).await
    }

    /// Rewrites every stored row that is plain or encrypted with an older
    /// key, with the active key, so older keys can be retired (design 0008
    /// §15.2). Fails with `encryption_key_missing` unless the storage was
    /// opened with [`connect_encrypted`](Self::connect_encrypted) on a
    /// database.
    #[cfg(feature = "encryption")]
    pub async fn reencrypt(&self) -> Result<crate::ReencryptReport, StorageError> {
        let report = match &self.backend {
            Some(backend) => backend.reencrypt().await?,
            None => {
                return Err(crate::crypto::key_missing(
                    "reencrypt needs a database opened with `Storage::connect_encrypted`",
                ));
            }
        };
        self.record(
            AuditEvent::new("storage.reencrypted")
                .outcome("ok")
                .detail("checkpoints", report.checkpoints)
                .detail("task_writes", report.task_writes)
                .detail("memory_items", report.memory_items),
        )
        .await?;
        Ok(report)
    }

    async fn open(url: &str, keys: Option<Keyring>) -> Result<Self, StorageError> {
        // Used only by the SQL backends.
        let _ = &keys;
        if url == "memory:" {
            return Ok(Self::in_memory());
        }
        if url.starts_with("sqlite:") {
            #[cfg(feature = "sqlite")]
            return {
                #[cfg(feature = "encryption")]
                let checkpointer = match keys {
                    Some(keys) => crate::SqliteCheckpointer::connect_encrypted(url, keys).await?,
                    None => crate::SqliteCheckpointer::connect(url).await?,
                };
                #[cfg(not(feature = "encryption"))]
                let checkpointer = crate::SqliteCheckpointer::connect(url).await?;
                let memory = checkpointer.memory_store();
                let audit = checkpointer.audit_log();
                let checkpointer = Arc::new(checkpointer);
                let mut storage = Self::shared(checkpointer.clone()).with_memory(memory);
                storage.backend = Some(checkpointer);
                storage.database_audit = Some(Arc::new(audit));
                Ok(storage)
            };
            #[cfg(not(feature = "sqlite"))]
            return Err(StorageError::new(
                StorageErrorKind::Unavailable,
                "SQLite storage needs the `sqlite` feature of criogen-storage",
            ));
        }
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            #[cfg(feature = "postgres")]
            return {
                #[allow(unused_mut)]
                let mut options = crate::PostgresOptions::new();
                #[cfg(feature = "encryption")]
                if let Some(keys) = keys {
                    options = options.encryption(keys);
                }
                let checkpointer = crate::PostgresCheckpointer::connect_with(url, options).await?;
                let memory = checkpointer.memory_store();
                let locks = checkpointer.thread_locks();
                let audit = checkpointer.audit_log();
                let checkpointer = Arc::new(checkpointer);
                let mut storage = Self::shared(checkpointer.clone())
                    .with_memory(memory)
                    .with_locks(locks);
                storage.backend = Some(checkpointer);
                storage.database_audit = Some(Arc::new(audit));
                Ok(storage)
            };
            #[cfg(not(feature = "postgres"))]
            return Err(StorageError::new(
                StorageErrorKind::Unavailable,
                "Postgres storage needs the `postgres` feature of criogen-storage",
            ));
        }
        // Only the scheme: the rest may hold a password.
        let scheme = url.split(':').next().unwrap_or_default();
        Err(StorageError::new(
            StorageErrorKind::Unavailable,
            format!(
                "unsupported storage URL scheme `{scheme}:` (expected `memory:`, `sqlite:` or \
                 `postgres:`)"
            ),
        ))
    }

    /// [`new`](Self::new) with a shared checkpointer.
    pub fn shared(checkpointer: Arc<dyn Checkpointer>) -> Self {
        Self {
            checkpointer,
            memory: Arc::new(InMemoryStore::new()),
            locks: None,
            leases: Arc::default(),
            on_busy: OnBusy::default(),
            busy_timeout: Duration::from_secs(60),
            durability: Durability::default(),
            retention: Retention::default(),
            backend: None,
            audit: None,
            database_audit: None,
        }
    }

    /// Turns the audit trail on (design 0010): in the database's
    /// `criogen_audit` table for storage from [`connect`](Self::connect)
    /// on SQLite or Postgres, otherwise in process memory. Agents record
    /// runs, model and tool calls, approvals, thread changes and memory
    /// writes; [`prune`](Self::prune) and `reencrypt` record themselves.
    pub fn with_audit(mut self) -> Self {
        self.audit = Some(
            self.database_audit
                .clone()
                .unwrap_or_else(|| Arc::new(InMemoryAuditLog::new())),
        );
        self
    }

    /// Records to `log` instead (any [`AuditLog`]).
    pub fn with_audit_log(self, log: impl AuditLog) -> Self {
        self.with_shared_audit_log(Arc::new(log))
    }

    /// [`with_audit_log`](Self::with_audit_log) with a shared log.
    pub fn with_shared_audit_log(mut self, log: Arc<dyn AuditLog>) -> Self {
        self.audit = Some(log);
        self
    }

    /// The audit trail, if turned on.
    pub fn audit(&self) -> Option<&Arc<dyn AuditLog>> {
        self.audit.as_ref()
    }

    /// Records `event` if the audit trail is on. A record that cannot be
    /// written fails with `audit_failed` (design 0010 §4: fail closed).
    pub async fn record(&self, event: AuditEvent) -> Result<(), StorageError> {
        let Some(log) = &self.audit else {
            return Ok(());
        };
        let action = event.action.clone();
        log.append(event).await.map(drop).map_err(|err| {
            StorageError::new(
                StorageErrorKind::AuditFailed,
                format!("could not record `{action}`: {err}"),
            )
        })
    }

    /// Uses `store` for long-term memory.
    pub fn with_memory(self, store: impl MemoryStore) -> Self {
        self.with_shared_memory(Arc::new(store))
    }

    /// [`with_memory`](Self::with_memory) with a shared store.
    pub fn with_shared_memory(mut self, store: Arc<dyn MemoryStore>) -> Self {
        self.memory = store;
        self
    }

    /// Locks threads across processes with `locks` (see [`ThreadLocks`]).
    pub fn with_locks(self, locks: impl ThreadLocks) -> Self {
        self.with_shared_locks(Arc::new(locks))
    }

    /// [`with_locks`](Self::with_locks) with shared locks.
    pub fn with_shared_locks(mut self, locks: Arc<dyn ThreadLocks>) -> Self {
        self.locks = Some(locks);
        self
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

    /// Sets when runs are written. Default [`Durability::Step`].
    pub fn durability(mut self, durability: Durability) -> Self {
        self.durability = durability;
        self
    }

    /// The [`durability`](Self::durability) setting.
    pub fn durability_mode(&self) -> Durability {
        self.durability
    }

    /// Closes the database's connections (SQLite: the file is released;
    /// Postgres: the pools and lock connections). Later calls fail with
    /// `storage_unavailable`. Close after the last run; in-memory storage
    /// has nothing to close. Dropping the last clone also closes, but at a
    /// time the runtime chooses.
    pub async fn close(&self) {
        if let Some(backend) = &self.backend {
            backend.close().await;
        }
    }

    /// The checkpointer.
    pub fn checkpointer(&self) -> &Arc<dyn Checkpointer> {
        &self.checkpointer
    }

    /// The long-term memory store.
    pub fn memory(&self) -> &Arc<dyn MemoryStore> {
        &self.memory
    }

    /// Takes the thread for one operation, following
    /// [`on_busy`](Self::on_busy). Released when the lease is dropped.
    ///
    /// First in this process (shared by clones of this `Storage`, FIFO),
    /// then, with [`ThreadLocks`], across processes; `busy_timeout` covers
    /// both. Without thread locks, [`Checkpointer::commit`]'s
    /// compare-and-set still keeps another process from forking a thread.
    pub async fn lease(&self, thread: &str) -> Result<ThreadLease, StorageError> {
        let started = criogen_rt::Instant::now();
        let mut lease = self.local_lease(thread).await?;
        if let Some(locks) = &self.locks {
            let slot = lease.slot.as_ref().expect("a new lease has its slot");
            lease.remote = Some(self.remote_lock(locks, thread, slot, started).await?);
        }
        Ok(lease)
    }

    /// The thread's lock in other processes, after the local lease.
    async fn remote_lock(
        &self,
        locks: &Arc<dyn ThreadLocks>,
        thread: &str,
        slot: &Arc<Slot>,
        started: criogen_rt::Instant,
    ) -> Result<HeldLock, StorageError> {
        // Another process interrupts: cancel this lease's run, even if it
        // registers its token only later. Weak: the lease table forgets
        // slots nobody holds.
        let weak = Arc::downgrade(slot);
        let generation = slot.generation();
        let on_interrupt: InterruptHandler = Arc::new(move || {
            if let Some(slot) = weak.upgrade() {
                slot.interrupt(generation);
            }
        });
        let remaining = || self.busy_timeout.saturating_sub(started.elapsed());
        let held = match self.on_busy {
            OnBusy::Reject => locks.lock(thread, None, on_interrupt).await?,
            OnBusy::Wait => locks.lock(thread, Some(remaining()), on_interrupt).await?,
            OnBusy::Interrupt => match locks.lock(thread, None, on_interrupt.clone()).await? {
                Some(held) => Some(held),
                None => {
                    locks.interrupt(thread).await?;
                    locks.lock(thread, Some(remaining()), on_interrupt).await?
                }
            },
        };
        held.ok_or_else(|| match self.on_busy {
            OnBusy::Reject => busy(thread, "another process runs it"),
            _ => busy(
                thread,
                format!(
                    "another run (in another process) is still active after {} ms",
                    self.busy_timeout.as_millis()
                ),
            ),
        })
    }

    /// The lease in this process.
    async fn local_lease(&self, thread: &str) -> Result<ThreadLease, StorageError> {
        let slot = self.leases.slot(thread);
        let lock = slot.lock.clone();
        // Read before trying: if the lock is taken, its holder's lease has
        // this generation for as long as it holds it.
        let generation = slot.generation();
        let guard = match (lock.clone().try_lock_owned().ok(), self.on_busy) {
            (Some(guard), _) => guard,
            (None, OnBusy::Reject) => {
                drop(slot);
                self.leases.release(thread);
                return Err(busy(thread, "another run is active"));
            }
            (None, on_busy) => {
                if on_busy == OnBusy::Interrupt {
                    slot.interrupt(generation);
                }
                match criogen_rt::timeout(self.busy_timeout, lock.lock_owned()).await {
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
            remote: None,
            slot: Some(slot),
            leases: self.leases.clone(),
        })
    }

    /// Cancels the run that holds `thread` in this process, if any. `true`
    /// if one was running. (A run in another process is stopped by a lease
    /// with [`OnBusy::Interrupt`] on storage with [`ThreadLocks`].)
    pub fn cancel(&self, thread: &str) -> bool {
        let slot = self.leases.existing(thread);
        let token = slot
            .as_ref()
            .and_then(|slot| lock_slot(&slot.run).token.clone());
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

/// Exclusive use of a thread, from [`Storage::lease`]: in this process, and
/// across processes with [`ThreadLocks`].
pub struct ThreadLease {
    thread: String,
    guard: Option<OwnedMutexGuard<()>>,
    remote: Option<HeldLock>,
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
            let mut run = lock_slot(&slot.run);
            // Interrupted before the run registered: stop it at once.
            if run.interrupted {
                token.cancel();
            }
            run.token = Some(token);
        }
    }

    /// Whether this lease's run was asked to stop ([`Storage::cancel`] or
    /// [`OnBusy::Interrupt`]), including before it registered a token with
    /// [`set_cancel`](Self::set_cancel). Work done between taking the lease
    /// and registering can check it.
    pub fn interrupted(&self) -> bool {
        self.slot.as_ref().is_some_and(|slot| {
            let run = lock_slot(&slot.run);
            run.interrupted || run.token.as_ref().is_some_and(CancelToken::is_cancelled)
        })
    }
}

impl Drop for ThreadLease {
    fn drop(&mut self) {
        if let Some(slot) = &self.slot {
            let mut run = lock_slot(&slot.run);
            run.token = None;
            run.interrupted = false;
            // Late interrupts meant for this lease are ignored.
            run.generation += 1;
        }
        // Other processes first: a local waiter then finds it free sooner.
        self.remote = None;
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
    run: Mutex<Run>,
}

/// The run of the lease that holds a slot.
#[derive(Debug, Default)]
struct Run {
    /// Its cancel token, once registered.
    token: Option<CancelToken>,
    /// Counts leases: one more each time a lease ends.
    generation: u64,
    /// Interrupted before it registered its token.
    interrupted: bool,
}

impl Slot {
    fn generation(&self) -> u64 {
        lock_slot(&self.run).generation
    }

    /// Interrupts the run of the lease with `generation`, if it still
    /// holds the slot: now, or when it registers its token.
    fn interrupt(&self, generation: u64) {
        let mut run = lock_slot(&self.run);
        if run.generation == generation {
            match &run.token {
                Some(token) => token.cancel(),
                None => run.interrupted = true,
            }
        }
    }
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
