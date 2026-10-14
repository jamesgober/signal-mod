//! Hook execution: enforced deadlines, the force phase, run-once, panic
//! reporting, re-entrancy and async execution. Each test names the 1.0.0
//! audit finding it guards against where there is one.

#![cfg(not(loom))]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use signal_mod::{
    Coordinator, FnHook, HookPhase, HookStatus, ShutdownHook, ShutdownReason, hook_from_fn,
};

/// How long a "hung" hook sleeps. Deadline tests use budgets of a few
/// hundred milliseconds and only assert that the run returned well
/// before a hung hook would have, so they stay reliable on loaded CI
/// runners (SM2-002).
const HANG: Duration = Duration::from_secs(30);
const WELL_BEFORE_HANG: Duration = Duration::from_secs(15);
const BUDGET: Duration = Duration::from_millis(300);

fn hang(name: &str, priority: i32) -> FnHook<impl Fn(ShutdownReason) + Send + Sync + 'static> {
    hook_from_fn(name, priority, |_| thread::sleep(HANG))
}

fn counter_hook(name: &str, priority: i32, counter: &Arc<AtomicUsize>) -> impl ShutdownHook {
    let c = Arc::clone(counter);
    hook_from_fn(name, priority, move |_| {
        c.fetch_add(1, Ordering::SeqCst);
    })
}

// SM-002: 1.0.0 only checked the budget between hooks, so one hung hook
// ran unbounded (412 ms against a 50 ms budget in the audit).
#[test]
fn test_hung_hook_is_abandoned_at_graceful_deadline() {
    let coord = Coordinator::builder()
        .graceful_timeout(BUDGET)
        .force_timeout(BUDGET)
        .hook(hang("hang", 0))
        .build();
    let start = Instant::now();
    let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    assert!(start.elapsed() < WELL_BEFORE_HANG, "{:?}", start.elapsed());
    assert_eq!(report.timed_out(), 1);
    assert_eq!(report.outcomes()[0].status, HookStatus::TimedOut);
    assert_eq!(report.outcomes()[0].phase, Some(HookPhase::Graceful));
    assert!(report.outcomes()[0].duration.unwrap() >= BUDGET / 2);
}

// SM2-002: the clock starts when the first hook is about to start, so
// the first hook always starts in the graceful phase, however slowly the
// worker thread got scheduled.
#[test]
fn test_first_hook_always_starts_in_graceful_phase() {
    for _ in 0..20 {
        let coord = Coordinator::builder()
            .graceful_timeout(Duration::from_millis(1))
            .force_timeout(Duration::from_millis(1))
            .hook(hook_from_fn("first", 0, |_| {}))
            .build();
        let report = coord.run_hooks_with_report(ShutdownReason::Requested);
        assert_eq!(report.outcomes()[0].phase, Some(HookPhase::Graceful));
    }
}

#[test]
fn test_hooks_after_a_hung_hook_run_in_force_phase_with_forced_reason() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = Arc::clone(&seen);
    let coord = Coordinator::builder()
        .graceful_timeout(BUDGET)
        .force_timeout(Duration::from_secs(60))
        .hook(hang("hang", 10))
        .hook(hook_from_fn("flush", 0, move |reason| {
            s.lock().unwrap().push(reason);
        }))
        .build();
    let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    assert_eq!(report.timed_out(), 1);
    assert_eq!(report.completed(), 1);
    assert_eq!(report.forced(), 1);
    assert_eq!(report.outcomes()[1].phase, Some(HookPhase::Force));
    assert_eq!(*seen.lock().unwrap(), vec![ShutdownReason::Forced]);
    assert_eq!(report.reason(), ShutdownReason::Requested);
    assert!(report.elapsed() < WELL_BEFORE_HANG);
}

#[test]
fn test_hooks_beyond_force_deadline_are_skipped() {
    let counter = Arc::new(AtomicUsize::new(0));
    let coord = Coordinator::builder()
        .graceful_timeout(BUDGET)
        .force_timeout(BUDGET * 2)
        .hook(hang("hang-a", 2))
        .hook(hang("hang-b", 1))
        .hook(counter_hook("never", 0, &counter))
        .build();
    let start = Instant::now();
    let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    assert!(start.elapsed() < WELL_BEFORE_HANG);
    let statuses: Vec<HookStatus> = report.outcomes().iter().map(|o| o.status).collect();
    assert_eq!(statuses[0], HookStatus::TimedOut);
    // hang-b starts in the force phase and times out, or, on a machine so
    // loaded that the supervisor woke after the force deadline, is skipped.
    assert!(matches!(
        statuses[1],
        HookStatus::TimedOut | HookStatus::Skipped
    ));
    assert_eq!(statuses[2], HookStatus::Skipped);
    assert_eq!(report.outcomes()[2].phase, None);
    assert_eq!(counter.load(Ordering::SeqCst), 0);
    assert!(coord.statistics().hooks_skipped >= 1);
}

