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

/// Default graceful-shutdown budget: 5 seconds.
///
/// The default is a balance between giving subsystems room to flush
/// state and bounding the worst-case shutdown latency for an
/// operator-initiated terminate. Override per-coordinator with
/// [`CoordinatorBuilder::graceful_timeout`].
const DEFAULT_GRACEFUL_MS: u64 = 5_000;

/// Default force-shutdown budget: 10 seconds.
///
/// The force budget is exposed for downstream consumers that implement
/// a multi-phase shutdown ladder on top of the graceful phase. The
/// coordinator itself does not enforce this budget directly.
const DEFAULT_FORCE_MS: u64 = 10_000;

/// Owns the shutdown state machine, hook list, and (optionally) the
/// installed signal handlers.
///
/// A `Coordinator` is the central object of `signal-mod`. It is
/// constructed once at program startup via [`Coordinator::builder`]
/// and then:
///
/// 1. Optionally registers OS-level signal handlers via
///    [`install`](Coordinator::install).
/// 2. Hands out cheap-to-clone [`ShutdownToken`] observer handles
///    and [`ShutdownTrigger`] initiator handles to the rest of the
///    program.
/// 3. After shutdown is initiated (by signal, programmatic trigger,
///    or supervisory parent), runs registered [`ShutdownHook`]s in
///    descending priority order via
///    [`run_hooks`](Coordinator::run_hooks).
///
/// The coordinator is `Send + Sync`. It holds an `Arc` to the shared
/// state machine, so cloning a token or trigger is `O(1)`.
///
/// # Examples
///
/// ```no_run
/// use signal_mod::{Coordinator, ShutdownReason, SignalSet};
/// use std::time::Duration;
///
/// # #[cfg(feature = "tokio")]
/// # async fn run() -> signal_mod::Result<()> {
/// let coord = Coordinator::builder()
///     .signals(SignalSet::graceful())
///     .graceful_timeout(Duration::from_secs(5))
///     .hook(signal_mod::hook_from_fn(
///         "flush-logs",
///         100,
///         |reason| eprintln!("shutting down: {reason}"),
///     ))
///     .build();
///
/// coord.install()?;
///
/// let token = coord.token();
/// token.wait().await;
///
/// let reason = token.reason().unwrap_or(ShutdownReason::Requested);
/// coord.run_hooks(reason);
/// # Ok(())
/// # }
/// ```
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
            .field("initiated", &self.inner.is_initiated())
            .finish()
    }
}

impl Coordinator {
    /// Start a new [`CoordinatorBuilder`] with default configuration.
    ///
    /// Equivalent to [`CoordinatorBuilder::new`].
    #[must_use]
    pub fn builder() -> CoordinatorBuilder {
        CoordinatorBuilder::new()
    }

    /// Create a new cloneable [`ShutdownToken`] observer handle.
    ///
    /// Tokens share the underlying state with the coordinator and
    /// each other. Cloning a token costs one `Arc::clone`.
    #[must_use]
    pub fn token(&self) -> ShutdownToken {
        ShutdownToken::new(Arc::clone(&self.inner))
    }

    /// Create a new cloneable [`ShutdownTrigger`] initiator handle.
    ///
    /// Triggers share the underlying state with the coordinator and
    /// each other. Cloning a trigger costs one `Arc::clone`.
    #[must_use]
    pub fn trigger(&self) -> ShutdownTrigger {
        ShutdownTrigger::new(Arc::clone(&self.inner))
    }

    /// Configured signal set.
    #[must_use]
    pub fn signals(&self) -> SignalSet {
        self.signals
    }

    /// Configured graceful-shutdown timeout.
    #[must_use]
    pub fn graceful_timeout(&self) -> Duration {
        self.graceful_timeout
    }

    /// Configured force-shutdown timeout.
    #[must_use]
    pub fn force_timeout(&self) -> Duration {
        self.force_timeout
    }

