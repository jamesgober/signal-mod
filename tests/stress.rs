//! Stress / concurrency tests for `signal-mod`.
//!
//! These tests exercise the state machine under contention and at
//! scale. They run in CI but are kept small enough to complete in
//! well under a second per case.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, ShutdownToken};

const SHORT_DURATION: Duration = Duration::from_secs(2);

#[test]
fn many_concurrent_triggers_yield_single_initiator() {
    let coord = Coordinator::builder().build();
    let trigger = coord.trigger();
    let success_count = Arc::new(AtomicUsize::new(0));

    let threads = 32;
    let handles: Vec<_> = (0..threads)
        .map(|i| {
            let t = trigger.clone();
            let s = Arc::clone(&success_count);
            thread::spawn(move || {
                let reason = if i % 2 == 0 {
                    ShutdownReason::Requested
                } else {
                    ShutdownReason::Forced
                };
                if t.trigger(reason) {
                    s.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        success_count.load(Ordering::Relaxed),
        1,
        "exactly one triggerer should win the CAS"
    );
    assert!(coord.token().is_initiated());
}

#[test]
fn many_observers_wake_on_single_trigger() {
    let coord = Coordinator::builder().build();
    let token = coord.token();

    let waiters = 16;
    let woken = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..waiters)
        .map(|_| {
            let t: ShutdownToken = token.clone();
            let w = Arc::clone(&woken);
            thread::spawn(move || {
                if t.wait_blocking_timeout(SHORT_DURATION) {
                    w.fetch_add(1, Ordering::Relaxed);
                }
            })
        })
        .collect();

    // Give the threads a moment to enter their wait.
    thread::sleep(Duration::from_millis(50));
    coord.trigger().trigger(ShutdownReason::Requested);

    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(woken.load(Ordering::Relaxed), waiters);
}

#[test]
fn high_volume_clones_are_cheap_and_consistent() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let trigger = coord.trigger();

    let n = 10_000;
    let tokens: Vec<_> = (0..n).map(|_| token.clone()).collect();
    let triggers: Vec<_> = (0..n).map(|_| trigger.clone()).collect();

    // All clones see the not-initiated state.
    assert!(tokens.iter().all(|t| !t.is_initiated()));
    assert!(triggers.iter().all(|t| !t.is_initiated()));

    triggers[0].trigger(ShutdownReason::Requested);

    // All clones now see the initiated state.
    assert!(tokens.iter().all(ShutdownToken::is_initiated));
    assert!(triggers
        .iter()
        .all(signal_mod::ShutdownTrigger::is_initiated));
}

#[test]
fn rapid_trigger_then_observe_is_consistent() {
    for _ in 0..256 {
        let coord = Coordinator::builder().build();
        let trigger = coord.trigger();
        let token = coord.token();

        let handle = thread::spawn(move || {
            trigger.trigger(ShutdownReason::Signal(signal_mod::Signal::Terminate))
        });

        let observed_first = handle.join().unwrap();
        let observed_after = token.reason();

        if observed_first {
            assert_eq!(
                observed_after,
                Some(ShutdownReason::Signal(signal_mod::Signal::Terminate))
            );
        }
    }
}

#[test]
fn statistics_are_consistent_under_concurrent_hook_runs() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut builder = Coordinator::builder().graceful_timeout(Duration::from_secs(5));
    for i in 0..16 {
        let c = Arc::clone(&counter);
        builder = builder.hook(hook_from_fn(format!("h{i}"), 0, move |_| {
            c.fetch_add(1, Ordering::Relaxed);
        }));
    }
    let coord = Arc::new(builder.build());

    let threads = 8;
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let c = Arc::clone(&coord);
            thread::spawn(move || c.run_hooks(ShutdownReason::Requested))
        })
        .collect();

    let mut total = 0;
    for h in handles {
        total += h.join().unwrap();
    }

    // Each thread sees the same 16-hook list under the mutex, so we
    // expect 8 * 16 cumulative invocations.
    assert_eq!(total, threads * 16);
    assert_eq!(counter.load(Ordering::Relaxed), threads * 16);
    let stats = coord.statistics();
    assert_eq!(stats.hooks_completed, threads * 16);
}

#[test]
fn wait_blocking_timeout_under_contention_stays_responsive() {
    let coord = Coordinator::builder().build();
    let trigger = coord.trigger();
    let token = coord.token();

    let triggerer = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        trigger.trigger(ShutdownReason::Requested);
    });

    let start = Instant::now();
    let observed = token.wait_blocking_timeout(SHORT_DURATION);
    let elapsed = start.elapsed();

    triggerer.join().unwrap();
    assert!(observed, "expected to observe shutdown within budget");
    assert!(elapsed < Duration::from_secs(1), "wake-up took {elapsed:?}");
}