#[test]
fn test_per_hook_timeout_bounds_one_hook_and_the_next_still_runs_gracefully() {
    let counter = Arc::new(AtomicUsize::new(0));
    let coord = Coordinator::builder()
        .graceful_timeout(Duration::from_secs(60))
        .hook(hang("slow", 1).with_timeout(BUDGET))
        .hook(counter_hook("next", 0, &counter))
        .build();
    let start = Instant::now();
    let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    assert!(start.elapsed() < WELL_BEFORE_HANG);
    assert_eq!(report.outcomes()[0].status, HookStatus::TimedOut);
    assert_eq!(report.outcomes()[1].status, HookStatus::Completed);
    assert_eq!(report.outcomes()[1].phase, Some(HookPhase::Graceful));
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

// SM2-009: an abandoned hook can see that it was abandoned.
#[test]
fn test_abandoned_hook_observes_is_abandoned() {
    let observed = Arc::new(AtomicUsize::new(0));
    let o = Arc::clone(&observed);
    let coord = Coordinator::builder()
        .graceful_timeout(BUDGET)
        .force_timeout(BUDGET)
        .hook(hook_from_fn("cooperative", 0, move |_| {
            let ctx = signal_mod::HookContext::current().unwrap();
            assert!(ctx.deadline().is_some());
            let give_up = Instant::now() + HANG;
            while !ctx.is_abandoned() && Instant::now() < give_up {
                thread::sleep(Duration::from_millis(5));
            }
            if ctx.is_abandoned() {
                assert_eq!(ctx.remaining(), Some(Duration::ZERO));
                o.store(1, Ordering::SeqCst);
            }
        }))
        .build();
    let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    assert_eq!(report.timed_out(), 1);
    let deadline = Instant::now() + WELL_BEFORE_HANG;
    while observed.load(Ordering::SeqCst) == 0 {
        assert!(Instant::now() < deadline, "hook never saw the abandonment");
        thread::sleep(Duration::from_millis(5));
    }
}

// SM-020 / audit P3: 1.0.0's zero-budget test asserted nothing.
#[test]
fn test_zero_budgets_skip_every_hook_deterministically() {
    let counter = Arc::new(AtomicUsize::new(0));
    let coord = Coordinator::builder()
        .graceful_timeout(Duration::ZERO)
        .force_timeout(Duration::ZERO)
        .hook(counter_hook("a", 0, &counter))
        .hook(counter_hook("b", 0, &counter))
        .build();
    let report = coord.run_hooks_with_report(ShutdownReason::Forced);
    assert_eq!(report.skipped(), 2);
    assert_eq!(counter.load(Ordering::SeqCst), 0);
}

#[test]
fn test_zero_graceful_budget_runs_every_hook_in_force_phase() {
    let counter = Arc::new(AtomicUsize::new(0));
    let coord = Coordinator::builder()
        .graceful_timeout(Duration::ZERO)
        .force_timeout(Duration::from_secs(5))
        .hook(counter_hook("a", 0, &counter))
        .hook(counter_hook("b", 0, &counter))
        .build();
    let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    assert_eq!(report.completed(), 2);
    assert_eq!(report.forced(), 2);
}

// SM-003 / SM-017: 1.0.0 swallowed panics and counted the hook as
// completed.
#[test]
fn test_panicking_hook_is_reported_with_message_and_not_counted_completed() {
    let after = Arc::new(AtomicUsize::new(0));
    let coord = Coordinator::builder()
        .hook(hook_from_fn("explode", 100, |_| {
            panic!("hook panic {}", 42)
        }))
        .hook(counter_hook("after", 0, &after))
        .build();
    let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    assert_eq!(report.completed(), 1);
    assert_eq!(report.panicked(), 1);
    assert_eq!(
        report.outcomes()[0].panic_message.as_deref(),
        Some("hook panic 42")
    );
    assert_eq!(after.load(Ordering::SeqCst), 1);
    assert_eq!(coord.run_hooks(ShutdownReason::Requested), 1);
}

// SM-007: 1.0.0 re-ran every hook on every call.
#[test]
fn test_second_call_returns_same_report_without_rerunning_hooks() {
    let counter = Arc::new(AtomicUsize::new(0));
    let coord = Coordinator::builder()
        .hook(counter_hook("once", 0, &counter))
        .build();
    let first = coord.run_hooks_with_report(ShutdownReason::Requested);
    let second = coord.run_hooks_with_report(ShutdownReason::Forced);
    assert_eq!(first, second);
    assert_eq!(second.reason(), ShutdownReason::Requested);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert_eq!(coord.statistics().hooks_completed, 1);
}

#[test]
fn test_concurrent_callers_share_one_run() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut builder = Coordinator::builder().graceful_timeout(Duration::from_secs(60));
    for i in 0..16 {
        let c = Arc::clone(&counter);
        builder = builder.hook(hook_from_fn(format!("h{i}"), 0, move |_| {
            thread::sleep(Duration::from_millis(1));
            c.fetch_add(1, Ordering::SeqCst);
        }));
    }
    let coord = Arc::new(builder.build());
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let c = Arc::clone(&coord);
            thread::spawn(move || c.run_hooks_with_report(ShutdownReason::Requested))
        })
        .collect();
    let reports: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(counter.load(Ordering::SeqCst), 16);
    assert!(reports.windows(2).all(|w| w[0] == w[1]));
    assert_eq!(reports[0].completed(), 16);
}

