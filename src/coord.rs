//! The [`Coordinator`], its [`builder`](CoordinatorBuilder), and
//! the [`Statistics`] snapshot type.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::error::{Error, Result};
use crate::hook::ShutdownHook;
use crate::reason::ShutdownReason;
use crate::signal::SignalSet;
use crate::state::Inner;
use crate::token::{ShutdownToken, ShutdownTrigger};

#[cfg(any(feature = "tokio", all(feature = "async-std", not(feature = "tokio")),))]
use crate::signal::Signal;

/// Default graceful timeout: 5 seconds.
const DEFAULT_GRACEFUL_MS: u64 = 5_000;

/// Default force timeout: 10 seconds.
const DEFAULT_FORCE_MS: u64 = 10_000;

/// Owns the shutdown state machine, hook list, and (optionally) the
/// installed signal handlers.
///
/// Construct with [`Coordinator::builder`]. After construction:
///
/// 1. Optionally call [`install`](Coordinator::install) to register
///    OS-level signal handlers for the configured [`SignalSet`].
/// 2. Hand out [`token`](Coordinator::token) clones to subsystems
///    that observe shutdown, and [`trigger`](Coordinator::trigger)
///    clones to code paths that may initiate shutdown.
/// 3. Once shutdown is initiated, call
///    [`run_hooks`](Coordinator::run_hooks) to execute registered
///    cleanup in priority order under the graceful budget.
pub struct Coordinator {
    inner: Arc<Inner>,
    signals: SignalSet,
    graceful_timeout: Duration,
    force_timeout: Duration,
    hooks: Mutex<Vec<Box<dyn ShutdownHook>>>,
    installed: AtomicBool,
    hooks_completed: AtomicUsize,
}

impl core::fmt::Debug for Coordinator {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Coordinator")
            .field("signals", &self.signals)
            .field("graceful_timeout", &self.graceful_timeout)
            .field("force_timeout", &self.force_timeout)
            .field(
                "hooks",
                &format_args!("[{} hook(s)]", self.hooks.lock().len()),
            )
            .field("installed", &self.installed.load(Ordering::Relaxed))
            .finish()
    }
}

impl Coordinator {
    /// Start a new [`CoordinatorBuilder`] with default configuration.
    #[must_use]
    pub fn builder() -> CoordinatorBuilder {
        CoordinatorBuilder::new()
    }

    /// Create a new observer handle.
    #[must_use]
    pub fn token(&self) -> ShutdownToken {
        ShutdownToken::new(Arc::clone(&self.inner))
    }

    /// Create a new initiator handle.
    #[must_use]
    pub fn trigger(&self) -> ShutdownTrigger {
        ShutdownTrigger::new(Arc::clone(&self.inner))
    }

    /// Configured signal set.
    #[must_use]
    pub fn signals(&self) -> SignalSet {
        self.signals
    }

    /// Configured graceful timeout.
    #[must_use]
    pub fn graceful_timeout(&self) -> Duration {
        self.graceful_timeout
    }

    /// Configured force timeout.
    #[must_use]
    pub fn force_timeout(&self) -> Duration {
        self.force_timeout
    }

    /// `true` if [`install`](Self::install) has been called.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.installed.load(Ordering::Relaxed)
    }

    /// Snapshot of the current shutdown state.
    #[must_use]
    pub fn statistics(&self) -> Statistics {
        let hooks_registered = self.hooks.lock().len();
        let hooks_completed = self.hooks_completed.load(Ordering::Relaxed);
        Statistics {
            initiated: self.inner.is_initiated(),
            reason: self.inner.reason(),
            hooks_registered,
            hooks_completed,
            elapsed: self.inner.elapsed(),
        }
    }

    /// Run registered hooks in descending priority order under the
    /// graceful timeout budget. Returns the number of hooks that
    /// completed before the budget elapsed.
    ///
    /// Hooks are sorted on the first call and again on each
    /// subsequent call (the list is small; the cost is negligible).
    /// Within a priority, insertion order is preserved.
    pub fn run_hooks(&self, reason: ShutdownReason) -> usize {
        let mut hooks = self.hooks.lock();
        hooks.sort_by_key(|h| core::cmp::Reverse(h.priority()));
        let start = Instant::now();
        let mut count = 0usize;
        for hook in hooks.iter() {
            if start.elapsed() > self.graceful_timeout {
                break;
            }
            hook.run(reason);
            count += 1;
            self.hooks_completed.fetch_add(1, Ordering::Relaxed);
        }
        count
    }

    /// Install OS-level signal handlers for the configured set.
    ///
    /// The implementation is selected at compile time by feature
    /// flags:
    ///
    /// - `tokio` (default): spawns background `tokio` tasks. Must be
    ///   called from inside a `tokio` runtime context.
    /// - `async-std`: spawns background `async-std` tasks. Must be
    ///   called from inside an `async-std` runtime context.
    /// - `ctrlc-fallback`: installs a synchronous `ctrlc` handler.
    ///   Covers `Signal::Interrupt` only.
    /// - No feature: returns [`Error::NoRuntime`].
    ///
    /// `tokio` takes precedence over `async-std`.
    ///
    /// # Errors
    ///
    /// - [`Error::AlreadyInstalled`] if this coordinator has already
    ///   installed handlers.
    /// - [`Error::SignalRegistration`] if the platform rejects a
    ///   specific signal.
    /// - [`Error::NoRuntime`] if no back-end feature is enabled.
    pub fn install(&self) -> Result<()> {
        if self
            .installed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(Error::AlreadyInstalled);
        }

        let result = self.install_impl();

        if result.is_err() {
            self.installed.store(false, Ordering::Release);
        }
        result
    }

    #[cfg(feature = "tokio")]
    fn install_impl(&self) -> Result<()> {
        install_tokio(self)
    }

    #[cfg(all(feature = "async-std", not(feature = "tokio")))]
    fn install_impl(&self) -> Result<()> {
        install_async_std(self)
    }

    #[cfg(all(
        feature = "ctrlc-fallback",
        not(feature = "tokio"),
        not(feature = "async-std")
    ))]
    fn install_impl(&self) -> Result<()> {
        install_ctrlc(self)
    }

    #[cfg(not(any(feature = "tokio", feature = "async-std", feature = "ctrlc-fallback")))]
    #[allow(clippy::unused_self)]
    fn install_impl(&self) -> Result<()> {
        Err(Error::NoRuntime)
    }
}

