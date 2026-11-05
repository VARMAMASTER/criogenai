//! The engine's tasks and timers in a JavaScript host (design 0021): tasks on
//! the event loop, `setTimeout`, `performance.now()`. Mirrors the part of
//! Tokio's API the engine uses (`native` re-exports Tokio itself).

use std::fmt;
use std::future::{Future, poll_fn};
use std::ops;
use std::pin::{Pin, pin};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::*;

/// Wraps a JavaScript value (never `Send`) so it can live in the engine's
/// `Send` futures. WebAssembly runs on the host's one thread, so the value
/// never moves to another; `SendWrapper` checks that and panics if it did.
pub use send_wrapper::SendWrapper;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = performance, js_name = now, catch)]
    fn performance_now() -> Result<f64, JsValue>;
    #[wasm_bindgen(js_name = setTimeout, catch)]
    fn set_timeout(handler: &js_sys::Function, milliseconds: i32) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = clearTimeout, catch)]
    fn clear_timeout(id: &JsValue) -> Result<(), JsValue>;
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic aborts the instance, so a poisoned lock cannot be observed.
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------- the clock ----------

/// The latest reading of the clock, in nanoseconds: [`Instant::now`] never
/// goes back, even on the `Date.now()` fallback.
static LAST_NANOS: AtomicU64 = AtomicU64::new(0);

/// Milliseconds on the host's monotonic clock (`performance.now()`), or on
/// the wall clock where a host has no `performance`.
fn now_millis() -> f64 {
    match performance_now() {
        Ok(ms) if ms.is_finite() && ms >= 0.0 => ms,
        _ => js_sys::Date::now(),
    }
}

/// A point on the monotonic clock, like `tokio::time::Instant`. Arithmetic
/// saturates instead of panicking: in the browser a panic would end the
/// WebAssembly instance.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instant(Duration);

impl Instant {
    /// Now.
    pub fn now() -> Self {
        // `as` saturates: NaN and negative readings become 0.
        let reading = (now_millis() * 1_000_000.0) as u64;
        let previous = LAST_NANOS.fetch_max(reading, Ordering::Relaxed);
        Self(Duration::from_nanos(reading.max(previous)))
    }

    /// The time from `earlier` to `self` (zero if `earlier` is later).
    pub fn duration_since(&self, earlier: Instant) -> Duration {
        self.saturating_duration_since(earlier)
    }

    /// The time from `earlier` to `self`, or `None` if `earlier` is later.
    pub fn checked_duration_since(&self, earlier: Instant) -> Option<Duration> {
        self.0.checked_sub(earlier.0)
    }

    /// The time from `earlier` to `self` (zero if `earlier` is later).
    pub fn saturating_duration_since(&self, earlier: Instant) -> Duration {
        self.0.saturating_sub(earlier.0)
    }

    /// The time since `self`.
    pub fn elapsed(&self) -> Duration {
        Instant::now().saturating_duration_since(*self)
    }

    /// `self + duration`, or `None` if that cannot be represented.
    pub fn checked_add(&self, duration: Duration) -> Option<Instant> {
        self.0.checked_add(duration).map(Instant)
    }

    /// `self - duration`, or `None` before the clock's origin.
    pub fn checked_sub(&self, duration: Duration) -> Option<Instant> {
        self.0.checked_sub(duration).map(Instant)
    }
}

impl fmt::Debug for Instant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Instant({:?} since the time origin)", self.0)
    }
}

impl ops::Add<Duration> for Instant {
    type Output = Instant;

    fn add(self, rhs: Duration) -> Instant {
        Instant(self.0.saturating_add(rhs))
    }
}

impl ops::AddAssign<Duration> for Instant {
    fn add_assign(&mut self, rhs: Duration) {
        *self = *self + rhs;
    }
}

impl ops::Sub<Duration> for Instant {
    type Output = Instant;

    fn sub(self, rhs: Duration) -> Instant {
        Instant(self.0.saturating_sub(rhs))
    }
}

impl ops::SubAssign<Duration> for Instant {
    fn sub_assign(&mut self, rhs: Duration) {
        *self = *self - rhs;
    }
}

impl ops::Sub<Instant> for Instant {
    type Output = Duration;

    fn sub(self, rhs: Instant) -> Duration {
        self.saturating_duration_since(rhs)
    }
}

/// The real monotonic clock (on servers `std::time::Instant`, unaffected
/// by Tokio's paused test clock); here the same as [`Instant`].
pub type RealInstant = Instant;

/// The wall clock (`Date.now()`), for timestamps. `SystemTime::now` panics
/// in this target.
pub fn system_now() -> SystemTime {
    let millis = js_sys::Date::now();
    let millis = if millis.is_finite() && millis > 0.0 {
        millis
    } else {
        0.0
    };
    // `as` saturates; whole milliseconds are enough for timestamps.
    UNIX_EPOCH + Duration::from_millis(millis as u64)
}