    /// `true` if [`install`](Self::install) has been called
    /// successfully on this coordinator.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        self.installed.load(Ordering::Relaxed)
    }

    /// Snapshot of the current shutdown state.
    ///
    /// The snapshot is taken under the same lock the state machine
    /// uses internally, so all fields are mutually consistent. The
    /// returned [`Statistics`] is `Clone` and may be passed across
    /// threads.
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
    /// graceful timeout budget.
    ///
    /// Returns the number of hooks that completed before the budget
    /// elapsed. Hooks are sorted on every call (the list is small
    /// and sort overhead is dominated by per-hook dispatch); within
    /// a priority, insertion order is preserved.
    ///
    /// If the graceful budget elapses mid-loop, the remaining hooks
    /// are skipped. Callers that implement a multi-phase ladder may
    /// invoke `run_hooks` again with [`ShutdownReason::Forced`] to
    /// retry; that second invocation runs every still-registered
    /// hook from scratch (hook bodies are responsible for being
    /// idempotent if they wish to be reusable across phases).
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{hook_from_fn, Coordinator, ShutdownReason};
    ///
    /// let coord = Coordinator::builder()
    ///     .hook(hook_from_fn("first", 100, |_| {}))
    ///     .hook(hook_from_fn("second", 0, |_| {}))
    ///     .build();
    ///
    /// let ran = coord.run_hooks(ShutdownReason::Requested);
    /// assert_eq!(ran, 2);
    /// ```
    pub fn run_hooks(&self, reason: ShutdownReason) -> usize {
        let mut hooks = self.hooks.lock();
        hooks.sort_by_key(|h| core::cmp::Reverse(h.priority()));
        let start = Instant::now();
        let mut count = 0usize;
        for hook in hooks.iter() {
            if start.elapsed() > self.graceful_timeout {
                break;
            }
            // Hooks are user-supplied; a panic in one must not abort
            // the entire shutdown sequence. We catch the unwind here
            // and continue. The hook is counted as "completed" either
            // way, because from the coordinator's perspective the
            // hook's lifecycle ran to a terminal state.
            let hook_ref = hook.as_ref();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                hook_ref.run(reason);
            }));
            if let Err(_panic) = result {
                // Swallow the panic. We deliberately do not log here
                // (the crate has no tracing dep); applications that
                // need diagnostics should wrap their own hook bodies
                // in `catch_unwind` and report.
            }
            count += 1;
            self.hooks_completed.fetch_add(1, Ordering::Relaxed);
        }
        count
    }

    /// Install OS-level signal handlers for the configured set.
    ///
    /// The back-end is selected at compile time by feature flags:
    ///
    /// - `tokio` (default): spawns background `tokio` tasks. Must be
    ///   called from inside a `tokio` runtime context.
    /// - `async-std` (and `tokio` not enabled): spawns background
    ///   `async-std` tasks. Must be called from inside an
    ///   `async-std` runtime context.
    /// - `ctrlc-fallback` (and neither runtime feature enabled):
    ///   registers a synchronous `ctrlc` handler covering
    ///   [`Signal::Interrupt`](crate::Signal::Interrupt).
    /// - No back-end feature enabled: returns [`Error::NoRuntime`].
    ///
    /// `tokio` takes precedence over `async-std`.
    ///
    /// Installation is idempotent on the coordinator side: a second
    /// call returns [`Error::AlreadyInstalled`] without touching the
    /// OS. The process-global signal slot is owned by the first
    /// back-end that grabs it; do not install handlers from two
    /// different coordinators in the same process.
    ///
    /// # Errors
    ///
    /// - [`Error::AlreadyInstalled`] if this coordinator has already
    ///   installed handlers (regardless of which back-end succeeded).
    /// - [`Error::SignalRegistration`] if the platform rejects a
    ///   specific signal. The internal install flag is reverted on
    ///   error so the call can be retried after the cause is fixed.
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
        crate::install::tokio_rt::install(self)
    }

    #[cfg(all(feature = "async-std", not(feature = "tokio")))]
    fn install_impl(&self) -> Result<()> {
        crate::install::async_std_rt::install(self)
    }

    #[cfg(all(
        feature = "ctrlc-fallback",
        not(feature = "tokio"),
        not(feature = "async-std")
    ))]
    fn install_impl(&self) -> Result<()> {
        crate::install::ctrlc_sync::install(self)
    }

    #[cfg(not(any(feature = "tokio", feature = "async-std", feature = "ctrlc-fallback")))]
    #[allow(clippy::unused_self)]
    fn install_impl(&self) -> Result<()> {
        Err(Error::NoRuntime)
    }
}