/// Builder for [`Coordinator`].
pub struct CoordinatorBuilder {
    signals: SignalSet,
    graceful_timeout: Duration,
    force_timeout: Duration,
    hooks: Vec<Box<dyn ShutdownHook>>,
}

impl core::fmt::Debug for CoordinatorBuilder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CoordinatorBuilder")
            .field("signals", &self.signals)
            .field("graceful_timeout", &self.graceful_timeout)
            .field("force_timeout", &self.force_timeout)
            .field("hooks", &format_args!("[{} hook(s)]", self.hooks.len()))
            .finish()
    }
}

impl CoordinatorBuilder {
    /// Start a new builder with the default configuration:
    /// [`SignalSet::graceful`], 5s graceful, 10s force, no hooks.
    #[must_use]
    pub fn new() -> Self {
        Self {
            signals: SignalSet::graceful(),
            graceful_timeout: Duration::from_millis(DEFAULT_GRACEFUL_MS),
            force_timeout: Duration::from_millis(DEFAULT_FORCE_MS),
            hooks: Vec::new(),
        }
    }

    /// Set the signal set the coordinator will install handlers for.
    #[must_use]
    pub fn signals(mut self, set: SignalSet) -> Self {
        self.signals = set;
        self
    }

    /// Set the graceful-shutdown timeout. Hooks have at most this
    /// long, in aggregate, before remaining hooks are skipped.
    #[must_use]
    pub fn graceful_timeout(mut self, d: Duration) -> Self {
        self.graceful_timeout = d;
        self
    }

    /// Set the force-shutdown timeout. Exposed for downstream
    /// consumers that implement their own forced ladder; the
    /// coordinator itself does not enforce it.
    #[must_use]
    pub fn force_timeout(mut self, d: Duration) -> Self {
        self.force_timeout = d;
        self
    }

    /// Register a [`ShutdownHook`].
    #[must_use]
    pub fn hook<H: ShutdownHook>(mut self, h: H) -> Self {
        self.hooks.push(Box::new(h));
        self
    }

    /// Finalize into a [`Coordinator`].
    #[must_use]
    pub fn build(self) -> Coordinator {
        Coordinator {
            inner: Inner::new(),
            signals: self.signals,
            graceful_timeout: self.graceful_timeout,
            force_timeout: self.force_timeout,
            hooks: Mutex::new(self.hooks),
            installed: AtomicBool::new(false),
            hooks_completed: AtomicUsize::new(0),
        }
    }
}

impl Default for CoordinatorBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Snapshot of coordinator state, returned by
/// [`Coordinator::statistics`].
#[derive(Debug, Clone)]
pub struct Statistics {
    /// `true` if shutdown has been initiated.
    pub initiated: bool,
    /// Reason carried with the trigger that initiated shutdown.
    pub reason: Option<ShutdownReason>,
    /// Number of hooks registered on the coordinator.
    pub hooks_registered: usize,
    /// Number of hook runs completed across all `run_hooks` calls.
    pub hooks_completed: usize,
    /// Wall-clock time since shutdown was initiated.
    pub elapsed: Option<Duration>,
}

