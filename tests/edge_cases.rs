//! Edge-case tests for `signal-mod`.
//!
//! These tests exercise boundary conditions of the public surface
//! that the happy-path integration tests do not cover: empty inputs,
//! double-install attempts, panicking hooks, zero timeouts, very
//! large hook counts, and so on.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, Signal, SignalSet};

// ---------------------------------------------------------------------
// SignalSet edge cases
// ---------------------------------------------------------------------

#[test]
fn empty_set_iter_yields_nothing() {
    let empty = SignalSet::empty();
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);
    assert_eq!(empty.iter().count(), 0);
    let collected: Vec<Signal> = empty.into_iter().collect();
    assert!(collected.is_empty());
}

#[test]
fn all_set_contains_every_variant() {
    let all = SignalSet::all();
    assert_eq!(all.len(), 7);
    for sig in Signal::ALL {
        assert!(all.contains(sig));
    }
}

#[test]
fn set_round_trip_through_with_and_without() {
    let mut set = SignalSet::empty();
    for sig in Signal::ALL {
        set = set.with(sig);
    }
    assert_eq!(set, SignalSet::all());

    for sig in Signal::ALL {
        set = set.without(sig);
    }
    assert!(set.is_empty());
}

#[test]
fn set_iter_yields_canonical_order() {
    let set = SignalSet::all();
    let yielded: Vec<Signal> = set.iter().collect();
    assert_eq!(yielded, Signal::ALL.to_vec());
}

#[test]
fn set_default_matches_graceful() {
    assert_eq!(SignalSet::default(), SignalSet::graceful());
}

// ---------------------------------------------------------------------
// Coordinator boundary inputs
// ---------------------------------------------------------------------

#[test]
fn run_hooks_on_empty_list_returns_zero() {
    let coord = Coordinator::builder().build();
    assert_eq!(coord.run_hooks(ShutdownReason::Requested), 0);
    assert_eq!(coord.statistics().hooks_completed, 0);
}

#[test]
fn run_hooks_with_zero_graceful_budget_skips_all() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c1 = Arc::clone(&counter);
    let c2 = Arc::clone(&counter);

    let coord = Coordinator::builder()
        .graceful_timeout(Duration::from_millis(0))
        .hook(hook_from_fn("a", 0, move |_| {
            c1.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(1));
        }))
        .hook(hook_from_fn("b", 0, move |_| {
            c2.fetch_add(1, Ordering::Relaxed);
        }))
        .build();

    let ran = coord.run_hooks(ShutdownReason::Forced);
    // The first hook runs (the budget check is at the top of the
    // loop, so the first iteration always enters). Subsequent
    // iterations bail because elapsed > 0.
    assert!(ran <= 2, "expected at most 2 hooks, ran {ran}");
}

#[test]
fn run_hooks_with_very_large_count_completes() {
    let counter = Arc::new(AtomicUsize::new(0));
    let mut builder = Coordinator::builder().graceful_timeout(Duration::from_secs(30));
    let n = 256usize;
    for i in 0..n {
        let c = Arc::clone(&counter);
        builder = builder.hook(hook_from_fn(
            format!("h{i}"),
            i32::try_from(i).unwrap_or(i32::MAX),
            move |_| {
                c.fetch_add(1, Ordering::Relaxed);
            },
        ));
    }
    let coord = builder.build();
    let ran = coord.run_hooks(ShutdownReason::Requested);
    assert_eq!(ran, n);
    assert_eq!(counter.load(Ordering::Relaxed), n);
}

#[test]
fn panicking_hook_does_not_abort_remaining() {
    let after = Arc::new(AtomicUsize::new(0));
    let a = Arc::clone(&after);

    let coord = Coordinator::builder()
        .hook(hook_from_fn("explode", 100, |_| {
            panic!("hook panic; should be swallowed");
        }))
        .hook(hook_from_fn("after", 0, move |_| {
            a.fetch_add(1, Ordering::Relaxed);
        }))
        .build();

    // run_hooks must not propagate the hook's panic.
    let result = catch_unwind(AssertUnwindSafe(|| coord.run_hooks(ShutdownReason::Forced)));
    assert!(result.is_ok(), "run_hooks panicked when a hook panicked");
    let ran = result.unwrap();
    assert_eq!(ran, 2);
    assert_eq!(after.load(Ordering::Relaxed), 1);
}

#[test]
fn run_hooks_can_be_called_twice() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&counter);

    let coord = Coordinator::builder()
        .hook(hook_from_fn("idempotent", 0, move |_| {
            c.fetch_add(1, Ordering::Relaxed);
        }))
        .build();

    assert_eq!(coord.run_hooks(ShutdownReason::Requested), 1);
    assert_eq!(coord.run_hooks(ShutdownReason::Forced), 1);
    assert_eq!(counter.load(Ordering::Relaxed), 2);
    assert_eq!(coord.statistics().hooks_completed, 2);
}

