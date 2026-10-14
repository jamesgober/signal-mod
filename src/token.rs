//! Cloneable observer and initiator handles, and the runtime-agnostic
//! futures they hand out.

use core::fmt;
use core::future::{Future, IntoFuture};
use core::pin::Pin;
use core::task::{Context, Poll};
use std::time::Duration;

use crate::reason::ShutdownReason;
use crate::state::Inner;
use crate::sync::Arc;
use crate::waitlist::WaitKey;

/// Cloneable observer handle.
///
/// Hand one of these to every subsystem that needs to react to
/// shutdown. Cloning costs one `Arc::clone`. A token can observe
/// shutdown but cannot initiate it; that capability lives in
/// [`ShutdownTrigger`].
///
/// Waiting works on any executor (and without one):
/// [`wait`](Self::wait) returns a [`ShutdownFuture`] driven by plain
/// waker notification, so it never polls on a timer. The token itself
/// implements [`IntoFuture`], so `token.await` works too, and
/// [`cancelled`](Self::cancelled) is the same future under the name
/// `tokio_util::sync::CancellationToken` uses.
///
/// # Examples
///
/// ```
/// use signal_mod::{Coordinator, ShutdownReason};
/// use std::time::Duration;
///
/// let coord = Coordinator::builder().build();
/// let token = coord.token();
/// let worker = std::thread::spawn({
///     let token = token.clone();
///     move || {
///         token.wait_blocking();
///         token.reason()
///     }
/// });
///
/// coord.trigger().trigger(ShutdownReason::Requested);
/// assert_eq!(worker.join().ok().flatten(), Some(ShutdownReason::Requested));
/// assert!(token.elapsed().unwrap_or_default() < Duration::from_secs(60));
/// ```
#[derive(Debug, Clone)]
pub struct ShutdownToken {
    inner: Arc<Inner>,
}

impl ShutdownToken {
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    /// `true` once shutdown has been initiated.
    ///
    /// Uses an `Acquire` load: everything the initiating thread did
    /// before calling [`ShutdownTrigger::trigger`] is visible to a
    /// thread that observes `true`, and [`reason`](Self::reason) is
    /// guaranteed to be `Some`.
    #[must_use]
    #[inline]
    pub fn is_initiated(&self) -> bool {
        self.inner.is_initiated()
    }

    /// Reason for the shutdown; `Some` exactly when
    /// [`is_initiated`](Self::is_initiated) is `true`. Lock-free.
    #[must_use]
    #[inline]
    pub fn reason(&self) -> Option<ShutdownReason> {
        self.inner.reason()
    }

    /// Wall-clock time since shutdown was initiated; `None` before.
    #[must_use]
    pub fn elapsed(&self) -> Option<Duration> {
        self.inner.elapsed()
    }

    /// Block the current thread until shutdown is initiated.
    ///
    /// Returns immediately if shutdown is already initiated. Do not
    /// call this from an async task; use [`wait`](Self::wait) there.
    pub fn wait_blocking(&self) {
        self.inner.wait_blocking();
    }

    /// Block the current thread for at most `timeout`.
    ///
    /// Returns `true` if shutdown was observed within the budget,
    /// `false` if the timeout elapsed first. `Duration::MAX` waits
    /// without a deadline.
    #[must_use = "the return value says whether shutdown was observed"]
    pub fn wait_blocking_timeout(&self, timeout: Duration) -> bool {
        self.inner.wait_blocking_timeout(timeout)
    }

    /// Future that resolves once shutdown is initiated (immediately if
    /// it already is).
    ///
    /// Runtime-agnostic: it works under Tokio, smol, async-std, the
    /// `futures` executor, or any other, with or without the `tokio`
    /// feature. The future owns a clone of the token, so it is
    /// `'static` and can be moved into spawned tasks or handed to
    /// `axum::serve(..).with_graceful_shutdown(..)`.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() {
    /// use signal_mod::{Coordinator, ShutdownReason};
    ///
    /// let coord = Coordinator::builder().build();
    /// let token = coord.token();
    /// let waiter = tokio::spawn(token.wait());
    ///
    /// coord.trigger().trigger(ShutdownReason::Requested);
    /// waiter.await.ok();
    /// assert!(token.is_initiated());
    /// # }
    /// ```
    #[must_use = "futures do nothing unless awaited"]
    pub fn wait(&self) -> ShutdownFuture {
        ShutdownFuture {
            inner: Arc::clone(&self.inner),
            key: None,
        }
    }

