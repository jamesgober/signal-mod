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

/// Delivers `signal` to every registered coordinator covering it.
/// Returns whether one did.
pub(crate) fn dispatch(signal: Signal) -> bool {
    let targets: Vec<Arc<Dispatcher>> = coordinators()
        .iter()
        .filter(|entry| entry.covered.contains(signal))
        .filter_map(|entry| entry.dispatcher.upgrade())
        .collect();
    for target in &targets {
        target.deliver(signal);
    }
    !targets.is_empty()
}
