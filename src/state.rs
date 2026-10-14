//! Shutdown state shared by [`ShutdownToken`](crate::ShutdownToken),
//! [`ShutdownTrigger`](crate::ShutdownTrigger) and the
//! [`Coordinator`](crate::Coordinator).
//!
//! # Publication protocol
//!
//! `trigger` writes the reason and timestamp into their own atomics and
//! only then stores `initiated = true` with `Release` ordering, all
//! while holding the `shared` mutex. Readers load `initiated` with
//! `Acquire`; once they see `true`, the reason and timestamp are
//! guaranteed visible. So `is_initiated() == true` implies
//! `reason().is_some()`, and every reader of the reason is lock-free.
//! (1.0.0 set the flag before writing the reason, which let observers
//! see an initiated state with no reason.)
//!
//! Waiters (condition-variable and async) check `initiated` while
//! holding `shared`, and `trigger` drains them while holding it, so no
//! wake-up can be lost.

use core::fmt;
use core::task::{Context, Poll};
use std::time::{Duration, Instant};

use crate::reason::ShutdownReason;
use crate::sync::{
    self, Arc, AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Condvar, Mutex, Ordering, WeakArc,
};
use crate::waitlist::{WaitKey, WaitList, wake_all};

/// Prune dead child references once this many have accumulated.
const CHILD_PRUNE_THRESHOLD: usize = 32;

#[derive(Debug, Default)]
struct Shared {
    /// Tasks waiting for initiation.
    initiated: WaitList,
    /// Tasks waiting for every subsystem guard to drop.
    drained: WaitList,
    /// Child scopes to initiate together with this one.
    children: Vec<WeakArc<Inner>>,
    /// Cancellation tokens to cancel on initiation.
    #[cfg(feature = "tokio-util")]
    cancellation: Vec<tokio_util::sync::CancellationToken>,
}

#[cfg(feature = "tokio-util")]
type Cancellations = Vec<tokio_util::sync::CancellationToken>;
#[cfg(not(feature = "tokio-util"))]
type Cancellations = ();

#[cfg(feature = "tokio-util")]
fn take_cancellations(shared: &mut Shared) -> Cancellations {
    core::mem::take(&mut shared.cancellation)
}
#[cfg(not(feature = "tokio-util"))]
fn take_cancellations(_shared: &mut Shared) -> Cancellations {}

#[cfg(feature = "tokio-util")]
fn cancel_all(tokens: Cancellations) {
    for token in tokens {
        token.cancel();
    }
}
#[cfg(not(feature = "tokio-util"))]
fn cancel_all((): Cancellations) {}

/// The state machine behind one shutdown scope.
pub(crate) struct Inner {
    created: Instant,
    initiated: AtomicBool,
    reason: AtomicU32,
    initiated_at_nanos: AtomicU64,
    /// Live subsystem guards in this scope or any descendant scope.
    active: AtomicUsize,
    shared: Mutex<Shared>,
    cv: Condvar,
    parent: Option<Arc<Inner>>,
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner")
            .field("initiated", &self.is_initiated())
            .field("reason", &self.reason())
            .field("active_subsystems", &self.active_subsystems())
            .field("has_parent", &self.parent.is_some())
            .finish_non_exhaustive()
    }
}