    /// Alias of [`wait`](Self::wait), named after
    /// `tokio_util::sync::CancellationToken::cancelled` so the token can
    /// stand in for a cancellation token in `select!` code.
    #[must_use = "futures do nothing unless awaited"]
    pub fn cancelled(&self) -> ShutdownFuture {
        self.wait()
    }

    /// Create a child scope.
    ///
    /// Returns a trigger and token for a new scope that is initiated
    /// when this scope is (with the same reason) or when its own
    /// trigger fires. Initiating the child does not affect this scope.
    /// Use it for a sub-tree that can be shut down on its own, such as
    /// one connection or one job, while still following the process-wide
    /// shutdown.
    ///
    /// Subsystem guards taken from a child token also count towards
    /// this scope (see [`subsystem_guard`](Self::subsystem_guard)).
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{Coordinator, ShutdownReason};
    ///
    /// let coord = Coordinator::builder().build();
    /// let (job_trigger, job_token) = coord.token().child();
    ///
    /// job_trigger.trigger(ShutdownReason::Requested);
    /// assert!(job_token.is_initiated());
    /// assert!(!coord.token().is_initiated());
    ///
    /// let (_t, other_job) = coord.token().child();
    /// coord.trigger().trigger(ShutdownReason::Timeout);
    /// assert_eq!(other_job.reason(), Some(ShutdownReason::Timeout));
    /// ```
    #[must_use]
    pub fn child(&self) -> (ShutdownTrigger, Self) {
        let child = Inner::child(&self.inner);
        (ShutdownTrigger::new(Arc::clone(&child)), Self::new(child))
    }