// SM-005 / audit P1: a hook calling `statistics()` deadlocked forever in
// 1.0.0 because the hook mutex was held across hook bodies.
#[test]
fn test_hook_can_read_statistics_and_debug_of_its_coordinator() {
    let coord = Arc::new(Mutex::new(None::<Arc<Coordinator>>));
    let seen = Arc::new(Mutex::new(None));
    let (c, s) = (Arc::clone(&coord), Arc::clone(&seen));
    let built = Arc::new(
        Coordinator::builder()
            .graceful_timeout(Duration::from_secs(60))
            .hook(hook_from_fn("introspect", 0, move |_| {
                let guard = c.lock().unwrap();
                let me = guard.as_ref().unwrap();
                let stats = me.statistics();
                let _ = format!("{me:?}"); // must not deadlock either
                *s.lock().unwrap() = Some(stats.hooks[0].status);
            }))
            .build(),
    );
    *coord.lock().unwrap() = Some(Arc::clone(&built));
    let start = Instant::now();
    let report = built.run_hooks_with_report(ShutdownReason::Requested);
    assert!(start.elapsed() < WELL_BEFORE_HANG);
    assert!(report.is_clean());
    assert_eq!(*seen.lock().unwrap(), Some(HookStatus::Running));
    coord.lock().unwrap().take();
}

// Audit P5: `statistics()` blocked for the whole hook run in 1.0.0.
#[test]
fn test_statistics_does_not_block_while_a_hook_runs() {
    let coord = Arc::new(
        Coordinator::builder()
            .hook(hook_from_fn("slow", 0, |_| {
                thread::sleep(Duration::from_secs(2));
            }))
            .build(),
    );
    let runner = Arc::clone(&coord);
    let handle = thread::spawn(move || runner.run_hooks(ShutdownReason::Requested));
    // Wait until the hook is running.
    let deadline = Instant::now() + Duration::from_secs(5);
    while coord.statistics().hooks[0].status != HookStatus::Running {
        assert!(Instant::now() < deadline, "hook never started");
        thread::yield_now();
    }
    let start = Instant::now();
    let stats = coord.statistics();
    assert!(start.elapsed() < Duration::from_secs(1));
    assert_eq!(stats.hooks[0].status, HookStatus::Running);
    assert_eq!(handle.join().unwrap(), 1);
}

#[test]
fn test_hooks_run_in_priority_order_with_stable_ties() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let mk = |name: &'static str, p: i32| {
        let o = Arc::clone(&order);
        hook_from_fn(name, p, move |_| o.lock().unwrap().push(name))
    };
    let coord = Coordinator::builder()
        .hook(mk("min", i32::MIN))
        .hook(mk("tie-1", 7))
        .hook(mk("max", i32::MAX))
        .hook(mk("tie-2", 7))
        .hook(mk("zero", 0))
        .hook(mk("tie-3", 7))
        .build();
    assert_eq!(coord.run_hooks(ShutdownReason::Requested), 6);
    assert_eq!(
        *order.lock().unwrap(),
        vec!["max", "tie-1", "tie-2", "tie-3", "zero", "min"]
    );
}

#[test]
fn test_many_hooks_all_complete() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut builder = Coordinator::builder().graceful_timeout(Duration::from_secs(30));
    for i in 0..256 {
        builder = builder.hook(counter_hook(&format!("h{i}"), i, &counter));
    }
    let coord = builder.build();
    assert_eq!(coord.run_hooks(ShutdownReason::Requested), 256);
    assert_eq!(counter.load(Ordering::SeqCst), 256);
}

#[test]
fn test_hooks_run_on_a_worker_thread_not_the_callers() {
    let caller = thread::current().id();
    let seen = Arc::new(Mutex::new(None));
    let s = Arc::clone(&seen);
    let coord = Coordinator::builder()
        .hook(hook_from_fn("where", 0, move |_| {
            *s.lock().unwrap() = Some((
                thread::current().id(),
                thread::current().name().map(String::from),
            ));
        }))
        .build();
    coord.run_hooks(ShutdownReason::Requested);
    let (id, name) = seen.lock().unwrap().clone().unwrap();
    assert_ne!(id, caller);
    assert_eq!(name.as_deref(), Some("signal-mod-hook"));
}