// ---------- timers ----------

/// The longest `setTimeout` (about 24.8 days); longer waits re-arm.
const MAX_TIMER_MS: u64 = i32::MAX as u64;

/// Whole milliseconds to wait for `left`, rounded up (a timer never fires
/// before its deadline) and capped at what `setTimeout` takes.
fn timer_millis(left: Duration) -> i32 {
    let ms = left.as_nanos().div_ceil(1_000_000);
    i32::try_from(ms.min(u128::from(MAX_TIMER_MS))).unwrap_or(i32::MAX)
}

#[derive(Default)]
struct TimerShared {
    fired: bool,
    waker: Option<Waker>,
}

/// One `setTimeout`, cleared when dropped before it fires.
struct Timer {
    id: SendWrapper<JsValue>,
    /// Keeps the JavaScript callback alive until the timer fires or is cleared.
    _callback: SendWrapper<Closure<dyn FnMut()>>,
    shared: Arc<Mutex<TimerShared>>,
}

impl Timer {
    fn start(left: Duration, waker: Waker) -> Timer {
        let shared = Arc::new(Mutex::new(TimerShared {
            fired: false,
            waker: Some(waker),
        }));
        let fire = shared.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            let waker = {
                let mut shared = lock(&fire);
                shared.fired = true;
                shared.waker.take()
            };
            if let Some(waker) = waker {
                waker.wake();
            }
        });
        let id = set_timeout(callback.as_ref().unchecked_ref(), timer_millis(left))
            .expect("setTimeout is not available in this JavaScript host");
        Timer {
            id: SendWrapper::new(id),
            _callback: SendWrapper::new(callback),
            shared,
        }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if !lock(&self.shared).fired {
            let _ = clear_timeout(&self.id);
        }
    }
}

/// A future that completes at its deadline, like `tokio::time::Sleep`. The
/// timer starts on the first poll and is cleared when the future is dropped.
pub struct Sleep {
    deadline: Instant,
    timer: Option<Timer>,
}

impl Sleep {
    /// When this sleep ends.
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Whether the deadline has passed.
    pub fn is_elapsed(&self) -> bool {
        Instant::now() >= self.deadline
    }

    /// Moves the deadline (the timer restarts on the next poll).
    pub fn reset(&mut self, deadline: Instant) {
        self.deadline = deadline;
        self.timer = None;
    }
}

impl fmt::Debug for Sleep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sleep")
            .field("deadline", &self.deadline)
            .field("armed", &self.timer.is_some())
            .finish()
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        let now = Instant::now();
        if now >= this.deadline {
            this.timer = None;
            return Poll::Ready(());
        }
        if let Some(timer) = &this.timer {
            let mut shared = lock(&timer.shared);
            if !shared.fired {
                // Still waiting: wake whoever polled last.
                if !shared
                    .waker
                    .as_ref()
                    .is_some_and(|w| w.will_wake(cx.waker()))
                {
                    shared.waker = Some(cx.waker().clone());
                }
                return Poll::Pending;
            }
        }
        // Not started, or fired before the deadline (a wait longer than
        // `setTimeout` allows): arm a timer for what is left.
        this.timer = Some(Timer::start(
            this.deadline.saturating_duration_since(now),
            cx.waker().clone(),
        ));
        Poll::Pending
    }
}

/// Waits for `duration`.
pub fn sleep(duration: Duration) -> Sleep {
    sleep_until(Instant::now() + duration)
}

/// Waits until `deadline` (at once if it has passed).
pub fn sleep_until(deadline: Instant) -> Sleep {
    Sleep {
        deadline,
        timer: None,
    }
}

/// The error of [`timeout`] and [`timeout_at`]: the time ran out first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Elapsed(());

impl fmt::Display for Elapsed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("deadline has elapsed")
    }
}

impl std::error::Error for Elapsed {}

/// Runs `future`, or fails with [`Elapsed`] once `duration` has passed. The
/// future is polled first, so one that is ready at the deadline still wins.
pub async fn timeout<F: Future>(duration: Duration, future: F) -> Result<F::Output, Elapsed> {
    timeout_at(Instant::now() + duration, future).await
}

/// Runs `future`, or fails with [`Elapsed`] at `deadline`.
pub async fn timeout_at<F: Future>(deadline: Instant, future: F) -> Result<F::Output, Elapsed> {
    let mut future = pin!(future);
    let mut timer = sleep_until(deadline);
    poll_fn(|cx| {
        if let Poll::Ready(output) = future.as_mut().poll(cx) {
            return Poll::Ready(Ok(output));
        }
        Pin::new(&mut timer).poll(cx).map(|()| Err(Elapsed(())))
    })
    .await
}

