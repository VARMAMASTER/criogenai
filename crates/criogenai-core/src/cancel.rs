//! Cooperative cancellation (design 0005 §4.3).

use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use tokio::sync::Notify;

/// A cancellation signal shared between whoever cancels and the work being
/// cancelled. Cloning is cheap; clones share the same signal.
///
/// Cancelling a token also cancels every token created from it with
/// [`child`](Self::child), but not the other way round.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    cancelled: AtomicBool,
    notify: Notify,
    children: Mutex<Vec<Weak<Inner>>>,
}

impl CancelToken {
    /// A token that has not been cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels this token and its children. Calling it again has no effect.
    pub fn cancel(&self) {
        cancel(&self.inner);
    }

    /// `true` once [`cancel`](Self::cancel) was called on this token or an
    /// ancestor.
    pub fn is_cancelled(&self) -> bool {
        self.inner.cancelled.load(Ordering::SeqCst)
    }

    /// Completes when the token is cancelled (immediately if it already is).
    pub async fn cancelled(&self) {
        loop {
            let mut notified = pin!(self.inner.notify.notified());
            // Register before checking the flag, so a `cancel` in between is
            // not missed.
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// A new token that is cancelled when this one is, and can also be
    /// cancelled on its own.
    pub fn child(&self) -> CancelToken {
        let child = CancelToken::new();
        {
            let mut children = lock(&self.inner.children);
            children.retain(|c| c.strong_count() > 0);
            children.push(Arc::downgrade(&child.inner));
        }
        // Checked after registering, so a concurrent `cancel` reaches the
        // child one way or the other.
        if self.is_cancelled() {
            child.cancel();
        }
        child
    }
}

fn cancel(inner: &Inner) {
    if inner.cancelled.swap(true, Ordering::SeqCst) {
        return;
    }
    inner.notify.notify_waiters();
    let children = std::mem::take(&mut *lock(&inner.children));
    for child in children.iter().filter_map(Weak::upgrade) {
        cancel(&child);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // The protected list stays valid even if a holder panicked.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancel_wakes_waiters_and_children() {
        let parent = CancelToken::new();
        let child = parent.child();
        let grandchild = child.child();
        let waiter = tokio::spawn({
            let grandchild = grandchild.clone();
            async move { grandchild.cancelled().await }
        });
        tokio::task::yield_now().await;
        assert!(!grandchild.is_cancelled());

        parent.cancel();
        waiter.await.unwrap();
        assert!(child.is_cancelled() && grandchild.is_cancelled());
        // Already cancelled: returns at once.
        grandchild.cancelled().await;
    }

    #[test]
    fn child_does_not_cancel_parent() {
        let parent = CancelToken::new();
        let child = parent.child();
        child.cancel();
        assert!(child.is_cancelled());
        assert!(!parent.is_cancelled());
    }

    #[test]
    fn child_of_cancelled_token_starts_cancelled() {
        let parent = CancelToken::new();
        parent.cancel();
        assert!(parent.child().is_cancelled());
    }

    #[test]
    fn dropped_children_are_pruned() {
        let parent = CancelToken::new();
        for _ in 0..100 {
            drop(parent.child());
        }
        let _kept = parent.child();
        assert_eq!(lock(&parent.inner.children).len(), 1);
    }
}