// --------------------------------------------------------------------
// Tokio back-end
// --------------------------------------------------------------------

#[cfg(feature = "tokio")]
fn install_tokio(coord: &Coordinator) -> Result<()> {
    let trigger = coord.trigger();
    let set = coord.signals;

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        macro_rules! reg {
            ($sig:expr, $kind:expr) => {{
                if set.contains($sig) {
                    let mut stream = signal($kind).map_err(|e| Error::SignalRegistration {
                        signal: $sig,
                        source: e,
                    })?;
                    let t = trigger.clone();
                    tokio::spawn(async move {
                        while stream.recv().await.is_some() {
                            t.trigger(ShutdownReason::Signal($sig));
                        }
                    });
                }
            }};
        }

        reg!(Signal::Terminate, SignalKind::terminate());
        reg!(Signal::Interrupt, SignalKind::interrupt());
        reg!(Signal::Quit, SignalKind::quit());
        reg!(Signal::Hangup, SignalKind::hangup());
        reg!(Signal::Pipe, SignalKind::pipe());
        reg!(Signal::User1, SignalKind::user_defined1());
        reg!(Signal::User2, SignalKind::user_defined2());
    }

    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_shutdown};

        if set.contains(Signal::Interrupt) {
            let mut s = ctrl_c().map_err(|e| Error::SignalRegistration {
                signal: Signal::Interrupt,
                source: e,
            })?;
            let t = trigger.clone();
            tokio::spawn(async move {
                while s.recv().await.is_some() {
                    t.trigger(ShutdownReason::Signal(Signal::Interrupt));
                }
            });
        }
        if set.contains(Signal::Quit) {
            let mut s = ctrl_break().map_err(|e| Error::SignalRegistration {
                signal: Signal::Quit,
                source: e,
            })?;
            let t = trigger.clone();
            tokio::spawn(async move {
                while s.recv().await.is_some() {
                    t.trigger(ShutdownReason::Signal(Signal::Quit));
                }
            });
        }
        if set.contains(Signal::Terminate) {
            let mut s = ctrl_close().map_err(|e| Error::SignalRegistration {
                signal: Signal::Terminate,
                source: e,
            })?;
            let t = trigger.clone();
            tokio::spawn(async move {
                while s.recv().await.is_some() {
                    t.trigger(ShutdownReason::Signal(Signal::Terminate));
                }
            });
        }
        if set.contains(Signal::Hangup) {
            let mut s = ctrl_shutdown().map_err(|e| Error::SignalRegistration {
                signal: Signal::Hangup,
                source: e,
            })?;
            let t = trigger.clone();
            tokio::spawn(async move {
                while s.recv().await.is_some() {
                    t.trigger(ShutdownReason::Signal(Signal::Hangup));
                }
            });
        }
        // Pipe / User1 / User2 are inert on Windows; skip silently.
        let _ = &trigger;
    }

    Ok(())
}

// --------------------------------------------------------------------
// async-std back-end
// --------------------------------------------------------------------

#[cfg(all(feature = "async-std", not(feature = "tokio")))]
fn install_async_std(coord: &Coordinator) -> Result<()> {
    let trigger = coord.trigger();
    let set = coord.signals;

    #[cfg(unix)]
    {
        use futures::stream::StreamExt;
        use signal_hook_async_std::Signals as SHSignals;

        let mut signum_to_variant: Vec<(i32, Signal)> = Vec::new();
        for sig in set.iter() {
            if let Some(n) = sig.unix_number() {
                signum_to_variant.push((n, sig));
            }
        }

        let nums: Vec<i32> = signum_to_variant.iter().map(|(n, _)| *n).collect();
        if !nums.is_empty() {
            let signals = SHSignals::new(&nums).map_err(|e| {
                let first = signum_to_variant
                    .first()
                    .map(|(_, s)| *s)
                    .unwrap_or(Signal::Terminate);
                Error::SignalRegistration {
                    signal: first,
                    source: e,
                }
            })?;
            let t = trigger.clone();
            async_std::task::spawn(async move {
                let mut signals = signals;
                while let Some(num) = signals.next().await {
                    if let Some(sig) = signum_to_variant
                        .iter()
                        .find(|(n, _)| *n == num)
                        .map(|(_, s)| *s)
                    {
                        t.trigger(ShutdownReason::Signal(sig));
                    }
                }
            });
        }
    }

    #[cfg(windows)]
    {
        if set.contains(Signal::Interrupt) {
            let t = trigger.clone();
            ctrlc::try_set_handler(move || {
                let _ = t.trigger(ShutdownReason::Signal(Signal::Interrupt));
            })
            .map_err(|e| Error::SignalRegistration {
                signal: Signal::Interrupt,
                source: std::io::Error::other(e),
            })?;
        }
        let _ = &trigger;
    }

    Ok(())
}

