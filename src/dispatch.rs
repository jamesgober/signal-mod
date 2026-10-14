//! Signal fan-out: shutdown initiation, second-signal escalation, and
//! subscriptions for non-shutdown signals.
//!
//! Every back-end funnels each delivered signal into
//! [`Dispatcher::deliver`]. The dispatcher is shared (behind an `Arc`)
//! between the coordinator and the back-end's listener tasks or thread.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use crate::reason::ShutdownReason;
use crate::signal::{Signal, SignalSet};
use crate::token::ShutdownTrigger;
use crate::waitlist::{WaitKey, WaitList, wake_all};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Routes delivered signals for one coordinator.
pub(crate) struct Dispatcher {
    trigger: ShutdownTrigger,
    shutdown: SignalSet,
    escalate: bool,
    subscribers: Mutex<Vec<Weak<Subscriber>>>,
}

impl fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dispatcher")
            .field("shutdown", &self.shutdown)
            .field("escalate", &self.escalate)
            .finish_non_exhaustive()
    }
}

impl Dispatcher {
    pub(crate) fn new(trigger: ShutdownTrigger, shutdown: SignalSet, escalate: bool) -> Self {
        Self {
            trigger,
            shutdown,
            escalate,
            subscribers: Mutex::new(Vec::new()),
        }
    }

    /// Handle one delivered signal.
    ///
    /// Subscribers see every delivered signal. A signal in the shutdown
    /// set initiates shutdown; if shutdown was already initiated and
    /// escalation is enabled, the process exits immediately with the
    /// signal's conventional status.
    pub(crate) fn deliver(&self, sig: Signal) {
        sm_info!(signal = %sig, "signal received");
        self.notify_subscribers(sig);
        if !self.shutdown.contains(sig) {
            return;
        }
        let first = self.trigger.trigger(ShutdownReason::Signal(sig));
        if !first && self.escalate {
            let code = sig.exit_code();
            sm_warn!(signal = %sig, code, "second shutdown signal; exiting immediately");
            std::process::exit(code);
        }
    }

    fn notify_subscribers(&self, sig: Signal) {
        let live: Vec<Arc<Subscriber>> = {
            let mut subs = lock(&self.subscribers);
            subs.retain(|weak| weak.strong_count() > 0);
            subs.iter().filter_map(Weak::upgrade).collect()
        };
        for sub in live {
            sub.push(sig);
        }
    }

    pub(crate) fn subscribe(&self) -> SignalSubscription {
        let sub = Arc::new(Subscriber::default());
        lock(&self.subscribers).push(Arc::downgrade(&sub));
        SignalSubscription { shared: sub }
    }
}

impl Drop for Dispatcher {
    fn drop(&mut self) {
        let subs = core::mem::take(&mut *lock(&self.subscribers));
        for sub in subs.iter().filter_map(Weak::upgrade) {
            sub.close();
        }
    }
}

#[derive(Debug)]
struct SubState {
    /// Pending signals, one bit per `Signal` (same layout as `SignalSet`).
    pending: SignalSet,
    closed: bool,
    waiters: WaitList,
}

#[derive(Debug, Default)]
struct Subscriber {
    state: Mutex<SubState>,
    cv: Condvar,
}

impl Default for SubState {
    fn default() -> Self {
        Self {
            pending: SignalSet::empty(),
            closed: false,
            waiters: WaitList::default(),
        }
    }
}

impl Subscriber {
    fn push(&self, sig: Signal) {
        let wakers = {
            let mut state = lock(&self.state);
            state.pending = state.pending.with(sig);
            state.waiters.take_all()
        };
        self.cv.notify_all();
        wake_all(wakers);
    }

    fn close(&self) {
        let wakers = {
            let mut state = lock(&self.state);
            state.closed = true;
            state.waiters.take_all()
        };
        self.cv.notify_all();
        wake_all(wakers);
    }
}

fn pop(state: &mut SubState) -> Option<Signal> {
    let sig = state.pending.iter().next()?;
    state.pending = state.pending.without(sig);
    Some(sig)
}

