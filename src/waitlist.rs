//! A small slab of task wakers.
//!
//! Every runtime-agnostic future in the crate (`ShutdownFuture`,
//! `SubsystemsFinished`, the `run_hooks_async` future, subscription
//! `recv`) parks its waker here, under the owner's mutex, after checking
//! its condition under that same mutex. The owner drains the list under
//! the mutex when the condition changes and wakes the tasks after
//! releasing it. That ordering is what makes the futures lost-wake-up
//! free without any polling.

use std::task::Waker;

/// Handle to one registered waker.
///
/// Carries the drain epoch it was issued in, so a key that outlived a
/// [`WaitList::take_all`] can never address a slot that was since handed
/// to a different task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WaitKey {
    epoch: u64,
    index: usize,
}

/// Slab of registered wakers.
#[derive(Debug, Default)]
pub(crate) struct WaitList {
    slots: Vec<Option<Waker>>,
    free: Vec<usize>,
    epoch: u64,
}

impl WaitList {
    /// Register `waker` under `key`, allocating a key on first use (or
    /// when `key` is stale) and replacing the stored waker only when it
    /// would wake a different task.
    pub(crate) fn register(&mut self, key: &mut Option<WaitKey>, waker: &Waker) {
        if let Some(k) = *key {
            if k.epoch == self.epoch {
                if let Some(slot) = self.slots.get_mut(k.index) {
                    match slot {
                        Some(existing) if existing.will_wake(waker) => {}
                        _ => *slot = Some(waker.clone()),
                    }
                    return;
                }
            }
        }
        let index = if let Some(index) = self.free.pop() {
            if let Some(slot) = self.slots.get_mut(index) {
                *slot = Some(waker.clone());
            }
            index
        } else {
            self.slots.push(Some(waker.clone()));
            self.slots.len() - 1
        };
        *key = Some(WaitKey {
            epoch: self.epoch,
            index,
        });
    }

    /// Forget the waker stored under `key`. Stale keys are ignored.
    pub(crate) fn remove(&mut self, key: WaitKey) {
        if key.epoch != self.epoch {
            return;
        }
        if let Some(slot) = self.slots.get_mut(key.index) {
            if slot.take().is_some() {
                self.free.push(key.index);
            }
        }
    }

    /// Drain every registered waker. Outstanding keys become stale.
    pub(crate) fn take_all(&mut self) -> Vec<Waker> {
        self.epoch = self.epoch.wrapping_add(1);
        self.free.clear();
        self.slots.drain(..).flatten().collect()
    }

    /// Number of registered wakers.
    #[cfg(all(test, not(loom)))]
    pub(crate) fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

/// Wake every waker in `wakers`. Called after the owner's lock is
/// released so that a waker that synchronously polls (some executors
/// do) cannot deadlock on it.
pub(crate) fn wake_all(wakers: Vec<Waker>) {
    for waker in wakers {
        waker.wake();
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Wake;

    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            let _previous = self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn counting() -> (Arc<CountingWaker>, Waker) {
        let inner = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let waker = Waker::from(Arc::clone(&inner));
        (inner, waker)
    }

    #[test]
    fn test_waitlist_register_twice_same_task_keeps_one_slot() {
        let mut list = WaitList::default();
        let (_c, waker) = counting();
        let mut key = None;
        list.register(&mut key, &waker);
        list.register(&mut key, &waker);
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn test_waitlist_remove_then_register_reuses_slot() {
        let mut list = WaitList::default();
        let (_c, waker) = counting();
        let mut a = None;
        let mut b = None;
        list.register(&mut a, &waker);
        list.register(&mut b, &waker);
        list.remove(a.unwrap_or(WaitKey { epoch: 0, index: 0 }));
        let mut c = None;
        list.register(&mut c, &waker);
        assert_eq!(c, a);
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn test_waitlist_take_all_wakes_every_registered_task() {
        let mut list = WaitList::default();
        let (count, waker) = counting();
        let mut keys = [None, None, None];
        for key in &mut keys {
            list.register(key, &waker);
        }
        wake_all(list.take_all());
        assert_eq!(count.0.load(Ordering::SeqCst), 3);
        assert_eq!(list.len(), 0);
    }

    #[test]
    fn test_waitlist_stale_key_cannot_clobber_new_registration() {
        let mut list = WaitList::default();
        let (old_count, old) = counting();
        let (new_count, new) = counting();
        let mut stale = None;
        list.register(&mut stale, &old);
        let _drained = list.take_all();

        let mut fresh = None;
        list.register(&mut fresh, &new);
        // The stale key addressed index 0, which now belongs to `fresh`.
        list.register(&mut stale, &old);
        if let Some(k) = stale {
            list.remove(k);
        }
        wake_all(list.take_all());
        assert_eq!(new_count.0.load(Ordering::SeqCst), 1);
        assert_eq!(old_count.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_waitlist_register_with_different_waker_replaces_it() {
        let mut list = WaitList::default();
        let (first_count, first) = counting();
        let (second_count, second) = counting();
        let mut key = None;
        list.register(&mut key, &first);
        list.register(&mut key, &second);
        wake_all(list.take_all());
        assert_eq!(first_count.0.load(Ordering::SeqCst), 0);
        assert_eq!(second_count.0.load(Ordering::SeqCst), 1);
    }
}
