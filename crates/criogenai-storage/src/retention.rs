//! Retention (design 0008 §15.1): idle threads expire, long threads are
//! shortened, expired memory is removed.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::checkpoint::{ThreadQuery, now_ms};
use crate::error::{StorageError, StorageErrorKind};
use crate::storage::{OnBusy, Storage};

/// How long stored data is kept. Applied by [`Storage::prune`] and
/// [`Storage::spawn_pruner`]; nothing is deleted until one of them runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Retention {
    thread_ttl: Option<Duration>,
    keep: Option<Keep>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Keep {
    /// The head's branch, whole.
    Branch,
    /// The head and its `n - 1` ancestors.
    Last(usize),
}

impl Retention {
    /// Keep everything (the default).
    pub fn new() -> Self {
        Self::default()
    }

    /// Deletes threads whose last commit is `ttl` or more ago, like
    /// `thread.delete()`. Threads waiting for approval count as idle.
    pub fn thread_ttl(mut self, ttl: Duration) -> Self {
        self.thread_ttl = Some(ttl);
        self
    }

    /// Keeps the head of each thread and its `n - 1` ancestors (at least
    /// the head); older checkpoints and abandoned branches go. History then
    /// starts at the oldest kept checkpoint.
    pub fn keep_checkpoints(mut self, n: usize) -> Self {
        self.keep = Some(Keep::Last(n.max(1)));
        self
    }

    /// Deletes abandoned branches (left by `rewind`) and keeps the head's
    /// branch whole. Replaced by [`keep_checkpoints`](Self::keep_checkpoints),
    /// which also drops them.
    pub fn drop_branches(mut self) -> Self {
        if self.keep.is_none() {
            self.keep = Some(Keep::Branch);
        }
        self
    }

    /// The thread time to live, if set.
    pub fn ttl(&self) -> Option<Duration> {
        self.thread_ttl
    }

    /// What [`Checkpointer::prune_thread`](crate::Checkpointer::prune_thread)
    /// is called with, if checkpoints are pruned at all.
    fn keep(&self) -> Option<Option<usize>> {
        self.keep.map(|keep| match keep {
            Keep::Branch => None,
            Keep::Last(n) => Some(n),
        })
    }
}

/// What a [`Storage::prune`] pass removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct PruneReport {
    /// Idle threads deleted.
    pub threads_deleted: u64,
    /// Expired threads skipped because a run held them.
    pub threads_busy: u64,
    /// Checkpoints deleted from threads that were kept.
    pub checkpoints_deleted: u64,
    /// Expired memory items removed.
    pub memory_items_deleted: u64,
}

/// Threads read per page.
const PAGE: usize = 100;

/// How far back a background pass looks for threads committed since the
/// previous pass: clocks of different processes may disagree (0008 P12).
const CLOCK_MARGIN_MS: u64 = 10 * 60 * 1000;

impl Storage {
    /// Sets how long data is kept (design 0008 §15.1). Applied by
    /// [`prune`](Self::prune) and [`spawn_pruner`](Self::spawn_pruner).
    pub fn retention(mut self, retention: Retention) -> Self {
        self.retention = retention;
        self
    }

    /// The [`retention`](Self::retention) rules.
    pub fn retention_rules(&self) -> Retention {
        self.retention
    }

    /// Applies the retention rules once: deletes idle threads (skipping
    /// those a run holds), shortens the others, removes expired memory.
    /// For a cron job or a serverless function; see also
    /// [`spawn_pruner`](Self::spawn_pruner).
    pub async fn prune(&self) -> Result<PruneReport, StorageError> {
        self.prune_pass(None).await
    }

    /// Records a pass that removed something (design 0010 §2).
    async fn record_pass(&self, report: &PruneReport) -> Result<(), StorageError> {
        if *report == PruneReport::default() {
            return Ok(());
        }
        self.record(
            crate::audit::AuditEvent::new("storage.pruned")
                .outcome("ok")
                .detail("threads_deleted", report.threads_deleted)
                .detail("threads_busy", report.threads_busy)
                .detail("checkpoints_deleted", report.checkpoints_deleted)
                .detail("memory_items_deleted", report.memory_items_deleted),
        )
        .await
    }