// --------------------------------------------------------------------
// ctrlc fallback (no async runtime)
// --------------------------------------------------------------------

#[cfg(all(
    feature = "ctrlc-fallback",
    not(feature = "tokio"),
    not(feature = "async-std")
))]
fn install_ctrlc(coord: &Coordinator) -> Result<()> {
    use crate::signal::Signal;
    let trigger = coord.trigger();
    if coord.signals.contains(Signal::Interrupt) {
        ctrlc::try_set_handler(move || {
            let _ = trigger.trigger(ShutdownReason::Signal(Signal::Interrupt));
        })
        .map_err(|e| Error::SignalRegistration {
            signal: Signal::Interrupt,
            source: std::io::Error::other(e),
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use crate::hook::hook_from_fn;

    #[test]
    fn builder_defaults() {
        let c = Coordinator::builder().build();
        assert_eq!(c.signals(), SignalSet::graceful());
        assert_eq!(c.graceful_timeout(), Duration::from_millis(5_000));
        assert_eq!(c.force_timeout(), Duration::from_millis(10_000));
        assert!(!c.is_installed());
        let stats = c.statistics();
        assert!(!stats.initiated);
        assert_eq!(stats.hooks_registered, 0);
        assert_eq!(stats.hooks_completed, 0);
    }

    #[test]
    fn token_observes_trigger() {
        let c = Coordinator::builder().build();
        let token = c.token();
        let trigger = c.trigger();
        assert!(!token.is_initiated());
        assert!(trigger.trigger(ShutdownReason::Requested));
        assert!(token.is_initiated());
        assert_eq!(token.reason(), Some(ShutdownReason::Requested));
        assert!(!trigger.trigger(ShutdownReason::Forced));
        assert_eq!(token.reason(), Some(ShutdownReason::Requested));
    }

    #[test]
    fn hooks_run_in_priority_order() {
        let order = Arc::new(parking_lot::Mutex::new(Vec::<i32>::new()));

        let push = |p: i32, order: &Arc<parking_lot::Mutex<Vec<i32>>>| {
            let o = Arc::clone(order);
            hook_from_fn(format!("p{p}"), p, move |_| {
                o.lock().push(p);
            })
        };

        let c = Coordinator::builder()
            .hook(push(0, &order))
            .hook(push(100, &order))
            .hook(push(50, &order))
            .build();

        let count = c.run_hooks(ShutdownReason::Requested);
        assert_eq!(count, 3);
        assert_eq!(*order.lock(), vec![100, 50, 0]);
        assert_eq!(c.statistics().hooks_completed, 3);
    }

    #[test]
    fn hooks_respect_graceful_budget() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c1 = Arc::clone(&counter);
        let c2 = Arc::clone(&counter);

        let slow = hook_from_fn("slow", 100, move |_| {
            c1.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(30));
        });
        let later = hook_from_fn("later", 0, move |_| {
            c2.fetch_add(1, Ordering::Relaxed);
        });

        let c = Coordinator::builder()
            .graceful_timeout(Duration::from_millis(5))
            .hook(slow)
            .hook(later)
            .build();

        let count = c.run_hooks(ShutdownReason::Requested);
        assert_eq!(count, 1);
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn elapsed_increases_after_trigger() {
        let c = Coordinator::builder().build();
        let token = c.token();
        assert!(token.elapsed().is_none());
        let _ = c.trigger().trigger(ShutdownReason::Requested);
        let first = token.elapsed().unwrap();
        std::thread::sleep(Duration::from_millis(5));
        let second = token.elapsed().unwrap();
        assert!(second >= first);
    }

    #[test]
    fn wait_blocking_timeout_returns_false_on_expiry() {
        let c = Coordinator::builder().build();
        let token = c.token();
        assert!(!token.wait_blocking_timeout(Duration::from_millis(5)));
    }

    #[test]
    fn wait_blocking_timeout_returns_true_on_trigger() {
        let c = Coordinator::builder().build();
        let token = c.token();
        let trigger = c.trigger();

        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            trigger.trigger(ShutdownReason::Requested);
        });

        assert!(token.wait_blocking_timeout(Duration::from_secs(1)));
        handle.join().unwrap();
    }

    #[cfg(not(any(feature = "tokio", feature = "async-std", feature = "ctrlc-fallback")))]
    #[test]
    fn install_errors_with_no_runtime() {
        let c = Coordinator::builder().build();
        assert!(matches!(c.install(), Err(Error::NoRuntime)));
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn token_wait_resolves_on_trigger() {
        let c = Coordinator::builder().build();
        let token = c.token();
        let trigger = c.trigger();

        let waiter = tokio::spawn(async move { token.wait().await });
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(trigger.trigger(ShutdownReason::Requested));
        let _ = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("wait did not resolve within 1s");
    }
}
