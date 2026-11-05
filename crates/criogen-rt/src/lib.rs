//! Tasks and timers for the Criogen engine, on every platform it runs on
//! (design 0021).
//!
//! On servers this crate *is* Tokio: [`spawn`], [`JoinSet`], [`sleep`],
//! [`timeout`] and [`Instant`] are re-exported from it, so behaviour (and the
//! paused clock of `#[tokio::test(start_paused = true)]`) is unchanged.
//!
//! In the browser, and in any other JavaScript host that loads the engine as
//! WebAssembly (`wasm32-unknown-unknown`), there is no Tokio runtime: tasks
//! run on the JavaScript event loop, timers are `setTimeout` and the clock is
//! `performance.now()`. The API is the part of Tokio the engine uses, with the
//! same behaviour, so engine code is written once:
//!
//! ```
//! # async fn demo() {
//! use std::time::Duration;
//!
//! let mut tasks = criogen_rt::JoinSet::new();
//! tasks.spawn(async { 1 });
//! tasks.spawn(async {
//!     criogen_rt::sleep(Duration::from_millis(5)).await;
//!     2
//! });
//! let mut sum = 0;
//! while let Some(done) = tasks.join_next().await {
//!     sum += done.unwrap();
//! }
//! assert_eq!(sum, 3);
//! let late = criogen_rt::timeout(Duration::from_millis(1), std::future::pending::<()>());
//! assert!(late.await.is_err());
//! # }
//! # #[cfg(not(target_arch = "wasm32"))]
//! # tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap().block_on(demo());
//! ```
//!
//! Differences in the browser, by necessity:
//! - A panic ends the WebAssembly instance (`panic = abort` is the only
//!   strategy there), so a [`JoinError`] only ever means "cancelled".
//! - Everything runs on one thread: tasks are concurrent, never parallel.
//! - Timers follow the host's: a background tab may fire them late (never
//!   early; a timer that fires early is re-armed).

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod native {
    use std::time::SystemTime;

    pub use tokio::task::{AbortHandle, Id, JoinError, JoinHandle, JoinSet, spawn, yield_now};
    pub use tokio::time::error::Elapsed;
    pub use tokio::time::{Instant, Sleep, sleep, sleep_until, timeout, timeout_at};

    /// The real monotonic clock, even under Tokio's paused test clock: for
    /// durations reported to people (how long a tool call took).
    pub use std::time::Instant as RealInstant;

    /// The wall clock (`SystemTime::now`), for timestamps.
    pub fn system_now() -> SystemTime {
        SystemTime::now()
    }
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use native::*;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use web::*;

/// `true` when the engine runs as WebAssembly in a JavaScript host (the
/// browser, Node, Deno, an edge runtime) rather than natively.
pub const IN_JAVASCRIPT_HOST: bool = cfg!(all(target_arch = "wasm32", target_os = "unknown"));
