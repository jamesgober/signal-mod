//! Real-thread stress tests for the concurrency claims: one winner per
//! trigger race, every waiter woken, `reason()` never `None` once
//! initiated, subscriptions and child scopes under contention.
//!
//! Loom (`tests/loom.rs`) explores the same state machine exhaustively;
//! these runs exercise it on real hardware with real clocks.

#![cfg(not(loom))]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use signal_mod::{Coordinator, ShutdownReason, ShutdownToken, Signal, SignalSet};

const BUDGET: Duration = Duration::from_secs(10);

#[test]
fn test_many_concurrent_triggers_yield_single_initiator() {
    for _ in 0..64 {
        let coord = Coordinator::builder().build();
        let barrier = Arc::new(Barrier::new(16));
        let wins = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let t = coord.trigger();
                let b = Arc::clone(&barrier);
                let w = Arc::clone(&wins);
                thread::spawn(move || {
                    b.wait();
                    let reason = if i % 2 == 0 {
                        ShutdownReason::Requested
                    } else {
                        ShutdownReason::Error
                    };
                    if t.trigger(reason) {
                        w.fetch_add(1, Ordering::SeqCst);
                    } else {
                        // A loser must already see the initiated state.
                        assert!(t.is_initiated());
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(wins.load(Ordering::SeqCst), 1);
        assert!(coord.token().reason().is_some());
    }
}

// SM-004 / audit P4: 1.0.0 published the initiated flag before the
// reason, so a spinning observer could see `initiated && reason == None`.
#[test]
fn test_reason_is_never_none_once_initiated_under_contention() {
    for round in 0..200 {
        let coord = Coordinator::builder().build();
        let token = coord.token();
        let trigger = coord.trigger();
        let barrier = Arc::new(Barrier::new(5));
        let observers: Vec<_> = (0..4)
            .map(|_| {
                let token = token.clone();
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    loop {
                        if token.is_initiated() {
                            let reason = token.reason();
                            let elapsed = token.elapsed();
                            return reason.is_some() && elapsed.is_some();
                        }
                        std::hint::spin_loop();
                    }
                })
            })
            .collect();
        barrier.wait();
        let reason = ShutdownReason::Signal(Signal::ALL[round % Signal::ALL.len()]);
        assert!(trigger.trigger(reason));
        for o in observers {
            assert!(o.join().unwrap(), "observer saw initiated without reason");
        }
        let stats = coord.statistics();
        assert_eq!(stats.initiated, stats.reason.is_some());
    }
}

#[test]
fn test_many_blocking_observers_all_wake_on_single_trigger() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let started = Arc::new(Barrier::new(33));
    let woken = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..32)
        .map(|_| {
            let t: ShutdownToken = token.clone();
            let w = Arc::clone(&woken);
            let s = Arc::clone(&started);
            thread::spawn(move || {
                s.wait();
                if t.wait_blocking_timeout(BUDGET) {
                    w.fetch_add(1, Ordering::SeqCst);
                }
            })
        })
        .collect();
    started.wait();
    coord.trigger().trigger(ShutdownReason::Requested);
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(woken.load(Ordering::SeqCst), 32);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_many_async_waiters_all_wake_on_single_trigger() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let tasks: Vec<_> = (0..1000).map(|_| tokio::spawn(token.wait())).collect();
    tokio::task::yield_now().await;
    coord.trigger().trigger(ShutdownReason::Requested);
    for t in tasks {
        tokio::time::timeout(BUDGET, t)
            .await
            .expect("woken")
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_waiters_registered_while_triggering_never_hang() {
    for _ in 0..50 {
        let coord = Coordinator::builder().build();
        let token = coord.token();
        let trigger = coord.trigger();
        let spawner = tokio::spawn(async move {
            let mut handles = Vec::new();
            for _ in 0..50 {
                handles.push(tokio::spawn(token.wait()));
                tokio::task::yield_now().await;
            }
            handles
        });
        let triggerer = tokio::spawn(async move {
            tokio::task::yield_now().await;
            trigger.trigger(ShutdownReason::Requested)
        });
        assert!(triggerer.await.unwrap());
        for h in spawner.await.unwrap() {
            tokio::time::timeout(BUDGET, h)
                .await
                .expect("woken")
                .unwrap();
        }
    }
}

#[test]
fn test_high_volume_clones_are_consistent() {
    let coord = Coordinator::builder().build();
    let tokens: Vec<_> = (0..10_000).map(|_| coord.token()).collect();
    let triggers: Vec<_> = (0..10_000).map(|_| coord.trigger()).collect();
    assert!(tokens.iter().all(|t| !t.is_initiated()));
    triggers[1234].trigger(ShutdownReason::Requested);
    assert!(tokens.iter().all(ShutdownToken::is_initiated));
    assert!(
        triggers
            .iter()
            .all(signal_mod::ShutdownTrigger::is_initiated)
    );
}

#[test]
fn test_concurrent_subscribers_each_see_every_signal() {
    let coord = Arc::new(
        Coordinator::builder()
            .signals(SignalSet::empty())
            .notify_signals(SignalSet::empty().with(Signal::User1).with(Signal::User2))
            .build(),
    );
    let subscribers: Vec<_> = (0..8)
        .map(|_| {
            let mut sub = coord.subscribe();
            thread::spawn(move || {
                let deadline = std::time::Instant::now() + BUDGET;
                let mut seen = SignalSet::empty();
                while seen.len() < 2 && std::time::Instant::now() < deadline {
                    if let Some(sig) = sub.recv_timeout(Duration::from_millis(20)) {
                        seen |= sig;
                    }
                }
                seen
            })
        })
        .collect();
    let senders: Vec<_> = [Signal::User1, Signal::User2]
        .into_iter()
        .map(|sig| {
            let c = Arc::clone(&coord);
            thread::spawn(move || {
                for _ in 0..1000 {
                    assert!(c.simulate_signal(sig));
                }
            })
        })
        .collect();
    for s in senders {
        s.join().unwrap();
    }
    for sub in subscribers {
        assert_eq!(sub.join().unwrap().len(), 2);
    }
    assert!(!coord.token().is_initiated());
}

#[test]
fn test_child_scopes_created_concurrently_with_trigger_all_end_initiated() {
    for _ in 0..50 {
        let coord = Coordinator::builder().build();
        let root = coord.token();
        let barrier = Arc::new(Barrier::new(9));
        let makers: Vec<_> = (0..8)
            .map(|_| {
                let root = root.clone();
                let b = Arc::clone(&barrier);
                thread::spawn(move || {
                    b.wait();
                    (0..64).map(|_| root.child().1).collect::<Vec<_>>()
                })
            })
            .collect();
        barrier.wait();
        coord.trigger().trigger(ShutdownReason::Requested);
        for m in makers {
            for child in m.join().unwrap() {
                assert_eq!(child.reason(), Some(ShutdownReason::Requested));
            }
        }
    }
}

#[test]
fn test_subsystem_guards_from_many_threads_drain_to_zero() {
    let coord = Coordinator::builder().build();
    let token = coord.token();
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let token = token.clone();
            thread::spawn(move || {
                for _ in 0..1000 {
                    let (_t, child) = token.child();
                    let g1 = token.subsystem_guard();
                    let g2 = child.subsystem_guard();
                    drop(g2);
                    drop(g1);
                }
                token.subsystem_guard()
            })
        })
        .collect();
    let guards: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert_eq!(token.active_subsystems(), 16);
    let waiter = {
        let token = token.clone();
        thread::spawn(move || token.wait_subsystems_blocking(BUDGET))
    };
    drop(guards);
    assert!(waiter.join().unwrap());
    assert_eq!(token.active_subsystems(), 0);
}