    /// Register a running subsystem.
    ///
    /// The scope counts as busy until the returned guard is dropped.
    /// After shutdown is initiated, [`subsystems_finished`](Self::subsystems_finished)
    /// or [`wait_subsystems_blocking`](Self::wait_subsystems_blocking)
    /// wait until every guard is gone: the "wait for all workers to
    /// stop" step of a graceful shutdown.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{Coordinator, ShutdownReason};
    /// use std::time::Duration;
    ///
    /// let coord = Coordinator::builder().build();
    /// let token = coord.token();
    ///
    /// let worker = std::thread::spawn({
    ///     let token = token.clone();
    ///     let guard = token.subsystem_guard();
    ///     move || {
    ///         token.wait_blocking();
    ///         // ... finish in-flight work ...
    ///         drop(guard);
    ///     }
    /// });
    ///
    /// coord.trigger().trigger(ShutdownReason::Requested);
    /// assert!(token.wait_subsystems_blocking(Duration::from_secs(5)));
    /// worker.join().ok();
    /// ```
    #[must_use = "the subsystem counts as finished as soon as the guard is dropped"]
    pub fn subsystem_guard(&self) -> SubsystemGuard {
        self.inner.acquire_subsystem();
        SubsystemGuard {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Number of live [`SubsystemGuard`]s in this scope and its child
    /// scopes.
    #[must_use]
    pub fn active_subsystems(&self) -> usize {
        self.inner.active_subsystems()
    }

    /// Future that resolves once no [`SubsystemGuard`] of this scope
    /// (or its child scopes) is alive. Runtime-agnostic and `'static`.
    ///
    /// Typically awaited after shutdown is initiated, with a timeout
    /// from your runtime.
    #[must_use = "futures do nothing unless awaited"]
    pub fn subsystems_finished(&self) -> SubsystemsFinished {
        SubsystemsFinished {
            inner: Arc::clone(&self.inner),
            key: None,
        }
    }

    /// Block until no [`SubsystemGuard`] of this scope is alive, for at
    /// most `timeout`. Returns `true` if they all finished in time.
    #[must_use = "the return value says whether every subsystem finished"]
    pub fn wait_subsystems_blocking(&self, timeout: Duration) -> bool {
        self.inner.wait_drained_timeout(timeout)
    }

    /// A `tokio_util` [`CancellationToken`](tokio_util::sync::CancellationToken)
    /// that is cancelled when this scope is initiated (immediately if
    /// it already is).
    ///
    /// Cancelling the returned token does not initiate shutdown: the
    /// link is one-way, matching the observer-only nature of a
    /// `ShutdownToken`.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{Coordinator, ShutdownReason};
    ///
    /// let coord = Coordinator::builder().build();
    /// let cancel = coord.token().cancellation_token();
    /// assert!(!cancel.is_cancelled());
    /// coord.trigger().trigger(ShutdownReason::Requested);
    /// assert!(cancel.is_cancelled());
    /// ```
    #[cfg(feature = "tokio-util")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tokio-util")))]
    #[must_use]
    pub fn cancellation_token(&self) -> tokio_util::sync::CancellationToken {
        let token = tokio_util::sync::CancellationToken::new();
        self.inner.link_cancellation(token.clone());
        token
    }
}

impl IntoFuture for ShutdownToken {
    type Output = ();
    type IntoFuture = ShutdownFuture;

    fn into_future(self) -> ShutdownFuture {
        ShutdownFuture {
            inner: self.inner,
            key: None,
        }
    }
}

impl IntoFuture for &ShutdownToken {
    type Output = ();
    type IntoFuture = ShutdownFuture;

    fn into_future(self) -> ShutdownFuture {
        self.wait()
    }
}

#[cfg(feature = "tokio-util")]
#[cfg_attr(docsrs, doc(cfg(feature = "tokio-util")))]
impl From<&ShutdownToken> for tokio_util::sync::CancellationToken {
    fn from(token: &ShutdownToken) -> Self {
        token.cancellation_token()
    }
}

/// Future returned by [`ShutdownToken::wait`], [`ShutdownToken::cancelled`]
/// and `ShutdownToken::into_future`. Resolves once shutdown is
/// initiated.
///
/// Wake-ups come straight from [`ShutdownTrigger::trigger`]; the future
/// never polls on a timer and works on any executor. Dropping it before
/// completion unregisters its waker.
#[must_use = "futures do nothing unless awaited"]
pub struct ShutdownFuture {
    inner: Arc<Inner>,
    key: Option<WaitKey>,
}

impl fmt::Debug for ShutdownFuture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShutdownFuture")
            .field("initiated", &self.inner.is_initiated())
            .finish_non_exhaustive()
    }
}

impl Future for ShutdownFuture {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        let ready = this.inner.poll_initiated(&mut this.key, cx);
        if ready.is_ready() {
            this.key = None;
        }
        ready
    }
}

impl Drop for ShutdownFuture {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.inner.cancel_initiated_wait(key);
        }
    }
}

/// Keeps a subsystem registered with its scope until dropped. Created by
/// [`ShutdownToken::subsystem_guard`].
#[must_use = "the subsystem counts as finished as soon as the guard is dropped"]
pub struct SubsystemGuard {
    inner: Arc<Inner>,
}

impl fmt::Debug for SubsystemGuard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubsystemGuard")
            .field("active_subsystems", &self.inner.active_subsystems())
            .finish()
    }
}

impl Drop for SubsystemGuard {
    fn drop(&mut self) {
        self.inner.release_subsystem();
    }
}

/// Future returned by [`ShutdownToken::subsystems_finished`]. Resolves
/// once no [`SubsystemGuard`] of the scope is alive.
#[must_use = "futures do nothing unless awaited"]
pub struct SubsystemsFinished {
    inner: Arc<Inner>,
    key: Option<WaitKey>,
}

impl fmt::Debug for SubsystemsFinished {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubsystemsFinished")
            .field("active_subsystems", &self.inner.active_subsystems())
            .finish_non_exhaustive()
    }
}

impl Future for SubsystemsFinished {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let this = self.get_mut();
        let ready = this.inner.poll_drained(&mut this.key, cx);
        if ready.is_ready() {
            if let Some(key) = this.key.take() {
                this.inner.cancel_drained_wait(key);
            }
        }
        ready
    }
}

impl Drop for SubsystemsFinished {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.inner.cancel_drained_wait(key);
        }
    }
}

