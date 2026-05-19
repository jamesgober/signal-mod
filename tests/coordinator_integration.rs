//! Integration tests for the public [`Coordinator`] surface.
//!
//! These tests intentionally avoid touching real OS signal handlers
//! (which would interfere with the test process); they exercise the
//! programmatic shutdown path that signal handlers eventually feed.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, Signal, SignalSet};

#[test]
fn token_clones_share_state() {
    let coord = Coordinator::builder().build();
    let t1 = coord.token();
    let t2 = t1.clone();

    assert!(!t1.is_initiated());
    assert!(!t2.is_initiated());

    coord.trigger().trigger(ShutdownReason::Requested);

    assert!(t1.is_initiated());
    assert!(t2.is_initiated());
    assert_eq!(t1.reason(), Some(ShutdownReason::Requested));
    assert_eq!(t2.reason(), Some(ShutdownReason::Requested));
}

#[test]
fn trigger_clones_are_idempotent() {
    let coord = Coordinator::builder().build();
    let trig1 = coord.trigger();
    let trig2 = trig1.clone();

    assert!(trig1.trigger(ShutdownReason::Signal(Signal::Terminate)));
    assert!(!trig2.trigger(ShutdownReason::Requested));
    assert!(!trig1.trigger(ShutdownReason::Forced));

    let stats = coord.statistics();
    assert!(stats.initiated);
    assert_eq!(
        stats.reason,
        Some(ShutdownReason::Signal(Signal::Terminate))
    );
}

#[test]
fn hooks_observe_reason() {
    let observed = Arc::new(parking_lot::Mutex::new(None));
    let o = Arc::clone(&observed);

    let coord = Coordinator::builder()
        .hook(hook_from_fn("observe", 0, move |reason| {
            *o.lock() = Some(reason);
        }))
        .build();

    coord.run_hooks(ShutdownReason::Signal(Signal::Hangup));
    assert_eq!(
        *observed.lock(),
        Some(ShutdownReason::Signal(Signal::Hangup))
    );
}

#[test]
fn statistics_track_hook_completion() {
    let counter = Arc::new(AtomicUsize::new(0));
    let c1 = Arc::clone(&counter);
    let c2 = Arc::clone(&counter);

    let coord = Coordinator::builder()
        .hook(hook_from_fn("h1", 10, move |_| {
            c1.fetch_add(1, Ordering::Relaxed);
        }))
        .hook(hook_from_fn("h2", 5, move |_| {
            c2.fetch_add(1, Ordering::Relaxed);
        }))
        .build();

    let stats_before = coord.statistics();
    assert_eq!(stats_before.hooks_registered, 2);
    assert_eq!(stats_before.hooks_completed, 0);

    coord.run_hooks(ShutdownReason::Requested);

    let stats_after = coord.statistics();
    assert_eq!(stats_after.hooks_registered, 2);
    assert_eq!(stats_after.hooks_completed, 2);
    assert_eq!(counter.load(Ordering::Relaxed), 2);
}

#[test]
fn signal_set_iteration_includes_only_enabled() {
    let mut seen = Vec::new();
    for sig in SignalSet::graceful() {
        seen.push(sig);
    }
    assert_eq!(
        seen,
        vec![Signal::Terminate, Signal::Interrupt, Signal::Hangup]
    );
}

#[test]
fn install_without_runtime_returns_no_runtime() {
    #[cfg(not(any(feature = "tokio", feature = "async-std", feature = "ctrlc-fallback")))]
    {
        let coord = Coordinator::builder().build();
        let err = coord.install().unwrap_err();
        assert!(matches!(err, signal_mod::Error::NoRuntime));
    }
}

#[test]
fn wait_blocking_timeout_short_returns_false() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let observed = token.wait_blocking_timeout(Duration::from_millis(5));
    assert!(!observed);
}
