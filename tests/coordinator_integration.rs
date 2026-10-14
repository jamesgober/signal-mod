//! Integration tests for the public `Coordinator` surface that do not
//! touch OS signal handlers (see `install.rs` and `real_signals.rs` for
//! those).

#![cfg(not(loom))]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use signal_mod::{
    Coordinator, HookPhase, HookStatus, ShutdownReason, Signal, SignalSet, hook_from_fn,
};

#[test]
fn test_token_clones_share_state() {
    let coord = Coordinator::builder().build();
    let t1 = coord.token();
    let t2 = t1.clone();
    assert!(!t1.is_initiated());
    assert!(!t2.is_initiated());

    assert!(coord.trigger().trigger(ShutdownReason::Requested));

    assert!(t1.is_initiated());
    assert!(t2.is_initiated());
    assert_eq!(t1.reason(), Some(ShutdownReason::Requested));
    assert_eq!(t2.reason(), Some(ShutdownReason::Requested));
}

#[test]
fn test_trigger_first_reason_wins_across_clones() {
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
fn test_hooks_observe_reason_passed_to_run_hooks() {
    let observed = Arc::new(Mutex::new(None));
    let o = Arc::clone(&observed);
    let coord = Coordinator::builder()
        .hook(hook_from_fn("observe", 0, move |reason| {
            *o.lock().unwrap() = Some(reason);
        }))
        .build();

    assert_eq!(coord.run_hooks(ShutdownReason::Signal(Signal::Hangup)), 1);
    assert_eq!(
        *observed.lock().unwrap(),
        Some(ShutdownReason::Signal(Signal::Hangup))
    );
}

#[test]
fn test_statistics_track_hook_completion_and_timings() {
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

    let before = coord.statistics();
    assert_eq!(before.hooks_registered, 2);
    assert_eq!(before.hooks_completed, 0);
    assert!(before.hooks.iter().all(|h| h.status == HookStatus::Pending));
    assert!(before.hooks.iter().all(|h| h.duration.is_none()));

    let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    assert!(report.is_clean());

    let after = coord.statistics();
    assert_eq!(after.hooks_registered, 2);
    assert_eq!(after.hooks_completed, 2);
    assert_eq!(after.hooks_panicked, 0);
    assert_eq!(after.hooks_timed_out, 0);
    assert_eq!(after.hooks_skipped, 0);
    assert_eq!(counter.load(Ordering::Relaxed), 2);
    assert_eq!(&*after.hooks[0].name, "h1");
    assert_eq!(after.hooks[0].phase, Some(HookPhase::Graceful));
    assert!(after.hooks.iter().all(|h| h.duration.is_some()));
    assert_eq!(after.hooks, report.outcomes().to_vec());
}

#[test]
fn test_signal_set_iteration_includes_only_enabled() {
    let seen: Vec<Signal> = SignalSet::graceful().into_iter().collect();
    assert_eq!(
        seen,
        vec![Signal::Terminate, Signal::Interrupt, Signal::Hangup]
    );
}

#[test]
fn test_wait_blocking_timeout_short_returns_false() {
    let coord = Coordinator::builder().build();
    assert!(
        !coord
            .token()
            .wait_blocking_timeout(Duration::from_millis(5))
    );
}

#[test]
fn test_wait_blocking_returns_after_trigger_from_other_thread() {
    let coord = Coordinator::builder().build();
    let trigger = coord.trigger();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        trigger.trigger(ShutdownReason::Requested)
    });
    coord.token().wait_blocking();
    assert!(handle.join().unwrap());
    assert!(coord.token().elapsed().is_some());
}

#[test]
fn test_simulated_shutdown_signal_initiates_and_reaches_subscribers() {
    let coord = Coordinator::builder().build();
    let mut sub = coord.subscribe();
    assert!(coord.simulate_signal(Signal::Interrupt));
    assert_eq!(
        coord.token().reason(),
        Some(ShutdownReason::Signal(Signal::Interrupt))
    );
    assert_eq!(sub.try_recv(), Some(Signal::Interrupt));
}

#[test]
fn test_notify_signal_reaches_subscribers_without_initiating() {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful() - Signal::Hangup)
        .notify_signals(SignalSet::empty().with(Signal::Hangup).with(Signal::User1))
        .build();
    let mut a = coord.subscribe();
    let mut b = coord.subscribe();
    assert!(coord.simulate_signal(Signal::Hangup));
    assert!(coord.simulate_signal(Signal::User1));
    assert!(!coord.token().is_initiated());
    for sub in [&mut a, &mut b] {
        assert_eq!(sub.try_recv(), Some(Signal::Hangup));
        assert_eq!(sub.try_recv(), Some(Signal::User1));
        assert_eq!(sub.try_recv(), None);
    }
}

#[test]
fn test_subscription_closes_when_coordinator_dropped() {
    let coord = Coordinator::builder()
        .notify_signals(SignalSet::from(Signal::User2))
        .build();
    let mut sub = coord.subscribe();
    assert!(!sub.is_closed());
    drop(coord);
    assert!(sub.is_closed());
    assert_eq!(sub.recv_blocking(), None);
}

#[test]
fn test_child_scope_follows_parent_and_guards_count_upwards() {
    let coord = Coordinator::builder().build();
    let (child_trigger, child) = coord.token().child();
    let guard = child.subsystem_guard();
    assert_eq!(coord.token().active_subsystems(), 1);
    assert_eq!(coord.statistics().active_subsystems, 1);

    assert!(child_trigger.trigger(ShutdownReason::Requested));
    assert!(!coord.token().is_initiated());

    let (_t, other) = coord.token().child();
    assert!(coord.trigger().trigger(ShutdownReason::Timeout));
    assert_eq!(other.reason(), Some(ShutdownReason::Timeout));
    // The first child was already initiated; it keeps its own reason.
    assert_eq!(child.reason(), Some(ShutdownReason::Requested));

    assert!(
        !coord
            .token()
            .wait_subsystems_blocking(Duration::from_millis(5))
    );
    drop(guard);
    assert!(
        coord
            .token()
            .wait_subsystems_blocking(Duration::from_millis(5))
    );
}

#[test]
fn test_reason_exit_codes_follow_convention() {
    assert_eq!(ShutdownReason::Signal(Signal::Interrupt).exit_code(), 130);
    assert_eq!(ShutdownReason::Signal(Signal::Terminate).exit_code(), 143);
    assert_eq!(ShutdownReason::Requested.exit_code(), 0);
    assert_eq!(ShutdownReason::Forced.exit_code(), 1);
}

#[test]
fn test_version_matches_manifest() {
    assert_eq!(signal_mod::VERSION, env!("CARGO_PKG_VERSION"));
}
