use std::time::Duration;

use agentbuild_core::BoxFuture;
use sqlx::postgres::{PgListener, PgPool};
use tokio::sync::oneshot;
use tokio::time::Instant;

use super::sql_error;
use crate::error::{StorageError, StorageErrorKind};
use crate::locks::{HeldLock, InterruptHandler, ThreadLocks};

/// The channel interrupts are sent on; the payload is the thread's lock key.
const INTERRUPT_CHANNEL: &str = "agentbuild_interrupt";

/// A thread's lock key, computed by the server so every process (any
/// language, any build) agrees on it.
const LOCK_KEY: &str = "hashtextextended('agentbuild/thread/' || $1, 0)";

/// Thread locks for every process on one Postgres database (design 0008
/// §5.2): session advisory locks, so a crashed process's locks go with its
/// connections. From [`PostgresCheckpointer::thread_locks`](crate::PostgresCheckpointer::thread_locks);
/// [`Storage::connect`](crate::Storage::connect) adds them.
///
/// Each held or awaited lock uses one connection (see
/// [`PostgresOptions::lock_connections`](crate::PostgresOptions::lock_connections)),
/// on which it also listens for interrupts (`LISTEN agentbuild_interrupt`).
/// Waiting is Postgres's lock queue, so waiters get the thread in arrival
/// order.
#[derive(Clone)]
pub struct PostgresThreadLocks {
    pool: PgPool,
}

impl std::fmt::Debug for PostgresThreadLocks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PostgresThreadLocks")
            .finish_non_exhaustive()
    }
}

impl PostgresThreadLocks {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn take(
        &self,
        thread: &str,
        wait: Option<Duration>,
        on_interrupt: InterruptHandler,
    ) -> Result<Option<HeldLock>, StorageError> {
        let (ready, outcome) = oneshot::channel();
        let (release, released) = oneshot::channel::<()>();
        // A task owns the connection from start to end, so the lock is
        // released (or never kept) even if this future is dropped while it
        // waits.
        tokio::spawn(hold(
            self.pool.clone(),
            thread.to_string(),
            wait.map(|wait| Instant::now() + wait),
            on_interrupt,
            ready,
            released,
        ));
        match outcome.await {
            // Dropping the sender ends the task, which unlocks.
            Ok(Ok(true)) => Ok(Some(HeldLock::new(release))),
            Ok(Ok(false)) => Ok(None),
            Ok(Err(err)) => Err(err),
            Err(_) => Err(StorageError::new(
                StorageErrorKind::Unavailable,
                "Postgres: the thread lock task ended unexpectedly",
            )),
        }
    }

    async fn notify(&self, thread: &str) -> Result<(), StorageError> {
        sqlx::query(&format!("SELECT pg_notify($2, ({LOCK_KEY})::TEXT)"))
            .bind(thread)
            .bind(INTERRUPT_CHANNEL)
            .execute(&self.pool)
            .await
            .map_err(sql_error)?;
        Ok(())
    }
}

/// Takes the lock on its own connection, reports to `ready`, then holds it
/// (cancelling the run on interrupts) until `released` fires or its sender
/// is dropped.
async fn hold(
    pool: PgPool,
    thread: String,
    deadline: Option<Instant>,
    on_interrupt: InterruptHandler,
    ready: oneshot::Sender<Result<bool, StorageError>>,
    mut released: oneshot::Receiver<()>,
) {
    let connect = PgListener::connect_with(&pool);
    let listener = match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, connect)
            .await
            .unwrap_or_else(|_| Err(sqlx::Error::PoolTimedOut)),
        None => connect.await,
    };
    let mut listener = match listener {
        Ok(listener) => listener,
        Err(err) => {
            let _ = ready.send(Err(sql_error(err)));
            return;
        }
    };
    let key = match lock(&mut listener, &thread, deadline).await {
        Ok(Some(key)) => key,
        Ok(None) => {
            let _ = ready.send(Ok(false));
            return;
        }
        Err(err) => {
            let _ = ready.send(Err(err));
            return;
        }
    };
    // Listen before reporting, so no interrupt is missed.
    if let Err(err) = listener.listen(INTERRUPT_CHANNEL).await {
        unlock(&mut listener).await;
        let _ = ready.send(Err(sql_error(err)));
        return;
    }
    if ready.send(Ok(true)).is_err() {
        // The caller stopped waiting.
        unlock(&mut listener).await;
        return;
    }
    let wanted = key.to_string();
    loop {
        tokio::select! {
            _ = &mut released => break,
            notice = listener.try_recv() => match notice {
                Ok(Some(notice)) if notice.payload() == wanted => on_interrupt(),
                Ok(Some(_)) => {}
                // The connection is gone, and the lock with it. Commits
                // stay compare-and-set, so the thread still cannot fork.
                Ok(None) | Err(_) => return,
            },
        }
    }
    unlock(&mut listener).await;
}

/// Takes the advisory lock: once (`deadline = None`), or waiting until
/// `deadline`. The key if taken.
async fn lock(
    listener: &mut PgListener,
    thread: &str,
    deadline: Option<Instant>,
) -> Result<Option<i64>, StorageError> {
    let key: i64 = sqlx::query_scalar(&format!("SELECT {LOCK_KEY}"))
        .bind(thread)
        .fetch_one(&mut *listener)
        .await
        .map_err(sql_error)?;
    let Some(deadline) = deadline else {
        let taken: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
            .bind(key)
            .fetch_one(&mut *listener)
            .await
            .map_err(sql_error)?;
        return Ok(taken.then_some(key));
    };
    // The server ends the wait (`lock_timeout`; 0 would mean forever), so
    // the connection is never dropped mid-query.
    let left = deadline.saturating_duration_since(Instant::now());
    sqlx::query("SELECT set_config('lock_timeout', $1, false)")
        .bind(format!("{}ms", left.as_millis().max(1)))
        .execute(&mut *listener)
        .await
        .map_err(sql_error)?;
    let taken = sqlx::query("SELECT pg_advisory_lock($1)")
        .bind(key)
        .execute(&mut *listener)
        .await;
    let reset = sqlx::query("SELECT set_config('lock_timeout', '0', false)")
        .execute(&mut *listener)
        .await;
    match (taken, reset) {
        (Ok(_), Ok(_)) => Ok(Some(key)),
        (Err(err), Ok(_)) if is_lock_timeout(&err) => Ok(None),
        (Err(err), _) | (_, Err(err)) => {
            unlock(listener).await;
            Err(sql_error(err))
        }
    }
}

/// Releases every advisory lock of the connection (it only ever holds
/// one), before it goes back to the pool.
async fn unlock(listener: &mut PgListener) {
    let _ = sqlx::query("SELECT pg_advisory_unlock_all()")
        .execute(&mut *listener)
        .await;
}

/// `lock_not_available`: `lock_timeout` ran out.
fn is_lock_timeout(err: &sqlx::Error) -> bool {
    matches!(err, sqlx::Error::Database(db) if db.code().as_deref() == Some("55P03"))
}

impl ThreadLocks for PostgresThreadLocks {
    fn lock<'a>(
        &'a self,
        thread: &'a str,
        wait: Option<Duration>,
        on_interrupt: InterruptHandler,
    ) -> BoxFuture<'a, Result<Option<HeldLock>, StorageError>> {
        Box::pin(self.take(thread, wait, on_interrupt))
    }

    fn interrupt<'a>(&'a self, thread: &'a str) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(self.notify(thread))
    }
}