    /// Runs [`prune`](Self::prune) now and then every `every`, in a task
    /// (Tokio's on servers), until the returned [`Pruner`] is dropped. After the first pass
    /// only threads committed since the previous one are shortened.
    pub fn spawn_pruner(&self, every: Duration) -> Pruner {
        let storage = self.clone();
        let last = Arc::new(Mutex::new(None));
        let report = last.clone();
        let task = criogenai_rt::spawn(async move {
            let mut since = None;
            loop {
                let started = now_ms();
                let result = storage.prune_pass(since).await;
                if result.is_ok() {
                    since = Some(started.saturating_sub(CLOCK_MARGIN_MS));
                }
                *report.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                criogenai_rt::sleep(every).await;
            }
        });
        Pruner { task, last }
    }

    /// One pass. `since`: shorten only threads committed at or after it.
    async fn prune_pass(&self, since: Option<u64>) -> Result<PruneReport, StorageError> {
        let mut report = PruneReport::default();
        if let Some(ttl) = self.retention.thread_ttl {
            self.expire_threads(ttl, &mut report).await?;
        }
        if let Some(keep) = self.retention.keep() {
            let mut query = ThreadQuery::new().limit(PAGE);
            query.updated_after = since;
            loop {
                let page = self.checkpointer.threads(&query).await?;
                for thread in &page {
                    report.checkpoints_deleted += self
                        .checkpointer
                        .prune_thread(&thread.thread_id, keep)
                        .await?;
                }
                match page.last() {
                    Some(last) if page.len() == PAGE => query.after = Some(last.thread_id.clone()),
                    _ => break,
                }
            }
        }
        report.memory_items_deleted = self.memory.prune_expired().await?;
        self.record_pass(&report).await?;
        Ok(report)
    }

    /// Deletes threads idle for `ttl` or longer.
    async fn expire_threads(
        &self,
        ttl: Duration,
        report: &mut PruneReport,
    ) -> Result<(), StorageError> {
        let ttl_ms = ttl.as_millis().min(u128::from(u64::MAX)) as u64;
        // Idle: last commit + ttl <= now, so before `now - ttl + 1`.
        let cutoff = now_ms().saturating_sub(ttl_ms).saturating_add(1);
        let rejecting = self.clone().on_busy(OnBusy::Reject);
        let mut query = ThreadQuery::new().updated_before(cutoff).limit(PAGE);
        loop {
            let page = self.checkpointer.threads(&query).await?;
            for thread in &page {
                let lease = match rejecting.lease(&thread.thread_id).await {
                    Ok(lease) => lease,
                    Err(err) if err.kind == StorageErrorKind::ThreadBusy => {
                        report.threads_busy += 1;
                        continue;
                    }
                    Err(err) => return Err(err),
                };
                // Used since it was listed: keep it.
                let head = self.checkpointer.history(&thread.thread_id, 1).await?;
                if head
                    .first()
                    .is_some_and(|head| head.version == thread.version)
                    && self.checkpointer.delete_thread(&thread.thread_id).await?
                {
                    report.threads_deleted += 1;
                }
                drop(lease);
            }
            match page.last() {
                Some(last) if page.len() == PAGE => query.after = Some(last.thread_id.clone()),
                _ => return Ok(()),
            }
        }
    }
}

/// The background task of [`Storage::spawn_pruner`]. Dropping it stops the
/// task.
#[derive(Debug)]
pub struct Pruner {
    task: criogenai_rt::JoinHandle<()>,
    last: Arc<Mutex<Option<Result<PruneReport, StorageError>>>>,
}

impl Pruner {
    /// The latest pass's report or error, once a pass has finished.
    pub fn last(&self) -> Option<Result<PruneReport, StorageError>> {
        self.last.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Stops the task (the same as dropping the handle).
    pub fn stop(self) {}
}

impl Drop for Pruner {
    fn drop(&mut self) {
        self.task.abort();
    }
}
