//! Synchronization primitives used by the shutdown state machine.
//!
//! Under `--cfg loom` these resolve to `loom`'s model-checked
//! equivalents so the models in `loom_tests.rs` can explore every interleaving of
//! trigger / wait / reason. Everything else in the crate (hook runner,
//! dispatcher, back-ends) uses `std` directly: it is either not
//! concurrent in an interesting way or is covered by real-thread
//! stress tests.

#[cfg(loom)]
pub(crate) use loom::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
#[cfg(loom)]
pub(crate) use loom::sync::{Arc, Condvar, Mutex, MutexGuard};

#[cfg(not(loom))]
pub(crate) use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
#[cfg(not(loom))]
pub(crate) use std::sync::{Arc, Condvar, Mutex, MutexGuard};

/// Non-owning reference from a parent scope to a child scope.
///
/// `loom::sync::Arc` has no `Weak`; under loom the parent keeps the
/// child alive instead. Loom models never create enough children for
/// that to matter.
#[cfg(not(loom))]
pub(crate) type WeakArc<T> = std::sync::Weak<T>;
#[cfg(loom)]
pub(crate) type WeakArc<T> = Arc<T>;

#[cfg(not(loom))]
pub(crate) fn downgrade<T>(arc: &Arc<T>) -> WeakArc<T> {
    Arc::downgrade(arc)
}
#[cfg(loom)]
pub(crate) fn downgrade<T>(arc: &Arc<T>) -> WeakArc<T> {
    Arc::clone(arc)
}

#[cfg(not(loom))]
pub(crate) fn upgrade<T>(weak: &WeakArc<T>) -> Option<Arc<T>> {
    weak.upgrade()
}
#[cfg(loom)]
pub(crate) fn upgrade<T>(weak: &WeakArc<T>) -> Option<Arc<T>> {
    Some(Arc::clone(weak))
}

#[cfg(not(loom))]
pub(crate) fn is_dangling<T>(weak: &WeakArc<T>) -> bool {
    weak.strong_count() == 0
}
#[cfg(loom)]
pub(crate) fn is_dangling<T>(_weak: &WeakArc<T>) -> bool {
    false
}

/// Lock a mutex, recovering the guard if a previous holder panicked.
///
/// No user code ever runs while one of the crate's locks is held (hooks,
/// wakers and callbacks are invoked after the guard is dropped), so a
/// poisoned lock can only come from a panic inside this crate's own
/// short critical sections, which leave the protected data consistent.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Wait on a condition variable, recovering from poisoning (see [`lock`]).
pub(crate) fn wait<'a, T>(cv: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    match cv.wait(guard) {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Wait on a condition variable for at most `dur`, recovering from
/// poisoning (see [`lock`]). Spurious and timed-out wake-ups are
/// indistinguishable to callers: they re-check their condition and
/// their own deadline.
pub(crate) fn wait_timeout<'a, T>(
    cv: &Condvar,
    guard: MutexGuard<'a, T>,
    dur: std::time::Duration,
) -> MutexGuard<'a, T> {
    match cv.wait_timeout(guard, dur) {
        Ok((guard, _)) => guard,
        Err(poisoned) => poisoned.into_inner().0,
    }
}