// ---------- tasks ----------

static NEXT_TASK: AtomicU64 = AtomicU64::new(1);

/// A task's id, unique for the life of the instance (like `tokio::task::Id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Id(u64);

impl fmt::Display for Id {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Why a task has no output: it was aborted. (A panic ends the whole
/// WebAssembly instance, so it never shows up here.)
pub struct JoinError {
    id: Id,
}

impl JoinError {
    /// Always true here: the task was aborted.
    pub fn is_cancelled(&self) -> bool {
        true
    }

    /// Always false here: a panic aborts the instance instead.
    pub fn is_panic(&self) -> bool {
        false
    }

    /// The panic payload. Never one here: this panics, as Tokio's does
    /// for an error that is not a panic.
    pub fn into_panic(self) -> Box<dyn std::any::Any + Send + 'static> {
        panic!("task {} was cancelled, it did not panic", self.id)
    }

    /// The panic payload, or the error back (always, here).
    pub fn try_into_panic(self) -> Result<Box<dyn std::any::Any + Send + 'static>, JoinError> {
        Err(self)
    }

    /// The id of the task.
    pub fn id(&self) -> Id {
        self.id
    }
}

impl fmt::Debug for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JoinError::Cancelled({})", self.id)
    }
}

impl fmt::Display for JoinError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "task {} was cancelled", self.id)
    }
}

impl std::error::Error for JoinError {}

struct TaskState<T> {
    output: Option<Result<T, JoinError>>,
    finished: bool,
    aborted: bool,
    /// The task itself, woken to notice an abort.
    task_waker: Option<Waker>,
    /// Whoever waits for the output.
    join_waker: Option<Waker>,
}

struct Task<T> {
    id: Id,
    state: Mutex<TaskState<T>>,
}

impl<T> Task<T> {
    fn new() -> Self {
        Self {
            id: Id(NEXT_TASK.fetch_add(1, Ordering::Relaxed)),
            state: Mutex::new(TaskState {
                output: None,
                finished: false,
                aborted: false,
                task_waker: None,
                join_waker: None,
            }),
        }
    }

    fn complete(&self, output: Result<T, JoinError>) {
        let waker = {
            let mut state = lock(&self.state);
            state.output = Some(output);
            state.finished = true;
            state.task_waker = None;
            state.join_waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn abort_task(&self) {
        let waker = {
            let mut state = lock(&self.state);
            if state.finished {
                return;
            }
            state.aborted = true;
            state.task_waker.take()
        };
        // A task not polled yet sees the flag on its first poll.
        if let Some(waker) = waker {
            waker.wake();
        }
    }

    fn finished(&self) -> bool {
        lock(&self.state).finished
    }
}

/// What an [`AbortHandle`] needs of a task, whatever its output type.
trait Abortable: Send + Sync {
    fn abort(&self);
    fn is_finished(&self) -> bool;
    fn id(&self) -> Id;
}

impl<T: Send> Abortable for Task<T> {
    fn abort(&self) {
        self.abort_task();
    }

    fn is_finished(&self) -> bool {
        self.finished()
    }

    fn id(&self) -> Id {
        self.id
    }
}

/// Runs `future` as a task on the JavaScript event loop, like
/// `tokio::spawn`. The handle's output is the future's; dropping the handle
/// detaches the task (it keeps running), [`JoinHandle::abort`] stops it.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let task = Arc::new(Task::new());
    let runner = task.clone();
    wasm_bindgen_futures::spawn_local(async move {
        // The future is dropped at the end of this block, before the output
        // is handed over (an aborted task has let go of everything).
        let output = {
            let mut future = pin!(future);
            poll_fn(|cx| {
                {
                    let mut state = lock(&runner.state);
                    if state.aborted {
                        return Poll::Ready(None);
                    }
                    if !state
                        .task_waker
                        .as_ref()
                        .is_some_and(|w| w.will_wake(cx.waker()))
                    {
                        state.task_waker = Some(cx.waker().clone());
                    }
                }
                future.as_mut().poll(cx).map(Some)
            })
            .await
        };
        runner.complete(output.ok_or(JoinError { id: runner.id }));
    });
    JoinHandle { task }
}

/// Waits for one task's output, like `tokio::task::JoinHandle`.
pub struct JoinHandle<T> {
    task: Arc<Task<T>>,
}

impl<T> JoinHandle<T> {
    /// Stops the task: its future is dropped at its next turn, and the
    /// handle's output is a cancelled [`JoinError`]. No-op once it finished.
    pub fn abort(&self) {
        self.task.abort_task();
    }

    /// Whether the task has finished (or was aborted).
    pub fn is_finished(&self) -> bool {
        self.task.finished()
    }

