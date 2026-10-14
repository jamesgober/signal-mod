//! Boundary conditions of the public surface: empty sets, already
//! initiated waits, extreme timeouts, signal metadata, and runtime-agnostic
//! waiting on executors other than Tokio.

#![cfg(not(loom))]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};
use std::time::{Duration, Instant};

use signal_mod::{Coordinator, ShutdownReason, Signal, SignalSet};

// ---------------------------------------------------------------------
// SignalSet
// ---------------------------------------------------------------------

#[test]
fn test_empty_set_iter_yields_nothing() {
    let empty = SignalSet::empty();
    assert!(empty.is_empty());
    assert_eq!(empty.len(), 0);
    assert_eq!(empty.iter().count(), 0);
    assert!(empty.into_iter().next().is_none());
}

#[test]
fn test_all_set_contains_every_variant() {
    let all = SignalSet::all();
    assert_eq!(all.len(), Signal::ALL.len());
    for sig in Signal::ALL {
        assert!(all.contains(sig));
    }
}

#[test]
fn test_set_round_trips_through_with_and_without() {
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
fn test_set_default_matches_graceful() {
    assert_eq!(SignalSet::default(), SignalSet::graceful());
}

#[test]
fn test_set_hash_distinguishes_sets() {
    use std::collections::HashSet;
    let sets: HashSet<SignalSet> = [
        SignalSet::empty(),
        SignalSet::graceful(),
        SignalSet::standard(),
        SignalSet::all(),
        SignalSet::graceful(),
    ]
    .into_iter()
    .collect();
    assert_eq!(sets.len(), 4);
}

// ---------------------------------------------------------------------
// Signal metadata (SM-001)
// ---------------------------------------------------------------------

#[test]
fn test_signal_numbers_are_unique_and_round_trip() {
    let mut seen = std::collections::HashSet::new();
    for sig in Signal::ALL {
        if let Some(n) = sig.unix_number() {
            assert!(seen.insert(n), "duplicate number {n}");
            assert_eq!(Signal::from_unix_number(n), Some(sig));
        }
    }
}

#[test]
fn test_signal_display_renders_description() {
    for sig in Signal::ALL {
        assert_eq!(format!("{sig}"), sig.description());
    }
}

#[test]
fn test_reason_descriptions_are_stable() {
    assert_eq!(ShutdownReason::Requested.description(), "requested");
    assert_eq!(ShutdownReason::Forced.description(), "forced");
    assert_eq!(ShutdownReason::Timeout.description(), "timeout");
    assert_eq!(ShutdownReason::Error.description(), "error");
    assert_eq!(
        ShutdownReason::Signal(Signal::Terminate).description(),
        "signal"
    );
}

// ---------------------------------------------------------------------
// Waiting
// ---------------------------------------------------------------------

#[test]
fn test_wait_blocking_timeout_returns_immediately_if_already_initiated() {
    let coord = Coordinator::builder().build();
    coord.trigger().trigger(ShutdownReason::Requested);
    let start = Instant::now();
    assert!(coord.token().wait_blocking_timeout(Duration::from_secs(30)));
    // Generous bound: this is the lock-free fast path, but CI runners
    // can deschedule any thread for a while.
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
fn test_wait_blocking_returns_when_already_initiated() {
    let coord = Coordinator::builder().build();
    coord.trigger().trigger(ShutdownReason::Requested);
    coord.token().wait_blocking();
}

#[test]
fn test_wait_blocking_timeout_zero_does_not_block() {
    let coord = Coordinator::builder().build();
    assert!(!coord.token().wait_blocking_timeout(Duration::ZERO));
}

#[test]
fn test_wait_subsystems_without_guards_is_immediate() {
    let coord = Coordinator::builder().build();
    assert!(coord.token().wait_subsystems_blocking(Duration::ZERO));
    assert_eq!(coord.token().active_subsystems(), 0);
}

#[tokio::test]
async fn test_wait_resolves_immediately_when_already_initiated() {
    let coord = Coordinator::builder().build();
    coord.trigger().trigger(ShutdownReason::Requested);
    tokio::time::timeout(Duration::from_secs(5), coord.token().wait())
        .await
        .expect("fast path");
}

#[tokio::test]
async fn test_token_into_future_and_cancelled_resolve_on_trigger() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let by_value = tokio::spawn(token.clone().into_future());
    let by_ref = tokio::spawn({
        let token = token.clone();
        async move { (&token).await }
    });
    let cancelled = tokio::spawn(token.cancelled());
    tokio::task::yield_now().await;
    coord.trigger().trigger(ShutdownReason::Requested);
    for handle in [by_value, by_ref, cancelled] {
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("woken")
            .unwrap();
    }
}

#[tokio::test]
async fn test_subsystems_finished_resolves_after_last_guard() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let guard = token.subsystem_guard();
    let waiter = tokio::spawn(token.subsystems_finished());
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert!(!waiter.is_finished());
    drop(guard);
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("woken")
        .unwrap();
}