/// Receives the signals delivered to a coordinator, including the ones
/// configured with
/// [`CoordinatorBuilder::notify_signals`](crate::CoordinatorBuilder::notify_signals)
/// that do not start a shutdown (SIGHUP for "reload", SIGUSR1 for "reopen
/// logs", and so on).
///
/// Created by [`Coordinator::subscribe`](crate::Coordinator::subscribe).
/// Every subscription sees every delivered signal from the moment it is
/// created. Like the OS itself, a subscription coalesces: repeated
/// deliveries of the same signal that arrive before you receive it are
/// reported once. Distinct pending signals are received in canonical
/// [`Signal::ALL`] order. Nothing is ever dropped silently and memory
/// use is constant: one bit per signal.
///
/// The receive methods return `None` once the subscription is closed,
/// which happens when the [`Coordinator`](crate::Coordinator) and its
/// listeners are gone.
///
/// # Examples
///
/// ```
/// use signal_mod::{Coordinator, Signal, SignalSet};
///
/// let coord = Coordinator::builder()
///     .signals(SignalSet::graceful() - Signal::Hangup)
///     .notify_signals(SignalSet::from(Signal::Hangup))
///     .build();
/// let mut reloads = coord.subscribe();
///
/// // Simulate a delivery (an installed back-end does this for real).
/// coord.simulate_signal(Signal::Hangup);
/// assert_eq!(reloads.try_recv(), Some(Signal::Hangup));
/// assert!(!coord.token().is_initiated()); // SIGHUP did not stop the process
/// ```
pub struct SignalSubscription {
    shared: Arc<Subscriber>,
}

impl fmt::Debug for SignalSubscription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = lock(&self.shared.state);
        f.debug_struct("SignalSubscription")
            .field("pending", &state.pending)
            .field("closed", &state.closed)
            .finish()
    }
}

impl SignalSubscription {
    /// Take a pending signal without waiting. `None` if nothing is
    /// pending (or the subscription is closed and drained).
    #[must_use = "a received signal is consumed"]
    pub fn try_recv(&mut self) -> Option<Signal> {
        pop(&mut lock(&self.shared.state))
    }

    /// Block the current thread until a signal arrives. `None` once the
    /// subscription is closed and drained.
    #[must_use = "a received signal is consumed"]
    pub fn recv_blocking(&mut self) -> Option<Signal> {
        let mut state = lock(&self.shared.state);
        loop {
            if let Some(sig) = pop(&mut state) {
                return Some(sig);
            }
            if state.closed {
                return None;
            }
            state = match self.shared.cv.wait(state) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    /// Block for at most `timeout`. `None` if nothing arrived in time or
    /// the subscription is closed and drained (see
    /// [`is_closed`](Self::is_closed)).
    #[must_use = "a received signal is consumed"]
    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<Signal> {
        let deadline = Instant::now().checked_add(timeout);
        let mut state = lock(&self.shared.state);
        loop {
            if let Some(sig) = pop(&mut state) {
                return Some(sig);
            }
            if state.closed {
                return None;
            }
            let wait = match deadline {
                None => Duration::MAX,
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return None;
                    }
                    deadline - now
                }
            };
            state = match self.shared.cv.wait_timeout(state, wait) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    /// Wait for the next signal. Runtime-agnostic; resolves to `None`
    /// once the subscription is closed and drained.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// use signal_mod::{Coordinator, Signal, SignalSet};
    ///
    /// let coord = Coordinator::builder()
    ///     .notify_signals(SignalSet::from(Signal::User1))
    ///     .build();
    /// let mut sub = coord.subscribe();
    /// coord.simulate_signal(Signal::User1);
    /// assert_eq!(sub.recv().await, Some(Signal::User1));
    /// # }
    /// ```
    pub fn recv(&mut self) -> Recv<'_> {
        Recv {
            sub: self,
            key: None,
        }
    }

    /// `true` once the coordinator and its listeners are gone; pending
    /// signals can still be drained.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        lock(&self.shared.state).closed
    }
}

