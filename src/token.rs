//! Cloneable observer and initiator handles.

use std::sync::Arc;
use std::time::Duration;

use crate::reason::ShutdownReason;
use crate::state::Inner;

/// Cloneable observer handle.
///
/// Hand one of these to every subsystem that needs to react to
/// shutdown. The handle is cheap to clone (single `Arc::clone`).
#[derive(Debug, Clone)]
pub struct ShutdownToken {
    inner: Arc<Inner>,
}

impl ShutdownToken {
    pub(crate) fn new(inner: Arc<Inner>) -> Self {
        Self { inner }
    }

    /// `true` if shutdown has been initiated.
    #[must_use]
    pub fn is_initiated(&self) -> bool {
        self.inner.is_initiated()
    }

    /// Reason for the shutdown, if one has been initiated.
    #[must_use]
    pub fn reason(&self) -> Option<ShutdownReason> {
        self.inner.reason()
    }

    /// Wall-clock time since shutdown was initiated.
    #[must_use]
    pub fn elapsed(&self) -> Option<Duration> {
        self.inner.elapsed()
    }

    /// Block the current thread until shutdown is initiated.
    ///
    /// Returns immediately if shutdown is already initiated.
    pub fn wait_blocking(&self) {
        self.inner.wait_blocking();
    }

    /// Block the current thread for at most `timeout`.
    ///
    /// Returns `true` if shutdown was observed within the budget,
    /// `false` if the timeout elapsed first.
    pub fn wait_blocking_timeout(&self, timeout: Duration) -> bool {
        self.inner.wait_blocking_timeout(timeout)
    }

    /// Async wait point. Returns once shutdown is initiated.
    ///
    /// Requires the `tokio` feature; the `async-std` variant takes
    /// precedence only when `tokio` is not enabled.
    #[cfg(feature = "tokio")]
    #[cfg_attr(docsrs, doc(cfg(feature = "tokio")))]
    pub async fn wait(&self) {
        if self.inner.is_initiated() {
            return;
        }
        let mut rx = self.inner.tx.subscribe();
        if self.inner.is_initiated() {
            return;
        }
        let _ = rx.recv().await;
    }

    /// Async wait point. Returns once shutdown is initiated.
    ///
    /// Compiled only with the `async-std` feature and when `tokio`
    /// is not enabled.
    #[cfg(all(feature = "async-std", not(feature = "tokio")))]
    #[cfg_attr(docsrs, doc(cfg(feature = "async-std")))]
    pub async fn wait(&self) {
        let mut poll = Duration::from_millis(1);
        let cap = Duration::from_millis(50);
        while !self.inner.is_initiated() {
            async_std::task::sleep(poll).await;
            poll = (poll * 2).min(cap);
        }
    }
}

/// Cloneable initiator handle.
///
/// Hand one of these to any code path that may need to ask for
/// shutdown.
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
    /// Returns `true` if this call performed the transition;
    /// `false` if it was already initiated.
    pub fn trigger(&self, reason: ShutdownReason) -> bool {
        self.inner.trigger(reason)
    }

    /// `true` if shutdown has been initiated.
    #[must_use]
    pub fn is_initiated(&self) -> bool {
        self.inner.is_initiated()
    }
}
