//! Property tests: algebraic laws of `SignalSet`, signal-number
//! bijection, trigger idempotence, and hook ordering / run-once.

#![cfg(not(loom))]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use proptest::collection::vec;
use proptest::prelude::*;

use signal_mod::{Coordinator, ShutdownReason, Signal, SignalSet, hook_from_fn};

fn any_signal() -> impl Strategy<Value = Signal> {
    prop::sample::select(Signal::ALL.to_vec())
}

fn any_signal_set() -> impl Strategy<Value = SignalSet> {
    vec(any_signal(), 0..8).prop_map(|sigs| sigs.into_iter().collect())
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

    #[test]
    fn prop_with_then_contains(set in any_signal_set(), sig in any_signal()) {
        prop_assert!(set.with(sig).contains(sig));
    }

    #[test]
    fn prop_without_then_not_contains(set in any_signal_set(), sig in any_signal()) {
        prop_assert!(!set.without(sig).contains(sig));
    }

    #[test]
    fn prop_with_then_without_is_without(set in any_signal_set(), sig in any_signal()) {
        prop_assert_eq!(set.with(sig).without(sig), set.without(sig));
    }

    #[test]
    fn prop_len_matches_iter_count_and_size_hint(set in any_signal_set()) {
        let it = set.iter();
        prop_assert_eq!(it.size_hint(), (set.len(), Some(set.len())));
        prop_assert_eq!(set.len(), it.count());
    }

    #[test]
    fn prop_iter_is_strictly_canonical_order(set in any_signal_set()) {
        let positions: Vec<usize> = set
            .iter()
            .map(|s| Signal::ALL.iter().position(|a| *a == s).unwrap())
            .collect();
        prop_assert!(positions.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn prop_collect_of_iter_is_identity(set in any_signal_set()) {
        prop_assert_eq!(set.iter().collect::<SignalSet>(), set);
    }

    #[test]
    fn prop_set_operators_obey_boolean_algebra(
        a in any_signal_set(),
        b in any_signal_set(),
        c in any_signal_set(),
    ) {
        prop_assert_eq!(a | b, b | a);
        prop_assert_eq!(a & b, b & a);
        prop_assert_eq!((a | b) | c, a | (b | c));
        prop_assert_eq!(a & (b | c), (a & b) | (a & c));
        prop_assert_eq!((a - b) & b, SignalSet::empty());
        prop_assert_eq!((a - b) | (a & b), a);
        for sig in Signal::ALL {
            prop_assert_eq!((a | b).contains(sig), a.contains(sig) || b.contains(sig));
            prop_assert_eq!((a & b).contains(sig), a.contains(sig) && b.contains(sig));
            prop_assert_eq!((a - b).contains(sig), a.contains(sig) && !b.contains(sig));
        }
    }

    #[test]
    fn prop_available_subset_is_subset(set in any_signal_set()) {
        let avail = set.available_on_current_platform();
        prop_assert_eq!(avail - set, SignalSet::empty());
        for sig in avail {
            prop_assert!(sig.available_on_current_platform());
        }
    }

    /// `unix_number` is injective and inverted by `from_unix_number`.
    #[test]
    fn prop_unix_numbers_are_unique_and_invertible(a in any_signal(), b in any_signal()) {
        if a != b {
            if let (Some(x), Some(y)) = (a.unix_number(), b.unix_number()) {
                prop_assert_ne!(x, y);
            }
        }
        if let Some(n) = a.unix_number() {
            prop_assert_eq!(Signal::from_unix_number(n), Some(a));
            prop_assert_eq!(a.exit_code(), 128 + n);
        }
    }

    #[test]
    fn prop_from_unix_number_rejects_unknown_numbers(n in any::<i32>()) {
        match Signal::from_unix_number(n) {
            Some(sig) => prop_assert_eq!(sig.unix_number(), Some(n)),
            None => prop_assert!(Signal::ALL.iter().all(|s| s.unix_number() != Some(n))),
        }
    }

    /// The first trigger wins; every later trigger reports `false` and
    /// leaves the reason unchanged.
    #[test]
    fn prop_first_trigger_wins(first in any_reason(), rest in vec(any_reason(), 0..8)) {
        let coord = Coordinator::builder().build();
        let trig = coord.trigger();
        prop_assert!(trig.trigger(first));
        for r in rest {
            prop_assert!(!trig.trigger(r));
        }
        prop_assert_eq!(coord.token().reason(), Some(first));
    }

    /// Hooks run once each, in non-increasing priority order with ties in
    /// registration order, and a second run executes nothing.
    #[test]
    fn prop_hooks_run_once_in_stable_priority_order(priorities in vec(any::<i32>(), 0..12)) {
        let order = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut builder = Coordinator::builder().graceful_timeout(Duration::from_secs(30));
        for (i, p) in priorities.iter().copied().enumerate() {
            let o = Arc::clone(&order);
            let c = Arc::clone(&calls);
            builder = builder.hook(hook_from_fn(format!("h{i}"), p, move |_| {
                o.lock().unwrap().push((p, i));
                c.fetch_add(1, Ordering::SeqCst);
            }));
        }
        let coord = builder.build();
        prop_assert_eq!(coord.run_hooks(ShutdownReason::Requested), priorities.len());
        prop_assert_eq!(coord.run_hooks(ShutdownReason::Requested), priorities.len());
        prop_assert_eq!(calls.load(Ordering::SeqCst), priorities.len());

        let mut expected: Vec<(i32, usize)> =
            priorities.iter().copied().enumerate().map(|(i, p)| (p, i)).collect();
        expected.sort_by_key(|&(p, i)| (std::cmp::Reverse(p), i));
        prop_assert_eq!(&*order.lock().unwrap(), &expected);
    }
}