    /// The task's id.
    pub fn id(&self) -> Id {
        self.task.id
    }
}

impl<T: Send + 'static> JoinHandle<T> {
    /// A handle that can abort the task without waiting for it.
    pub fn abort_handle(&self) -> AbortHandle {
        AbortHandle {
            task: self.task.clone(),
        }
    }
}

impl<T> fmt::Debug for JoinHandle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinHandle")
            .field("id", &self.task.id)
            .finish_non_exhaustive()
    }
}

impl<T> Future for JoinHandle<T> {
    type Output = Result<T, JoinError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = lock(&self.task.state);
        if let Some(output) = state.output.take() {
            return Poll::Ready(output);
        }
        assert!(!state.finished, "JoinHandle polled after completion");
        if !state
            .join_waker
            .as_ref()
            .is_some_and(|w| w.will_wake(cx.waker()))
        {
            state.join_waker = Some(cx.waker().clone());
        }
        Poll::Pending
    }
}

/// Aborts a task without waiting for it, like `tokio::task::AbortHandle`.
#[derive(Clone)]
pub struct AbortHandle {
    task: Arc<dyn Abortable>,
}

impl AbortHandle {
    /// Stops the task (see [`JoinHandle::abort`]).
    pub fn abort(&self) {
        self.task.abort();
    }

    /// Whether the task has finished (or was aborted).
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// The task's id.
    pub fn id(&self) -> Id {
        self.task.id()
    }
}

impl fmt::Debug for AbortHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AbortHandle")
            .field("id", &self.task.id())
            .finish_non_exhaustive()
    }
}

/// A set of tasks, joined in the order they finish, like
/// `tokio::task::JoinSet`. Dropping the set aborts the tasks still in it.
pub struct JoinSet<T> {
    tasks: Vec<JoinHandle<T>>,
}

impl<T: Send + 'static> JoinSet<T> {
    /// An empty set.
    pub fn new() -> Self {
        Self { tasks: Vec::new() }
    }

    /// Tasks not joined yet.
    pub fn len(&self) -> usize {
        self.tasks.len()
    }

    /// Whether every task has been joined.
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }

    /// Starts `future` as a task in this set.
    pub fn spawn<F>(&mut self, future: F) -> AbortHandle
    where
        F: Future<Output = T> + Send + 'static,
    {
        let handle = spawn(future);
        let abort = handle.abort_handle();
        self.tasks.push(handle);
        abort
    }

    /// The output of the next task to finish, or `None` when the set is
    /// empty. Cancel-safe: dropping the future loses no output.
    pub async fn join_next(&mut self) -> Option<Result<T, JoinError>> {
        poll_fn(|cx| self.poll_join_next(cx)).await
    }

    /// [`join_next`](Self::join_next) as a poll function.
    pub fn poll_join_next(&mut self, cx: &mut Context<'_>) -> Poll<Option<Result<T, JoinError>>> {
        if self.tasks.is_empty() {
            return Poll::Ready(None);
        }
        for index in 0..self.tasks.len() {
            if let Poll::Ready(output) = Pin::new(&mut self.tasks[index]).poll(cx) {
                self.tasks.swap_remove(index);
                return Poll::Ready(Some(output));
            }
        }
        Poll::Pending
    }

    /// Aborts every task; their outputs become cancelled [`JoinError`]s.
    pub fn abort_all(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }

    /// Aborts every task and waits until all have stopped.
    pub async fn shutdown(&mut self) {
        self.abort_all();
        while self.join_next().await.is_some() {}
    }

    /// Lets every task run on without the set (they are not aborted).
    pub fn detach_all(&mut self) {
        self.tasks.clear();
    }
}

impl<T: Send + 'static> Default for JoinSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> fmt::Debug for JoinSet<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinSet")
            .field("len", &self.tasks.len())
            .finish()
    }
}

impl<T> Drop for JoinSet<T> {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Lets other tasks run before this one continues.
pub async fn yield_now() {
    let mut yielded = false;
    poll_fn(|cx| {
        if yielded {
            return Poll::Ready(());
        }
        yielded = true;
        cx.waker().wake_by_ref();
        Poll::Pending
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn timer_millis_rounds_up_and_caps() {
        assert_eq!(timer_millis(Duration::ZERO), 0);
        assert_eq!(timer_millis(Duration::from_nanos(1)), 1);
        assert_eq!(timer_millis(Duration::from_micros(1500)), 2);
        assert_eq!(timer_millis(Duration::from_millis(250)), 250);
        assert_eq!(
            timer_millis(Duration::from_secs(60 * 60 * 24 * 365)),
            i32::MAX
        );
        assert_eq!(timer_millis(Duration::MAX), i32::MAX);
    }
}
