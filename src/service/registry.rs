//! The coordinators a service control reaches.
//!
//! The control handler has no coordinator of its own, so every installed
//! [`Coordinator`](crate::Coordinator) registers here (whatever back-end
//! it uses) and leaves at uninstall; a control code is delivered to each
//! one whose shutdown or notify set contains its signal.

use std::sync::{Arc, Mutex, MutexGuard, Weak};

use crate::dispatch::Dispatcher;
use crate::signal::{Signal, SignalSet};

struct Entry {
    dispatcher: Weak<Dispatcher>,
    covered: SignalSet,
}

static COORDINATORS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

fn coordinators() -> MutexGuard<'static, Vec<Entry>> {
    match COORDINATORS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(crate) fn register(dispatcher: &Arc<Dispatcher>, covered: SignalSet) {
    let mut list = coordinators();
    list.retain(|entry| entry.dispatcher.strong_count() > 0);
    list.push(Entry {
        dispatcher: Arc::downgrade(dispatcher),
        covered,
    });
}

pub(crate) fn unregister(dispatcher: &Arc<Dispatcher>) {
    coordinators().retain(|entry| {
        entry.dispatcher.strong_count() > 0
            && !std::ptr::eq(entry.dispatcher.as_ptr(), Arc::as_ptr(dispatcher))
    });
}

/// Whether `dispatcher` is registered.
#[cfg(all(test, feature = "agnostic"))]
pub(crate) fn is_registered(dispatcher: &Arc<Dispatcher>) -> bool {
    coordinators()
        .iter()
        .any(|entry| std::ptr::eq(entry.dispatcher.as_ptr(), Arc::as_ptr(dispatcher)))
}

/// Delivers `signal` to every registered coordinator covering it.
/// Returns whether one did.
///
/// A service control never escalates
/// ([`deliver_without_escalation`](Dispatcher::deliver_without_escalation)): the manager sends
/// one stop, so a repeat means the program was already shutting down on
/// its own, and ending it would skip the `STOPPED` report.
pub(crate) fn dispatch(signal: Signal) -> bool {
    let targets: Vec<Arc<Dispatcher>> = coordinators()
        .iter()
        .filter(|entry| entry.covered.contains(signal))
        .filter_map(|entry| entry.dispatcher.upgrade())
        .collect();
    for target in &targets {
        target.deliver_without_escalation(signal);
    }
    !targets.is_empty()
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::reason::ShutdownReason;
    use crate::state::Inner;
    use crate::token::ShutdownTrigger;

    // Pre-1.0 review: a stop control reached the escalation path, so a
    // service whose program had already started shutting down (while still
    // reporting RUNNING) was ended on the spot by `sc stop`, without its
    // STOPPED report; the SCM then records a failure and runs the
    // service's recovery actions. If this test regresses, the test process
    // exits with code 143.
    #[test]
    fn test_stop_control_after_initiation_does_not_escalate() {
        let inner = Inner::new();
        let dispatcher = Arc::new(Dispatcher::new(
            ShutdownTrigger::new(crate::sync::Arc::clone(&inner)),
            SignalSet::from(Signal::Terminate),
            true,
        ));
        register(&dispatcher, SignalSet::from(Signal::Terminate));
        assert!(inner.trigger(ShutdownReason::Error));
        assert!(dispatch(Signal::Terminate));
        assert!(dispatch(Signal::Terminate));
        assert_eq!(inner.reason(), Some(ShutdownReason::Error));
        unregister(&dispatcher);
        assert!(!dispatch(Signal::Terminate));
    }
}