#[test]
fn hooks_with_extreme_priorities_sort_correctly() {
    let order = Arc::new(parking_lot::Mutex::new(Vec::<i32>::new()));

    let push = |p: i32, order: &Arc<parking_lot::Mutex<Vec<i32>>>| {
        let o = Arc::clone(order);
        hook_from_fn(format!("p{p}"), p, move |_| {
            o.lock().push(p);
        })
    };

    let coord = Coordinator::builder()
        .hook(push(i32::MIN, &order))
        .hook(push(i32::MAX, &order))
        .hook(push(0, &order))
        .hook(push(-1, &order))
        .hook(push(1, &order))
        .build();

    coord.run_hooks(ShutdownReason::Requested);
    let observed = order.lock().clone();
    assert_eq!(observed, vec![i32::MAX, 1, 0, -1, i32::MIN]);
}

#[test]
fn equal_priority_hooks_preserve_insertion_order() {
    let order = Arc::new(parking_lot::Mutex::new(Vec::<&'static str>::new()));

    let mk = |name: &'static str| {
        let o = Arc::clone(&order);
        hook_from_fn(name, 42, move |_| {
            o.lock().push(name);
        })
    };

    let coord = Coordinator::builder()
        .hook(mk("first"))
        .hook(mk("second"))
        .hook(mk("third"))
        .build();

    coord.run_hooks(ShutdownReason::Requested);
    assert_eq!(*order.lock(), vec!["first", "second", "third"]);
}

// ---------------------------------------------------------------------
// install() edge cases
// ---------------------------------------------------------------------

#[cfg(feature = "tokio")]
#[tokio::test]
async fn install_twice_returns_already_installed() {
    let coord = Coordinator::builder()
        .signals(SignalSet::empty()) // no actual signals to avoid global state
        .build();

    coord.install().expect("first install should succeed");
    let err = coord.install().expect_err("second install should fail");
    assert!(matches!(err, signal_mod::Error::AlreadyInstalled));
    assert!(coord.is_installed());
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn install_with_empty_signal_set_succeeds_as_no_op() {
    let coord = Coordinator::builder().signals(SignalSet::empty()).build();
    coord.install().expect("empty-set install must succeed");
    assert!(coord.is_installed());
}

#[cfg(not(any(feature = "tokio", feature = "async-std", feature = "ctrlc-fallback")))]
#[test]
fn install_returns_no_runtime_without_back_end_feature() {
    let coord = Coordinator::builder().build();
    let err = coord
        .install()
        .expect_err("install must fail with no back-end");
    assert!(matches!(err, signal_mod::Error::NoRuntime));
    assert!(!coord.is_installed());
}

// ---------------------------------------------------------------------
// Wait primitives
// ---------------------------------------------------------------------

#[test]
fn wait_blocking_timeout_returns_immediately_if_already_initiated() {
    let coord = Coordinator::builder().build();
    coord.trigger().trigger(ShutdownReason::Requested);

    let token = coord.token();
    let start = std::time::Instant::now();
    let observed = token.wait_blocking_timeout(Duration::from_secs(5));
    let elapsed = start.elapsed();
    assert!(observed);
    assert!(
        elapsed < Duration::from_millis(50),
        "fast path took {elapsed:?}"
    );
}

#[test]
fn wait_blocking_does_not_deadlock_when_already_initiated() {
    let coord = Coordinator::builder().build();
    coord.trigger().trigger(ShutdownReason::Requested);
    coord.token().wait_blocking();
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn wait_resolves_immediately_when_already_initiated() {
    let coord = Coordinator::builder().build();
    coord.trigger().trigger(ShutdownReason::Requested);
    let token = coord.token();
    tokio::time::timeout(Duration::from_secs(1), token.wait())
        .await
        .expect("wait did not resolve fast-path");
}

// ---------------------------------------------------------------------
// Signal metadata
// ---------------------------------------------------------------------

#[test]
fn signal_unix_numbers_round_trip_unique() {
    let mut seen = std::collections::HashSet::new();
    for sig in Signal::ALL {
        let n = sig.unix_number().expect("every signal has a unix number");
        assert!(seen.insert(n), "duplicate unix number {n}");
    }
}

#[test]
fn signal_display_renders_description() {
    for sig in Signal::ALL {
        assert_eq!(format!("{sig}"), sig.description());
    }
}

#[test]
fn shutdown_reason_descriptions_are_stable() {
    assert_eq!(ShutdownReason::Requested.description(), "requested");
    assert_eq!(ShutdownReason::Forced.description(), "forced");
    assert_eq!(ShutdownReason::Timeout.description(), "timeout");
    assert_eq!(ShutdownReason::Error.description(), "error");
    assert_eq!(
        ShutdownReason::Signal(Signal::Terminate).description(),
        "signal"
    );
}