#[test]
fn test_wait_works_on_smol_without_tokio() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let trigger = coord.trigger();
    let reason = smol::block_on(async move {
        let waiter = smol::spawn({
            let token = token.clone();
            async move {
                token.wait().await;
                token.reason()
            }
        });
        smol::Timer::after(Duration::from_millis(10)).await;
        trigger.trigger(ShutdownReason::Requested);
        waiter.await
    });
    assert_eq!(reason, Some(ShutdownReason::Requested));
}

/// The smallest possible executor: park the thread until woken. Proves
/// the futures need nothing from any runtime.
fn block_on<F: Future>(fut: F) -> F::Output {
    struct Unpark(std::thread::Thread, AtomicUsize);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.1.fetch_add(1, Ordering::SeqCst);
            self.0.unpark();
        }
    }
    let unpark = Arc::new(Unpark(std::thread::current(), AtomicUsize::new(0)));
    let waker = Waker::from(Arc::clone(&unpark));
    let mut cx = Context::from_waker(&waker);
    let mut fut = Box::pin(fut);
    loop {
        if let Poll::Ready(out) = Pin::as_mut(&mut fut).poll(&mut cx) {
            return out;
        }
        std::thread::park();
    }
}

// SM-008: the 1.0.0 async-std `wait()` slept 1-50 ms between polls
// (median wake latency 27 ms in the audit). The new future is woken
// directly by `trigger`.
#[test]
fn test_wait_on_bare_executor_wakes_promptly_without_polling() {
    let coord = Coordinator::builder().build();
    let trigger = coord.trigger();
    let fired_at = Arc::new(std::sync::Mutex::new(None));
    let f = Arc::clone(&fired_at);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        *f.lock().unwrap() = Some(Instant::now());
        trigger.trigger(ShutdownReason::Requested)
    });
    block_on(coord.token().wait());
    let woke_at = Instant::now();
    assert!(handle.join().unwrap());
    let fired = fired_at.lock().unwrap().unwrap();
    let latency = woke_at.saturating_duration_since(fired);
    assert!(latency < Duration::from_millis(500), "latency {latency:?}");
}

#[test]
fn test_subscription_recv_on_bare_executor() {
    let coord = Coordinator::builder()
        .notify_signals(SignalSet::from(Signal::User1))
        .build();
    let mut sub = coord.subscribe();
    let coord = Arc::new(coord);
    let remote = Arc::clone(&coord);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        remote.simulate_signal(Signal::User1)
    });
    assert_eq!(block_on(sub.recv()), Some(Signal::User1));
    assert!(handle.join().unwrap());
}

// ---------------------------------------------------------------------
// Futures: Debug, drop while pending, closed subscriptions
// ---------------------------------------------------------------------

fn noop_cx_poll<F: Future + Unpin>(fut: &mut F) -> Poll<F::Output> {
    let mut cx = Context::from_waker(Waker::noop());
    Pin::new(fut).poll(&mut cx)
}