/// Cloneable initiator handle.
///
/// Hand one of these to any code path that may need to ask for
/// shutdown (an admin endpoint, a fatal-error branch, a supervisor).
///
/// # Examples
///
/// ```
/// use signal_mod::{Coordinator, ShutdownReason};
///
/// let coord = Coordinator::builder().build();
/// let trigger = coord.trigger();
/// assert!(trigger.trigger(ShutdownReason::Requested));
/// assert!(!trigger.trigger(ShutdownReason::Error)); // already initiated
/// assert_eq!(coord.token().reason(), Some(ShutdownReason::Requested));
/// ```
#[derive(Debug, Clone)]
pub struct ShutdownTrigger {
    inner: Arc<Inner>,
}

impl ShutdownTrigger {
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    /// Initiate shutdown with the given reason.
    ///
    /// Returns `true` if this call performed the transition, `false` if
    /// shutdown was already initiated (the first reason wins). Wakes
    /// every waiter and initiates every child scope.
    ///
    /// The return value is informational; ignoring it is normal.
    #[allow(clippy::must_use_candidate)] // callers routinely ignore "was I first?"
    pub fn trigger(&self, reason: ShutdownReason) -> bool {
        self.inner.trigger(reason)
    }

    /// `true` once shutdown has been initiated.
    #[must_use]
    #[inline]
    pub fn is_initiated(&self) -> bool {
        self.inner.is_initiated()
    }

    /// An observer handle for the same scope.
    #[must_use]
    pub fn token(&self) -> ShutdownToken {
        ShutdownToken::new(Arc::clone(&self.inner))
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Wake, Waker};

    struct Count(AtomicUsize);
    impl Wake for Count {
        fn wake(self: std::sync::Arc<Self>) {
            let _previous = self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn poll_once<F: Future + Unpin>(fut: &mut F, waker: &Waker) -> Poll<F::Output> {
        let mut cx = Context::from_waker(waker);
        Pin::new(fut).poll(&mut cx)
    }

    #[test]
    fn test_shutdown_future_wakes_exactly_once_on_trigger() {
        let inner = Inner::new();
        let token = ShutdownToken::new(Arc::clone(&inner));
        let count = std::sync::Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(std::sync::Arc::clone(&count));

        let mut fut = token.wait();
        assert!(poll_once(&mut fut, &waker).is_pending());
        assert!(poll_once(&mut fut, &waker).is_pending());
        assert!(inner.trigger(ShutdownReason::Requested));
        assert_eq!(count.0.load(Ordering::SeqCst), 1);
        assert!(poll_once(&mut fut, &waker).is_ready());
    }

    #[test]
    fn test_shutdown_future_dropped_while_pending_unregisters() {
        let inner = Inner::new();
        let token = ShutdownToken::new(Arc::clone(&inner));
        let count = std::sync::Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(std::sync::Arc::clone(&count));
        {
            let mut fut = token.wait();
            assert!(poll_once(&mut fut, &waker).is_pending());
        }
        assert!(inner.trigger(ShutdownReason::Requested));
        assert_eq!(count.0.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn test_subsystems_finished_wakes_when_last_guard_drops() {
        let inner = Inner::new();
        let token = ShutdownToken::new(inner);
        let count = std::sync::Arc::new(Count(AtomicUsize::new(0)));
        let waker = Waker::from(std::sync::Arc::clone(&count));

        let a = token.subsystem_guard();
        let b = token.subsystem_guard();
        let mut fut = token.subsystems_finished();
        assert!(poll_once(&mut fut, &waker).is_pending());
        drop(a);
        assert_eq!(count.0.load(Ordering::SeqCst), 0);
        drop(b);
        assert_eq!(count.0.load(Ordering::SeqCst), 1);
        assert!(poll_once(&mut fut, &waker).is_ready());
        assert!(format!("{fut:?}").contains("active_subsystems: 0"));
    }

    #[test]
    fn test_trigger_token_observes_same_scope() {
        let inner = Inner::new();
        let trigger = ShutdownTrigger::new(inner);
        let token = trigger.token();
        assert!(trigger.trigger(ShutdownReason::Error));
        assert!(token.is_initiated());
        assert!(trigger.is_initiated());
    }
}