impl Inner {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::with_parent(None))
    }

    fn with_parent(parent: Option<Arc<Inner>>) -> Self {
        Self {
            created: Instant::now(),
            initiated: AtomicBool::new(false),
            reason: AtomicU32::new(0),
            initiated_at_nanos: AtomicU64::new(0),
            active: AtomicUsize::new(0),
            shared: Mutex::new(Shared::default()),
            cv: Condvar::new(),
            parent,
        }
    }

    #[inline]
    pub(crate) fn is_initiated(&self) -> bool {
        self.initiated.load(Ordering::Acquire)
    }

    #[inline]
    pub(crate) fn reason(&self) -> Option<ShutdownReason> {
        if !self.is_initiated() {
            return None;
        }
        ShutdownReason::decode(self.reason.load(Ordering::Relaxed))
    }

    pub(crate) fn elapsed(&self) -> Option<Duration> {
        if !self.is_initiated() {
            return None;
        }
        let at = Duration::from_nanos(self.initiated_at_nanos.load(Ordering::Relaxed));
        Some(self.created.elapsed().saturating_sub(at))
    }

    /// Initiate shutdown. Returns `true` if this call performed the
    /// transition, `false` if the scope was already initiated.
    pub(crate) fn trigger(&self, reason: ShutdownReason) -> bool {
        if self.is_initiated() {
            return false;
        }
        let (wakers, children, cancellations) = {
            let mut shared = sync::lock(&self.shared);
            if self.initiated.load(Ordering::Relaxed) {
                return false;
            }
            let at = u64::try_from(self.created.elapsed().as_nanos()).unwrap_or(u64::MAX);
            self.reason.store(reason.encode(), Ordering::Relaxed);
            self.initiated_at_nanos.store(at, Ordering::Relaxed);
            self.initiated.store(true, Ordering::Release);
            (
                shared.initiated.take_all(),
                core::mem::take(&mut shared.children),
                take_cancellations(&mut shared),
            )
        };
        self.cv.notify_all();
        wake_all(wakers);
        cancel_all(cancellations);
        for child in &children {
            if let Some(child) = sync::upgrade(child) {
                let _transitioned = child.trigger(reason);
            }
        }
        true
    }

    pub(crate) fn wait_blocking(&self) {
        if self.is_initiated() {
            return;
        }
        let mut guard = sync::lock(&self.shared);
        while !self.is_initiated() {
            guard = sync::wait(&self.cv, guard);
        }
    }

    pub(crate) fn wait_blocking_timeout(&self, timeout: Duration) -> bool {
        if self.is_initiated() {
            return true;
        }
        let Some(deadline) = Instant::now().checked_add(timeout) else {
            self.wait_blocking();
            return true;
        };
        let mut guard = sync::lock(&self.shared);
        loop {
            if self.is_initiated() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            guard = sync::wait_timeout(&self.cv, guard, deadline - now);
        }
    }

    /// Poll for initiation, parking the task's waker under `key`.
    pub(crate) fn poll_initiated(&self, key: &mut Option<WaitKey>, cx: &Context<'_>) -> Poll<()> {
        if self.is_initiated() {
            return Poll::Ready(());
        }
        let mut shared = sync::lock(&self.shared);
        if self.is_initiated() {
            return Poll::Ready(());
        }
        shared.initiated.register(key, cx.waker());
        Poll::Pending
    }

    /// Forget a waker parked by [`poll_initiated`](Self::poll_initiated).
    pub(crate) fn cancel_initiated_wait(&self, key: WaitKey) {
        if self.is_initiated() {
            // `trigger` already drained the list; the key is stale.
            return;
        }
        sync::lock(&self.shared).initiated.remove(key);
    }

    /// Create a child scope: initiated when this scope is (with the
    /// same reason), or on its own without affecting this scope.
    pub(crate) fn child(this: &Arc<Self>) -> Arc<Self> {
        let child = Arc::new(Self::with_parent(Some(Arc::clone(this))));
        let inherited = {
            let mut shared = sync::lock(&this.shared);
            if this.is_initiated() {
                this.reason()
            } else {
                if shared.children.len() >= CHILD_PRUNE_THRESHOLD
                    && shared.children.len() == shared.children.capacity()
                {
                    shared.children.retain(|weak| !sync::is_dangling(weak));
                }
                shared.children.push(sync::downgrade(&child));
                None
            }
        };
        if let Some(reason) = inherited {
            let _transitioned = child.trigger(reason);
        }
        child
    }

    /// Register a cancellation token to cancel on initiation (cancels
    /// it immediately if already initiated).
    #[cfg(feature = "tokio-util")]
    pub(crate) fn link_cancellation(&self, token: tokio_util::sync::CancellationToken) {
        {
            let mut shared = sync::lock(&self.shared);
            if !self.is_initiated() {
                shared.cancellation.push(token);
                return;
            }
        }
        token.cancel();
    }

    // ---- subsystem tracking ------------------------------------------

    pub(crate) fn active_subsystems(&self) -> usize {
        self.active.load(Ordering::Acquire)
    }

    /// Count a new guard in this scope and every ancestor scope.
    pub(crate) fn acquire_subsystem(&self) {
        let mut node = Some(self);
        while let Some(scope) = node {
            let _previous = scope.active.fetch_add(1, Ordering::Relaxed);
            node = scope.parent.as_deref();
        }
    }

    /// Drop a guard counted by [`acquire_subsystem`](Self::acquire_subsystem).
    pub(crate) fn release_subsystem(&self) {
        let mut node = Some(self);
        while let Some(scope) = node {
            if scope.active.fetch_sub(1, Ordering::AcqRel) == 1 {
                scope.notify_drained();
            }
            node = scope.parent.as_deref();
        }
    }

    fn notify_drained(&self) {
        let wakers = sync::lock(&self.shared).drained.take_all();
        self.cv.notify_all();
        wake_all(wakers);
    }

    pub(crate) fn wait_drained_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let mut guard = sync::lock(&self.shared);
        loop {
            if self.active_subsystems() == 0 {
                return true;
            }
            match deadline {
                None => guard = sync::wait(&self.cv, guard),
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return false;
                    }
                    guard = sync::wait_timeout(&self.cv, guard, deadline - now);
                }
            }
        }
    }

    pub(crate) fn poll_drained(&self, key: &mut Option<WaitKey>, cx: &Context<'_>) -> Poll<()> {
        if self.active_subsystems() == 0 {
            return Poll::Ready(());
        }
        let mut shared = sync::lock(&self.shared);
        if self.active_subsystems() == 0 {
            return Poll::Ready(());
        }
        shared.drained.register(key, cx.waker());
        Poll::Pending
    }

    pub(crate) fn cancel_drained_wait(&self, key: WaitKey) {
        sync::lock(&self.shared).drained.remove(key);
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::signal::Signal;

    #[test]
    fn test_inner_reason_is_some_whenever_initiated() {
        let inner = Inner::new();
        assert_eq!(inner.reason(), None);
        assert!(inner.trigger(ShutdownReason::Signal(Signal::Quit)));
        assert!(inner.is_initiated());
        assert_eq!(inner.reason(), Some(ShutdownReason::Signal(Signal::Quit)));
        assert!(!inner.trigger(ShutdownReason::Requested));
        assert_eq!(inner.reason(), Some(ShutdownReason::Signal(Signal::Quit)));
    }

    #[test]
    fn test_inner_child_inherits_reason_from_parent() {
        let parent = Inner::new();
        let child = Inner::child(&parent);
        assert!(!child.is_initiated());
        assert!(parent.trigger(ShutdownReason::Timeout));
        assert_eq!(child.reason(), Some(ShutdownReason::Timeout));
    }

    #[test]
    fn test_inner_child_of_initiated_parent_starts_initiated() {
        let parent = Inner::new();
        assert!(parent.trigger(ShutdownReason::Error));
        let child = Inner::child(&parent);
        assert_eq!(child.reason(), Some(ShutdownReason::Error));
    }

    #[test]
    fn test_inner_child_trigger_does_not_reach_parent() {
        let parent = Inner::new();
        let child = Inner::child(&parent);
        assert!(child.trigger(ShutdownReason::Requested));
        assert!(!parent.is_initiated());
    }

    #[test]
    fn test_inner_dropped_children_are_pruned() {
        let parent = Inner::new();
        for _ in 0..(CHILD_PRUNE_THRESHOLD * 8) {
            let _child = Inner::child(&parent);
        }
        let len = sync::lock(&parent.shared).children.len();
        assert!(len <= CHILD_PRUNE_THRESHOLD * 2, "{len} dead children kept");
    }

    #[test]
    fn test_inner_subsystem_counts_propagate_to_ancestors() {
        let root = Inner::new();
        let child = Inner::child(&root);
        child.acquire_subsystem();
        assert_eq!(child.active_subsystems(), 1);
        assert_eq!(root.active_subsystems(), 1);
        assert!(!root.wait_drained_timeout(Duration::from_millis(1)));
        child.release_subsystem();
        assert_eq!(root.active_subsystems(), 0);
        assert!(root.wait_drained_timeout(Duration::from_millis(1)));
    }

    #[test]
    fn test_inner_wait_blocking_timeout_with_huge_timeout_still_returns_on_trigger() {
        let inner = Inner::new();
        let remote = Arc::clone(&inner);
        let handle = std::thread::spawn(move || remote.trigger(ShutdownReason::Requested));
        assert!(inner.wait_blocking_timeout(Duration::MAX));
        assert!(handle.join().unwrap_or(false));
    }

    #[test]
    fn test_inner_debug_reports_state() {
        let inner = Inner::new();
        let text = format!("{inner:?}");
        assert!(text.contains("initiated: false"));
    }
}
