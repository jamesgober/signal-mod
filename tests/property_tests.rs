//! Property tests for `signal-mod`.
//!
//! These tests exercise algebraic properties of the public surface
//! rather than specific example inputs. They are the primary
//! contract guard during the pre-1.0 stabilization window.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use proptest::collection::vec;
use proptest::prelude::*;

use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, Signal, SignalSet};

fn any_signal() -> impl Strategy<Value = Signal> {
    prop_oneof![
        Just(Signal::Terminate),
        Just(Signal::Interrupt),
        Just(Signal::Quit),
        Just(Signal::Hangup),
        Just(Signal::Pipe),
        Just(Signal::User1),
        Just(Signal::User2),
    ]
}

fn any_signal_set() -> impl Strategy<Value = SignalSet> {
    vec(any_signal(), 0..8)
        .prop_map(|sigs| sigs.into_iter().fold(SignalSet::empty(), SignalSet::with))
}

fn any_reason() -> impl Strategy<Value = ShutdownReason> {
    prop_oneof![
        Just(ShutdownReason::Requested),
        Just(ShutdownReason::Forced),
        Just(ShutdownReason::Timeout),
        Just(ShutdownReason::Error),
        any_signal().prop_map(ShutdownReason::Signal),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// `with(s).contains(s)` is a tautology.
    #[test]
    fn with_then_contains(set in any_signal_set(), sig in any_signal()) {
        prop_assert!(set.with(sig).contains(sig));
    }

    /// `without(s).contains(s)` is always false.
    #[test]
    fn without_then_not_contains(set in any_signal_set(), sig in any_signal()) {
        prop_assert!(!set.without(sig).contains(sig));
    }

    /// `with(s).without(s) == without(s)`.
    #[test]
    fn with_then_without_is_without(set in any_signal_set(), sig in any_signal()) {
        prop_assert_eq!(set.with(sig).without(sig), set.without(sig));
    }

    /// `without(s).with(s).contains(s)`.
    #[test]
    fn without_then_with_contains(set in any_signal_set(), sig in any_signal()) {
        prop_assert!(set.without(sig).with(sig).contains(sig));
    }

    /// `len()` matches the iterator count.
    #[test]
    fn len_matches_iter_count(set in any_signal_set()) {
        prop_assert_eq!(set.len(), set.iter().count());
    }

    /// `iter()` order is canonical [`Signal::ALL`] order.
    #[test]
    fn iter_is_canonical_order(set in any_signal_set()) {
        let mut prev_index: Option<usize> = None;
        for sig in set.iter() {
            let i = Signal::ALL.iter().position(|s| *s == sig).unwrap();
            if let Some(p) = prev_index {
                prop_assert!(i > p, "iter not in canonical order");
            }
            prev_index = Some(i);
        }
    }

    /// First trigger always returns true; subsequent triggers
    /// always return false.
    #[test]
    fn first_trigger_then_all_redundant(
        first in any_reason(),
        rest in vec(any_reason(), 0..8),
    ) {
        let coord = Coordinator::builder().build();
        let trig = coord.trigger();
        prop_assert!(trig.trigger(first));
        for r in rest {
            prop_assert!(!trig.trigger(r));
        }
        prop_assert_eq!(coord.token().reason(), Some(first));
    }

    /// `run_hooks` always returns `hooks.len()` when the budget is
    /// large.
    #[test]
    fn run_hooks_returns_full_count(priorities in vec(any::<i32>(), 0..16)) {
        let counter = Arc::new(AtomicUsize::new(0));
        let mut builder = Coordinator::builder()
            .graceful_timeout(std::time::Duration::from_secs(30));
        let n = priorities.len();
        for (i, p) in priorities.iter().enumerate() {
            let c = Arc::clone(&counter);
            builder = builder.hook(hook_from_fn(
                format!("h{i}"),
                *p,
                move |_| { c.fetch_add(1, Ordering::Relaxed); },
            ));
        }
        let coord = builder.build();
        let count = coord.run_hooks(ShutdownReason::Requested);
        prop_assert_eq!(count, n);
        prop_assert_eq!(counter.load(Ordering::Relaxed), n);
    }

    /// `Signal::unix_number` is unique per variant.
    #[test]
    fn unix_numbers_are_unique(a in any_signal(), b in any_signal()) {
        if a != b {
            prop_assert_ne!(a.unix_number(), b.unix_number());
        }
    }

    /// `Signal::is_unix_only` is consistent with the platform
    /// availability check on Windows.
    #[test]
    fn is_unix_only_consistent(sig in any_signal()) {
        if cfg!(unix) {
            prop_assert!(sig.available_on_current_platform());
        } else {
            prop_assert_eq!(
                sig.available_on_current_platform(),
                !sig.is_unix_only(),
            );
        }
    }
}