#[test]
fn test_public_handles_and_futures_have_informative_debug() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let guard = token.subsystem_guard();
    let wait = token.wait();
    let mut sub = coord.subscribe();
    let recv = sub.recv();
    assert!(format!("{recv:?}").contains("Recv"));
    drop(recv);
    let run = coord.run_hooks_async(ShutdownReason::Requested);
    for text in [
        format!("{wait:?}"),
        format!("{guard:?}"),
        format!("{run:?}"),
        format!("{token:?}"),
        format!("{:?}", coord.trigger()),
        format!("{sub:?}"),
    ] {
        assert!(!text.is_empty());
    }
    assert!(format!("{guard:?}").contains("active_subsystems: 1"));
}

#[test]
fn test_pending_futures_dropped_before_completion_unregister_cleanly() {
    let coord = Coordinator::builder()
        .notify_signals(SignalSet::from(Signal::User1))
        .build();
    let token = coord.token();
    let guard = token.subsystem_guard();

    let mut drained = token.subsystems_finished();
    assert!(noop_cx_poll(&mut drained).is_pending());
    drop(drained);

    let mut sub = coord.subscribe();
    {
        let mut recv = sub.recv();
        assert!(noop_cx_poll(&mut recv).is_pending());
    }
    drop(guard);
    assert!(coord.simulate_signal(Signal::User1));
    assert_eq!(sub.try_recv(), Some(Signal::User1));
}

#[test]
fn test_recv_on_closed_subscription_resolves_none() {
    let coord = Coordinator::builder().build();
    let mut sub = coord.subscribe();
    drop(coord);
    let mut recv = sub.recv();
    assert_eq!(noop_cx_poll(&mut recv), Poll::Ready(None));
}

#[test]
fn test_recv_timeout_with_unbounded_timeout_returns_delivered_signal() {
    let coord = Arc::new(
        Coordinator::builder()
            .notify_signals(SignalSet::from(Signal::User2))
            .build(),
    );
    let mut sub = coord.subscribe();
    let remote = Arc::clone(&coord);
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        remote.simulate_signal(Signal::User2)
    });
    assert_eq!(sub.recv_timeout(Duration::MAX), Some(Signal::User2));
    assert!(handle.join().unwrap());
}

#[test]
fn test_child_of_initiated_scope_is_ready_immediately() {
    let coord = Coordinator::builder().build();
    coord.trigger().trigger(ShutdownReason::Timeout);
    let (_t, child) = coord.token().child();
    let mut wait = child.wait();
    assert_eq!(noop_cx_poll(&mut wait), Poll::Ready(()));
    assert_eq!(child.reason(), Some(ShutdownReason::Timeout));
}

#[test]
fn test_wait_subsystems_without_deadline_returns_when_guard_dropped() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let guard = token.subsystem_guard();
    let handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(10));
        drop(guard);
    });
    assert!(token.wait_subsystems_blocking(Duration::MAX));
    handle.join().unwrap();
}

#[test]
fn test_builder_escalation_flag_round_trips() {
    let coord = Coordinator::builder()
        .force_exit_on_second_signal(true)
        .build();
    assert!(coord.force_exit_on_second_signal());
    assert!(format!("{coord:?}").contains("force_exit_on_second_signal: true"));
}

#[cfg(feature = "tokio-util")]
#[test]
fn test_cancellation_token_follows_shutdown_in_both_orders() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let before = token.cancellation_token();
    let via_from = tokio_util::sync::CancellationToken::from(&token);
    assert!(!before.is_cancelled() && !via_from.is_cancelled());
    // One-way link: cancelling the CancellationToken does not shut down.
    before.cancel();
    assert!(!token.is_initiated());
    coord.trigger().trigger(ShutdownReason::Requested);
    assert!(via_from.is_cancelled());
    let after = token.cancellation_token();
    assert!(after.is_cancelled());
}