#[test]
fn test_run_hooks_with_no_hooks_returns_empty_clean_report() {
    let coord = Coordinator::builder().build();
    let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    assert!(report.is_clean());
    assert_eq!(report.outcomes().len(), 0);
    assert_eq!(coord.run_hooks(ShutdownReason::Requested), 0);
}

// SM-003 / audit P13: in 1.0.0 the documented `Handle::block_on` bridge
// panicked inside `#[tokio::main]` and the panic was swallowed.
// Needs the `tokio` feature: that is what captures the caller's runtime.
#[cfg(feature = "tokio")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_async_run_lets_hooks_bridge_to_tokio_with_block_on() {
    let flushed = Arc::new(AtomicUsize::new(0));
    let f = Arc::clone(&flushed);
    let coord = Coordinator::builder()
        .hook(hook_from_fn("async-flush", 0, move |_| {
            let f = Arc::clone(&f);
            tokio::runtime::Handle::current().block_on(async move {
                tokio::time::sleep(Duration::from_millis(5)).await;
                f.fetch_add(1, Ordering::SeqCst);
            });
        }))
        .build();
    let report = coord.run_hooks_async(ShutdownReason::Requested).await;
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(flushed.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn test_async_run_does_not_block_a_current_thread_runtime() {
    let ticks = Arc::new(AtomicUsize::new(0));
    let t = Arc::clone(&ticks);
    let ticker = tokio::spawn(async move {
        for _ in 0..5 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            t.fetch_add(1, Ordering::SeqCst);
        }
    });
    let coord = Coordinator::builder()
        .hook(hook_from_fn("slow", 0, |_| {
            thread::sleep(Duration::from_secs(1));
        }))
        .build();
    let report = coord.run_hooks_async(ShutdownReason::Requested).await;
    assert!(report.is_clean());
    // The single runtime thread kept running the ticker while the hook ran.
    assert!(ticks.load(Ordering::SeqCst) >= 3, "executor was blocked");
    ticker.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_async_and_sync_callers_share_one_run() {
    let counter = Arc::new(AtomicUsize::new(0));
    let coord = Arc::new(
        Coordinator::builder()
            .hook({
                let c = Arc::clone(&counter);
                hook_from_fn("once", 0, move |_| {
                    thread::sleep(Duration::from_millis(30));
                    c.fetch_add(1, Ordering::SeqCst);
                })
            })
            .build(),
    );
    let fut = coord.run_hooks_async(ShutdownReason::Requested);
    let sync_coord = Arc::clone(&coord);
    let sync = tokio::task::spawn_blocking(move || {
        thread::sleep(Duration::from_millis(5));
        sync_coord.run_hooks_with_report(ShutdownReason::Forced)
    });
    let a = fut.await;
    let b = sync.await.unwrap();
    assert_eq!(a, b);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

#[test]
fn test_dropping_async_future_before_poll_does_not_start_run() {
    let counter = Arc::new(AtomicUsize::new(0));
    let coord = Coordinator::builder()
        .hook(counter_hook("h", 0, &counter))
        .build();
    drop(coord.run_hooks_async(ShutdownReason::Requested));
    assert_eq!(coord.statistics().hooks[0].status, HookStatus::Pending);
    assert_eq!(coord.run_hooks(ShutdownReason::Requested), 1);
}

#[test]
fn test_async_run_works_without_tokio_on_smol() {
    let counter = Arc::new(AtomicUsize::new(0));
    let coord = Coordinator::builder()
        .hook(counter_hook("h", 0, &counter))
        .build();
    let report = smol::block_on(coord.run_hooks_async(ShutdownReason::Requested));
    assert_eq!(report.completed(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_concurrent_async_callers_share_one_run_and_drop_is_safe() {
    let counter = Arc::new(AtomicUsize::new(0));
    let coord = Coordinator::builder()
        .hook({
            let c = Arc::clone(&counter);
            hook_from_fn("slow", 0, move |_| {
                thread::sleep(Duration::from_millis(50));
                c.fetch_add(1, Ordering::SeqCst);
            })
        })
        .build();
    let first = tokio::spawn(coord.run_hooks_async(ShutdownReason::Requested));
    tokio::time::sleep(Duration::from_millis(10)).await;
    // Joins the run in progress, then is dropped while pending.
    let abandoned = tokio::time::timeout(
        Duration::from_millis(1),
        coord.run_hooks_async(ShutdownReason::Forced),
    )
    .await;
    assert!(abandoned.is_err());
    let second = coord.run_hooks_async(ShutdownReason::Forced).await;
    assert_eq!(first.await.unwrap(), second);
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}
