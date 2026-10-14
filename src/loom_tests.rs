//! Loom model checks of the shutdown state machine.
//!
//! Run with:
//!
//! ```text
//! RUSTFLAGS="--cfg loom" cargo test --lib --release --no-default-features --features std loom
//! ```
//!
//! Under `--cfg loom` the state machine (`state.rs`) uses loom's atomics,
//! mutex and condition variable (see `sync.rs`), so every interleaving of
//! the operations below is explored. The models drive it through the
//! public API, exactly as users do.

use loom::thread;

use crate::{Coordinator, ShutdownReason, Signal};

/// Initiation publishes the reason before the flag: no interleaving lets
/// an observer see `is_initiated()` without a reason (audit SM-004).
#[test]
fn loom_reason_visible_whenever_initiated() {
    loom::model(|| {
        let coord = Coordinator::builder().build();
        let trigger = coord.trigger();
        let token = coord.token();

        let t = thread::spawn(move || trigger.trigger(ShutdownReason::Signal(Signal::Terminate)));

        if token.is_initiated() {
            assert_eq!(
                token.reason(),
                Some(ShutdownReason::Signal(Signal::Terminate))
            );
            assert!(token.elapsed().is_some());
        }
        assert!(t.join().unwrap_or(false));
        assert!(token.is_initiated());
    });
}

/// Exactly one of two racing triggers wins, and the winner's reason is
/// the one observed.
#[test]
fn loom_racing_triggers_have_one_winner() {
    loom::model(|| {
        let coord = Coordinator::builder().build();
        let a = coord.trigger();
        let b = coord.trigger();
        let ta = thread::spawn(move || a.trigger(ShutdownReason::Requested));
        let won_b = b.trigger(ShutdownReason::Error);
        let won_a = ta.join().unwrap_or(false);
        assert!(won_a ^ won_b);
        let expected = if won_a {
            ShutdownReason::Requested
        } else {
            ShutdownReason::Error
        };
        assert_eq!(coord.token().reason(), Some(expected));
    });
}

/// A blocking waiter always wakes: no lost wake-up between its check and
/// its wait.
#[test]
fn loom_wait_blocking_never_misses_the_trigger() {
    loom::model(|| {
        let coord = Coordinator::builder().build();
        let trigger = coord.trigger();
        let token = coord.token();
        let t = thread::spawn(move || {
            assert!(trigger.trigger(ShutdownReason::Requested));
        });
        token.wait_blocking();
        assert!(token.is_initiated());
        assert!(t.join().is_ok());
    });
}

/// The runtime-agnostic future always completes: its waker is either
/// registered before the trigger drains the list, or it sees the flag.
#[test]
fn loom_async_wait_never_misses_the_trigger() {
    loom::model(|| {
        let coord = Coordinator::builder().build();
        let trigger = coord.trigger();
        let token = coord.token();
        let t = thread::spawn(move || {
            assert!(trigger.trigger(ShutdownReason::Requested));
        });
        loom::future::block_on(token.wait());
        assert_eq!(token.reason(), Some(ShutdownReason::Requested));
        assert!(t.join().is_ok());
    });
}

/// Dropping the last subsystem guard wakes a drain waiter.
#[test]
fn loom_subsystem_drain_never_misses_the_last_guard() {
    loom::model(|| {
        let coord = Coordinator::builder().build();
        let token = coord.token();
        let guard = token.subsystem_guard();
        let t = thread::spawn(move || drop(guard));
        loom::future::block_on(token.subsystems_finished());
        assert_eq!(token.active_subsystems(), 0);
        assert!(t.join().is_ok());
    });
}
