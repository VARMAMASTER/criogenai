//! The browser runtime (design 0021), run as WebAssembly on a JavaScript
//! event loop: `CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=wasm-bindgen-test-runner
//! cargo test -p criogen-rt --target wasm32-unknown-unknown` (Node), or in
//! browsers through scripts/test-web.sh. Real timers: the claims are timed.
#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use criogen_rt::{Instant, JoinSet, sleep, sleep_until, spawn, system_now, timeout, yield_now};
use wasm_bindgen_test::wasm_bindgen_test;

/// Sets its flag when dropped: shows a task's future was let go.
struct DropFlag(Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[wasm_bindgen_test]
async fn a_task_returns_its_output() {
    let handle = spawn(async { 40 + 2 });
    assert_eq!(handle.await.unwrap(), 42);
}

#[wasm_bindgen_test]
async fn tasks_run_concurrently() {
    // Three 300 ms waits take about 300 ms together; one after another they
    // would take 900.
    let started = Instant::now();
    let mut tasks = JoinSet::new();
    for n in 0..3 {
        tasks.spawn(async move {
            sleep(ms(300)).await;
            n
        });
    }
    let mut seen = 0;
    while let Some(done) = tasks.join_next().await {
        done.unwrap();
        seen += 1;
    }
    assert_eq!(seen, 3);
    let took = started.elapsed();
    assert!(took >= ms(300), "finished before the timers: {took:?}");
    assert!(took < ms(700), "not concurrent: {took:?}");
}

#[wasm_bindgen_test]
async fn a_join_set_joins_in_the_order_tasks_finish() {
    let mut tasks = JoinSet::new();
    tasks.spawn(async {
        sleep(ms(120)).await;
        "slow"
    });
    tasks.spawn(async {
        sleep(ms(10)).await;
        "fast"
    });
    assert_eq!(tasks.join_next().await.unwrap().unwrap(), "fast");
    assert_eq!(tasks.join_next().await.unwrap().unwrap(), "slow");
    assert!(tasks.join_next().await.is_none());
}

#[wasm_bindgen_test]
async fn abort_stops_a_task_and_drops_its_future() {
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(dropped.clone());
    let started = Instant::now();
    let handle = spawn(async move {
        let _flag = flag;
        sleep(Duration::from_secs(10)).await;
        "never"
    });
    sleep(ms(20)).await;
    handle.abort();
    let error = handle.await.unwrap_err();
    assert!(error.is_cancelled());
    assert!(!error.is_panic());
    assert!(dropped.load(Ordering::SeqCst), "the future was not dropped");
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[wasm_bindgen_test]
async fn abort_before_the_first_poll() {
    let ran = Arc::new(AtomicBool::new(false));
    let seen = ran.clone();
    let handle = spawn(async move {
        seen.store(true, Ordering::SeqCst);
    });
    handle.abort();
    assert!(handle.await.unwrap_err().is_cancelled());
    assert!(!ran.load(Ordering::SeqCst), "an aborted task ran");
}

#[wasm_bindgen_test]
async fn a_join_error_names_its_task() {
    let mut tasks: JoinSet<()> = JoinSet::new();
    let abort = tasks.spawn(async { sleep(Duration::from_secs(10)).await });
    abort.abort();
    let error = tasks.join_next().await.unwrap().unwrap_err();
    assert_eq!(error.id(), abort.id());
    assert!(abort.is_finished());
}

#[wasm_bindgen_test]
async fn dropping_a_join_set_aborts_its_tasks() {
    let dropped = Arc::new(AtomicBool::new(false));
    let flag = DropFlag(dropped.clone());
    let mut tasks = JoinSet::new();
    tasks.spawn(async move {
        let _flag = flag;
        sleep(Duration::from_secs(10)).await;
    });
    sleep(ms(10)).await;
    drop(tasks);
    // The task drops its future at its next turn.
    for _ in 0..5 {
        yield_now().await;
    }
    sleep(ms(10)).await;
    assert!(dropped.load(Ordering::SeqCst), "the task kept running");
}

#[wasm_bindgen_test]
async fn a_dropped_handle_detaches_the_task() {
    let finished = Arc::new(AtomicBool::new(false));
    let mark = finished.clone();
    drop(spawn(async move {
        sleep(ms(20)).await;
        mark.store(true, Ordering::SeqCst);
    }));
    sleep(ms(80)).await;
    assert!(finished.load(Ordering::SeqCst), "detached task did not run");
}

#[wasm_bindgen_test]
async fn sleep_never_ends_early() {
    for wait in [1, 15, 40] {
        let started = Instant::now();
        sleep(ms(wait)).await;
        assert!(
            started.elapsed() >= ms(wait),
            "a {wait} ms sleep ended early"
        );
    }
}

#[wasm_bindgen_test]
async fn sleep_until_a_past_instant_is_immediate() {
    let past = Instant::now();
    sleep(ms(5)).await;
    let started = Instant::now();
    sleep_until(past).await;
    assert!(started.elapsed() < ms(50));
}

#[wasm_bindgen_test]
async fn timeout_fails_when_the_time_runs_out() {
    let started = Instant::now();
    let outcome = timeout(ms(50), std::future::pending::<()>()).await;
    assert!(outcome.is_err());
    assert!(started.elapsed() >= ms(50));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[wasm_bindgen_test]
async fn timeout_lets_a_faster_future_win() {
    let started = Instant::now();
    let outcome = timeout(Duration::from_secs(5), async {
        sleep(ms(10)).await;
        7
    })
    .await;
    assert_eq!(outcome.unwrap(), 7);
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[wasm_bindgen_test]
async fn many_timers_fire_once_each() {
    let fired = Arc::new(AtomicUsize::new(0));
    let mut tasks = JoinSet::new();
    for n in 0..50u64 {
        let fired = fired.clone();
        tasks.spawn(async move {
            sleep(ms(n % 7)).await;
            fired.fetch_add(1, Ordering::SeqCst);
        });
    }
    while tasks.join_next().await.is_some() {}
    assert_eq!(fired.load(Ordering::SeqCst), 50);
}

#[wasm_bindgen_test]
fn the_clock_never_goes_back() {
    let mut last = Instant::now();
    for _ in 0..1000 {
        let now = Instant::now();
        assert!(now >= last);
        last = now;
    }
    // Arithmetic saturates instead of panicking.
    let far = last + Duration::MAX;
    assert!(far > last);
    assert_eq!(last - far, Duration::ZERO);
}

#[wasm_bindgen_test]
fn the_wall_clock_is_plausible() {
    let since_epoch = system_now().duration_since(UNIX_EPOCH).unwrap();
    // After 2024-01-01.
    assert!(since_epoch > Duration::from_secs(1_704_067_200));
}