/// Future returned by [`SignalSubscription::recv`].
#[must_use = "futures do nothing unless awaited"]
pub struct Recv<'a> {
    sub: &'a mut SignalSubscription,
    key: Option<WaitKey>,
}

impl fmt::Debug for Recv<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Recv").finish_non_exhaustive()
    }
}

impl Future for Recv<'_> {
    type Output = Option<Signal>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Signal>> {
        let this = self.get_mut();
        let mut state = lock(&this.sub.shared.state);
        if let Some(sig) = pop(&mut state) {
            this.key = None;
            return Poll::Ready(Some(sig));
        }
        if state.closed {
            this.key = None;
            return Poll::Ready(None);
        }
        state.waiters.register(&mut this.key, cx.waker());
        Poll::Pending
    }
}

impl Drop for Recv<'_> {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            lock(&self.sub.shared.state).waiters.remove(key);
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::state::Inner;

    fn dispatcher(shutdown: SignalSet) -> (Arc<Inner>, Dispatcher) {
        let inner = Inner::new();
        let trigger = ShutdownTrigger::new(Arc::clone(&inner));
        (inner, Dispatcher::new(trigger, shutdown, false))
    }

    #[test]
    fn test_deliver_shutdown_signal_initiates_with_signal_reason() {
        let (inner, d) = dispatcher(SignalSet::graceful());
        d.deliver(Signal::Terminate);
        assert_eq!(
            inner.reason(),
            Some(ShutdownReason::Signal(Signal::Terminate))
        );
    }

    #[test]
    fn test_deliver_notify_only_signal_does_not_initiate() {
        let (inner, d) = dispatcher(SignalSet::empty().with(Signal::Terminate));
        let mut sub = d.subscribe();
        d.deliver(Signal::Hangup);
        assert!(!inner.is_initiated());
        assert_eq!(sub.try_recv(), Some(Signal::Hangup));
        assert_eq!(sub.try_recv(), None);
    }

    #[test]
    fn test_subscription_coalesces_repeats_and_orders_canonically() {
        let (_inner, d) = dispatcher(SignalSet::empty());
        let mut sub = d.subscribe();
        d.deliver(Signal::User2);
        d.deliver(Signal::Hangup);
        d.deliver(Signal::User2);
        assert_eq!(sub.try_recv(), Some(Signal::Hangup));
        assert_eq!(sub.try_recv(), Some(Signal::User2));
        assert_eq!(sub.try_recv(), None);
    }

    #[test]
    fn test_subscription_closes_when_dispatcher_drops() {
        let (_inner, d) = dispatcher(SignalSet::empty());
        let mut sub = d.subscribe();
        d.deliver(Signal::User1);
        drop(d);
        assert!(sub.is_closed());
        assert_eq!(sub.recv_blocking(), Some(Signal::User1));
        assert_eq!(sub.recv_blocking(), None);
        assert_eq!(sub.recv_timeout(Duration::from_secs(5)), None);
    }

    #[test]
    fn test_recv_timeout_expires_without_delivery() {
        let (_inner, d) = dispatcher(SignalSet::empty());
        let mut sub = d.subscribe();
        let start = Instant::now();
        assert_eq!(sub.recv_timeout(Duration::from_millis(20)), None);
        assert!(start.elapsed() >= Duration::from_millis(20));
        assert!(!sub.is_closed());
    }

    #[test]
    fn test_recv_blocking_wakes_on_delivery_from_other_thread() {
        let (_inner, d) = dispatcher(SignalSet::empty());
        let d = Arc::new(d);
        let mut sub = d.subscribe();
        let remote = Arc::clone(&d);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            remote.deliver(Signal::Quit);
        });
        assert_eq!(sub.recv_blocking(), Some(Signal::Quit));
        assert!(handle.join().is_ok());
        assert!(format!("{sub:?}").contains("closed: false"));
    }

    #[test]
    fn test_dropped_subscriptions_are_pruned_on_delivery() {
        let (_inner, d) = dispatcher(SignalSet::empty());
        for _ in 0..10 {
            let _sub = d.subscribe();
        }
        d.deliver(Signal::User1);
        assert_eq!(lock(&d.subscribers).len(), 0);
    }
}