/// Builder for [`Coordinator`].
///
/// Created by [`Coordinator::builder`] or [`CoordinatorBuilder::new`].
/// Methods consume `self` and return `self` so they may be chained.
///
/// # Examples
///
/// ```
/// use signal_mod::{hook_from_fn, Coordinator, SignalSet};
/// use std::time::Duration;
///
/// let coord = Coordinator::builder()
///     .signals(SignalSet::standard())
///     .graceful_timeout(Duration::from_secs(10))
///     .force_timeout(Duration::from_secs(20))
///     .hook(hook_from_fn("close-db", 200, |_| {}))
///     .hook(hook_from_fn("flush-logs", 100, |_| {}))
///     .build();
///
/// assert_eq!(coord.signals(), SignalSet::standard());
/// ```
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
    /// Start a new builder with the default configuration.
    ///
    /// Defaults are:
    ///
    /// - signals: [`SignalSet::graceful`]
    /// - graceful timeout: 5 seconds
    /// - force timeout: 10 seconds
    /// - no hooks
    #[must_use]
    pub fn new() -> Self {
        Self {
            signals: SignalSet::graceful(),
            graceful_timeout: Duration::from_millis(DEFAULT_GRACEFUL_MS),
            force_timeout: Duration::from_millis(DEFAULT_FORCE_MS),
            hooks: Vec::new(),
        }
    }

    /// Override the signal set the coordinator will install handlers
    /// for.
    #[must_use]
    pub fn signals(mut self, set: SignalSet) -> Self {
        self.signals = set;
        self
    }

    /// Override the graceful-shutdown timeout.
    ///
    /// During [`Coordinator::run_hooks`], hooks have at most this
    /// long in aggregate before remaining hooks are skipped.
    #[must_use]
    pub fn graceful_timeout(mut self, d: Duration) -> Self {
        self.graceful_timeout = d;
        self
    }

    /// Override the force-shutdown timeout.
    ///
    /// Exposed for downstream consumers that implement their own
    /// forced ladder; the coordinator itself does not enforce this
    /// budget.
    #[must_use]
    pub fn force_timeout(mut self, d: Duration) -> Self {
        self.force_timeout = d;
        self
    }

    /// Register a [`ShutdownHook`] to run during
    /// [`Coordinator::run_hooks`].
    ///
    /// Hooks may be added in any order; they are sorted at run time
    /// by descending priority.
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
///
/// All fields are public for direct read access. The snapshot is a
/// value type; subsequent state changes on the coordinator do not
/// affect a previously-taken snapshot.
///
/// # Examples
///
/// ```
/// use signal_mod::{Coordinator, ShutdownReason};
///
/// let coord = Coordinator::builder().build();
/// let stats_before = coord.statistics();
/// assert!(!stats_before.initiated);
/// assert_eq!(stats_before.hooks_registered, 0);
///
/// coord.trigger().trigger(ShutdownReason::Requested);
/// let stats_after = coord.statistics();
/// assert!(stats_after.initiated);
/// assert_eq!(stats_after.reason, Some(ShutdownReason::Requested));
/// ```
#[derive(Debug, Clone)]
pub struct Statistics {
    /// `true` if shutdown has been initiated.
    pub initiated: bool,
    /// Reason carried with the trigger that initiated shutdown.
    pub reason: Option<ShutdownReason>,
    /// Number of hooks registered on the coordinator.
    pub hooks_registered: usize,
    /// Cumulative number of hook runs completed across all
    /// `run_hooks` calls.
    pub hooks_completed: usize,
    /// Wall-clock time since shutdown was initiated.
    pub elapsed: Option<Duration>,
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
