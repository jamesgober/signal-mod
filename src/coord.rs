//! The [`Coordinator`], its [`builder`](CoordinatorBuilder), and the
//! [`Statistics`] snapshot type.

use core::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::dispatch::{Dispatcher, SignalSubscription};
use crate::error::{Error, Result};
use crate::hook::ShutdownHook;
use crate::install::{self, Backend, Registration};
use crate::reason::ShutdownReason;
use crate::report::{HookOutcome, HookStatus, RunReport};
use crate::runner::{HookRunner, RunHooksFuture, RuntimeContext};
use crate::signal::{Signal, SignalSet};
use crate::state::Inner;
use crate::sync;
use crate::token::{ShutdownToken, ShutdownTrigger};

/// Default graceful-shutdown budget: 5 seconds.
///
/// Long enough for typical flush-and-close work, short enough that an
/// operator's terminate does not appear to hang. Override with
/// [`CoordinatorBuilder::graceful_timeout`].
const DEFAULT_GRACEFUL: Duration = Duration::from_secs(5);

/// Default force budget: 10 seconds, measured from the start of the
/// hook run (so the force phase gets the 5 seconds after the graceful
/// phase). Override with [`CoordinatorBuilder::force_timeout`].
const DEFAULT_FORCE: Duration = Duration::from_secs(10);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Owns the shutdown state machine, the hooks, and (once installed) the
/// signal listeners.
///
/// A `Coordinator` is built once at program start via
/// [`Coordinator::builder`], then:
///
/// 1. [`install`](Self::install)s OS signal listeners for the shutdown
///    set and the notify set;
/// 2. hands out cheap [`ShutdownToken`] observers and [`ShutdownTrigger`]
///    initiators, and [`SignalSubscription`]s for non-shutdown signals;
/// 3. after shutdown is initiated, runs the registered
///    [`ShutdownHook`]s once, in descending priority order, under an
///    enforced graceful-then-force deadline
///    ([`run_hooks`](Self::run_hooks),
///    [`run_hooks_async`](Self::run_hooks_async),
///    [`run_hooks_with_report`](Self::run_hooks_with_report)).
///
/// The coordinator is also the installation's guard: dropping it (or
/// calling [`uninstall`](Self::uninstall)) removes its listeners and
/// lets the OS default action (terminate) apply again to signals no
/// other coordinator handles. Keep it alive for as long as the
/// handlers should be: typically for all of `main`.
///
/// `Coordinator` is `Send + Sync`; share it by reference or `Arc`.
///
/// # Examples
///
/// ```no_run
/// use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, SignalSet};
/// use std::time::Duration;
///
/// #[tokio::main]
/// async fn main() -> signal_mod::Result<()> {
///     let coord = Coordinator::builder()
///         .signals(SignalSet::graceful())
///         .graceful_timeout(Duration::from_secs(5))
///         .hook(hook_from_fn("flush-logs", 100, |reason| {
///             eprintln!("shutting down: {reason}");
///         }))
///         .build();
///
///     coord.install()?;
///
///     let token = coord.token();
///     token.wait().await;
///
///     let reason = token.reason().unwrap_or(ShutdownReason::Requested);
///     let report = coord.run_hooks_async(reason).await;
///     eprintln!("{report}");
///     std::process::exit(reason.exit_code());
/// }
/// ```
pub struct Coordinator {
    inner: sync::Arc<Inner>,
    runner: Arc<HookRunner>,
    dispatcher: Arc<Dispatcher>,
    signals: SignalSet,
    notify: SignalSet,
    graceful_timeout: Duration,
    force_timeout: Duration,
    escalate: bool,
    installation: Mutex<Option<Registration>>,
}

impl fmt::Debug for Coordinator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Coordinator")
            .field("signals", &self.signals)
            .field("notify_signals", &self.notify)
            .field("graceful_timeout", &self.graceful_timeout)
            .field("force_timeout", &self.force_timeout)
            .field("force_exit_on_second_signal", &self.escalate)
            .field("hooks", &self.runner.slots().len())
            .field("backend", &self.backend())
            .field("initiated", &self.inner.is_initiated())
            .finish_non_exhaustive()
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

    /// A new [`ShutdownToken`] observer handle (one `Arc::clone`).
    #[must_use]
    pub fn token(&self) -> ShutdownToken {
        ShutdownToken::new(sync::Arc::clone(&self.inner))
    }

    /// A new [`ShutdownTrigger`] initiator handle (one `Arc::clone`).
    #[must_use]
    pub fn trigger(&self) -> ShutdownTrigger {
        ShutdownTrigger::new(sync::Arc::clone(&self.inner))
    }

    /// Signals that initiate shutdown.
    #[must_use]
    pub fn signals(&self) -> SignalSet {
        self.signals
    }

    /// Signals delivered to [`subscribe`](Self::subscribe)rs without
    /// initiating shutdown.
    #[must_use]
    pub fn notify_signals(&self) -> SignalSet {
        self.notify
    }

    /// Configured graceful budget (see
    /// [`CoordinatorBuilder::graceful_timeout`]).
    #[must_use]
    pub fn graceful_timeout(&self) -> Duration {
        self.graceful_timeout
    }

    /// Configured force budget (see [`CoordinatorBuilder::force_timeout`]).
    #[must_use]
    pub fn force_timeout(&self) -> Duration {
        self.force_timeout
    }

    /// Whether a second shutdown signal exits the process (see
    /// [`CoordinatorBuilder::force_exit_on_second_signal`]).
    #[must_use]
    pub fn force_exit_on_second_signal(&self) -> bool {
        self.escalate
    }

    /// `true` while signal listeners are installed.
    #[must_use]
    pub fn is_installed(&self) -> bool {
        lock(&self.installation).is_some()
    }

    /// The back-end the current installation uses; `None` when not
    /// installed.
    #[must_use]
    pub fn backend(&self) -> Option<Backend> {
        lock(&self.installation).as_ref().map(Registration::backend)
    }

    /// Signals the current installation actually listens for: the
    /// shutdown and notify sets minus what the platform cannot deliver
    /// (`Pipe`, `User1`, `User2` on Windows). Empty when not installed.
    #[must_use]
    pub fn installed_signals(&self) -> SignalSet {
        lock(&self.installation)
            .as_ref()
            .map_or(SignalSet::empty(), Registration::covered)
    }

    /// Snapshot of the shutdown state and per-hook progress.
    ///
    /// Never blocks on a running hook: progress is read from atomics, so
    /// it is safe to call from a hook or a health endpoint during
    /// shutdown. `initiated`, `reason` and `elapsed` come from one atomic
    /// publication, so `initiated == reason.is_some()` always holds; the
    /// per-hook entries are read one by one and may straddle a hook
    /// finishing.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{hook_from_fn, Coordinator, ShutdownReason};
    ///
    /// let coord = Coordinator::builder().hook(hook_from_fn("h", 0, |_| {})).build();
    /// assert_eq!(coord.statistics().hooks_completed, 0);
    /// coord.trigger().trigger(ShutdownReason::Requested);
    /// coord.run_hooks(ShutdownReason::Requested);
    /// let stats = coord.statistics();
    /// assert!(stats.initiated);
    /// assert_eq!(stats.hooks_completed, 1);
    /// assert!(stats.hooks[0].duration.is_some());
    /// ```
    #[must_use]
    pub fn statistics(&self) -> Statistics {
        let hooks: Vec<HookOutcome> = self
            .runner
            .slots()
            .iter()
            .map(crate::runner::HookSlot::outcome)
            .collect();
        let count = |status| hooks.iter().filter(|h| h.status == status).count();
        Statistics {
            initiated: self.inner.is_initiated(),
            reason: self.inner.reason(),
            elapsed: self.inner.elapsed(),
            hooks_registered: hooks.len(),
            hooks_completed: count(HookStatus::Completed),
            hooks_panicked: count(HookStatus::Panicked),
            hooks_timed_out: count(HookStatus::TimedOut),
            hooks_skipped: count(HookStatus::Skipped),
            installed: self.is_installed(),
            active_subsystems: self.inner.active_subsystems(),
            hooks,
        }
    }

    /// Run the hooks and return how many completed.
    ///
    /// Shorthand for
    /// [`run_hooks_with_report(reason).completed()`](Self::run_hooks_with_report);
    /// see that method for the full contract. This blocks the calling
    /// thread for up to the force budget, so call it from synchronous
    /// code; from async code use [`run_hooks_async`](Self::run_hooks_async).
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
    /// assert_eq!(coord.run_hooks(ShutdownReason::Requested), 2);
    /// // Hooks run at most once: later calls report the same run.
    /// assert_eq!(coord.run_hooks(ShutdownReason::Forced), 2);
    /// ```
    pub fn run_hooks(&self, reason: ShutdownReason) -> usize {
        self.run_hooks_with_report(reason).completed()
    }

    /// Run the hooks under the enforced graceful-then-force deadline and
    /// return the full [`RunReport`].
    ///
    /// - Hooks run in descending priority order (registration order
    ///   within a priority), each on a worker thread. With the `tokio`
    ///   feature, a Tokio runtime current on the calling thread is
    ///   entered on the worker thread.
    /// - **Graceful phase:** hooks start with `reason` until
    ///   [`graceful_timeout`](CoordinatorBuilder::graceful_timeout) has
    ///   elapsed since the run began.
    /// - **Force phase:** hooks not started by then start with
    ///   [`ShutdownReason::Forced`] until
    ///   [`force_timeout`](CoordinatorBuilder::force_timeout) has
    ///   elapsed since the run began. Later hooks are `Skipped`.
    /// - A hook still running when its own
    ///   [`timeout`](ShutdownHook::timeout) or its phase's deadline
    ///   passes is reported `TimedOut` and abandoned (its thread keeps
    ///   running it); the run moves on. The call therefore returns
    ///   within the force budget plus scheduling noise, whatever the
    ///   hooks do.
    /// - A panicking hook is reported `Panicked` with its message and
    ///   does not stop the run.
    /// - **Run-once:** the first call runs the hooks. Every other call,
    ///   concurrent or later, sync or async, waits for that run and
    ///   returns its report; `reason` is then ignored. No hook body runs
    ///   twice.
    ///
    /// Blocks the calling thread; from async code use
    /// [`run_hooks_async`](Self::run_hooks_async). Do not call it from
    /// inside a hook: the call would wait for the run that is waiting
    /// for the hook, until the hook's deadline.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{hook_from_fn, Coordinator, HookStatus, ShutdownReason};
    /// use std::time::Duration;
    ///
    /// let coord = Coordinator::builder()
    ///     .graceful_timeout(Duration::from_millis(50))
    ///     .force_timeout(Duration::from_millis(500))
    ///     .hook(hook_from_fn("hangs", 10, |_| std::thread::sleep(Duration::from_secs(2))))
    ///     .hook(hook_from_fn("flush", 0, |_| {}))
    ///     .build();
    ///
    /// let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    /// assert!(report.elapsed() < Duration::from_secs(2));
    /// assert_eq!(report.outcomes()[0].status, HookStatus::TimedOut);
    /// // `flush` still ran, in the force phase.
    /// assert_eq!(report.outcomes()[1].status, HookStatus::Completed);
    /// assert_eq!(report.forced(), 1);
    /// ```
    pub fn run_hooks_with_report(&self, reason: ShutdownReason) -> RunReport {
        self.runner.run_blocking(reason, &RuntimeContext::capture())
    }

    /// Run the hooks without blocking the async executor.
    ///
    /// Same contract as [`run_hooks_with_report`](Self::run_hooks_with_report),
    /// but the hooks are driven from a dedicated supervisor thread and
    /// the returned future resolves with the report. It works on any
    /// executor. With the `tokio` feature, hooks can bridge to async
    /// cleanup with `tokio::runtime::Handle::current().block_on(..)`: the
    /// runtime stays free to drive that work (1.x documented this
    /// bridge, but `run_hooks` called from `#[tokio::main]` made it
    /// panic).
    ///
    /// The future is `'static` and `Send`. Dropping it does not cancel a
    /// run that has started.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main]
    /// # async fn main() {
    /// use signal_mod::{hook_from_fn, Coordinator, ShutdownReason};
    ///
    /// let coord = Coordinator::builder()
    ///     .hook(hook_from_fn("flush-db", 0, |_| {
    ///         // Synchronous cleanup here; with the `tokio` feature, async
    ///         // cleanup can go through `Handle::current().block_on(..)`.
    ///     }))
    ///     .build();
    ///
    /// let report = coord.run_hooks_async(ShutdownReason::Requested).await;
    /// assert!(report.is_clean());
    /// # }
    /// ```
    pub fn run_hooks_async(&self, reason: ShutdownReason) -> RunHooksFuture {
        self.runner.run_async(reason)
    }

    /// Install OS signal listeners for the shutdown set and the notify
    /// set.
    ///
    /// The back-end is chosen now, from the enabled features and the
    /// calling context:
    ///
    /// 1. `tokio` enabled and a Tokio runtime current: Tokio listener
    ///    tasks ([`Backend::Tokio`]).
    /// 2. otherwise, `agnostic` enabled: a listener thread
    ///    ([`Backend::Agnostic`]).
    /// 3. otherwise: [`Error::OutsideTokioRuntime`] (only `tokio`
    ///    enabled) or [`Error::NoRuntime`] (no back-end enabled).
    ///
    /// Installation is all-or-nothing and never panics. Several
    /// coordinators may be installed in one process; every one of them
    /// receives the signals it listens for.
    ///
    /// Listeners stay installed until [`uninstall`](Self::uninstall) or
    /// until the coordinator is dropped. After that, a signal that no
    /// other coordinator handles gets the OS default action again
    /// (SIGPIPE stays ignored, as Rust programs expect). A signal-mod
    /// limitation worth knowing on Unix: if your program also listens
    /// for one of these signals through `tokio::signal` directly, that
    /// listener no longer sees the signal once every coordinator that
    /// covered it has been uninstalled.
    ///
    /// # Errors
    ///
    /// - [`Error::AlreadyInstalled`]: this coordinator is installed.
    /// - [`Error::SignalRegistration`]: the platform rejected a signal,
    ///   or the Tokio runtime has no I/O driver.
    /// - [`Error::ListenerThread`]: the agnostic back-end could not
    ///   start its thread.
    /// - [`Error::OutsideTokioRuntime`] / [`Error::NoRuntime`]: see
    ///   above.
    ///
    /// # Examples
    ///
    /// ```
    /// # #[tokio::main(flavor = "current_thread")]
    /// # async fn main() -> signal_mod::Result<()> {
    /// use signal_mod::{Coordinator, Error, Signal, SignalSet};
    ///
    /// let coord = Coordinator::builder()
    ///     .signals(SignalSet::from(Signal::User2))
    ///     .build();
    /// match coord.install() {
    ///     Ok(()) => {
    ///         assert!(coord.is_installed());
    ///         assert!(matches!(coord.install(), Err(Error::AlreadyInstalled)));
    ///         assert!(coord.uninstall());
    ///     }
    ///     // Built without any back-end feature.
    ///     Err(Error::NoRuntime) => assert!(!coord.is_installed()),
    ///     Err(other) => return Err(other),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn install(&self) -> Result<()> {
        let mut installation = lock(&self.installation);
        if installation.is_some() {
            return Err(Error::AlreadyInstalled);
        }
        let registration = install::install(&self.dispatcher, self.signals | self.notify)?;
        *installation = Some(registration);
        Ok(())
    }

    /// Remove this coordinator's signal listeners. Returns `false` if it
    /// was not installed.
    ///
    /// Signals no other coordinator still handles get their OS default
    /// action back (see [`install`](Self::install)). The coordinator can
    /// be installed again afterwards. Dropping the coordinator does the
    /// same.
    pub fn uninstall(&self) -> bool {
        let registration = lock(&self.installation).take();
        match registration {
            Some(registration) => {
                registration.uninstall();
                true
            }
            None => false,
        }
    }

    /// Receive delivered signals (shutdown and notify sets alike). See
    /// [`SignalSubscription`].
    #[must_use]
    pub fn subscribe(&self) -> SignalSubscription {
        self.dispatcher.subscribe()
    }

    /// Deliver `sig` exactly as an installed back-end would, for tests
    /// and for forwarding signals that some other component received.
    ///
    /// Subscribers see it; if it is in the shutdown set it initiates
    /// shutdown; with
    /// [`force_exit_on_second_signal`](CoordinatorBuilder::force_exit_on_second_signal)
    /// a repeat **exits the process**, as a real second signal would.
    /// Returns `false` (and does nothing) for a signal in neither the
    /// shutdown nor the notify set, which no back-end would deliver.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{Coordinator, ShutdownReason, Signal};
    ///
    /// let coord = Coordinator::builder().build();
    /// assert!(coord.simulate_signal(Signal::Terminate));
    /// assert_eq!(
    ///     coord.token().reason(),
    ///     Some(ShutdownReason::Signal(Signal::Terminate))
    /// );
    /// assert!(!coord.simulate_signal(Signal::User1)); // not configured
    /// ```
    pub fn simulate_signal(&self, sig: Signal) -> bool {
        if !(self.signals | self.notify).contains(sig) {
            return false;
        }
        self.dispatcher.deliver(sig);
        true
    }
}

impl Drop for Coordinator {
    fn drop(&mut self) {
        let _was_installed = self.uninstall();
    }
}

/// Builder for [`Coordinator`].
///
/// # Examples
///
/// ```
/// use signal_mod::{hook_from_fn, Coordinator, Signal, SignalSet};
/// use std::time::Duration;
///
/// let coord = Coordinator::builder()
///     .signals(SignalSet::standard() - Signal::Hangup)
///     .notify_signals(SignalSet::from(Signal::Hangup))
///     .graceful_timeout(Duration::from_secs(10))
///     .force_timeout(Duration::from_secs(20))
///     .force_exit_on_second_signal(true)
///     .hook(hook_from_fn("close-db", 200, |_| {}))
///     .hook(hook_from_fn("flush-logs", 100, |_| {}))
///     .build();
///
/// assert!(!coord.signals().contains(Signal::Hangup));
/// ```
pub struct CoordinatorBuilder {
    signals: SignalSet,
    notify: SignalSet,
    graceful_timeout: Duration,
    force_timeout: Duration,
    escalate: bool,
    hooks: Vec<Box<dyn ShutdownHook>>,
}

impl fmt::Debug for CoordinatorBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoordinatorBuilder")
            .field("signals", &self.signals)
            .field("notify_signals", &self.notify)
            .field("graceful_timeout", &self.graceful_timeout)
            .field("force_timeout", &self.force_timeout)
            .field("force_exit_on_second_signal", &self.escalate)
            .field("hooks", &self.hooks.len())
            .finish()
    }
}

impl CoordinatorBuilder {
    /// Start a builder with the defaults:
    ///
    /// - shutdown signals: [`SignalSet::graceful`]
    /// - notify signals: none
    /// - graceful budget: 5 seconds; force budget: 10 seconds
    /// - no second-signal escalation
    /// - no hooks
    #[must_use]
    pub fn new() -> Self {
        Self {
            signals: SignalSet::graceful(),
            notify: SignalSet::empty(),
            graceful_timeout: DEFAULT_GRACEFUL,
            force_timeout: DEFAULT_FORCE,
            escalate: false,
            hooks: Vec::new(),
        }
    }

    /// Signals that initiate shutdown.
    #[must_use]
    pub fn signals(mut self, set: SignalSet) -> Self {
        self.signals = set;
        self
    }

    /// Signals to deliver to [`Coordinator::subscribe`]rs **without**
    /// initiating shutdown: SIGHUP for "reload configuration", SIGUSR1
    /// for "reopen log files", and so on. A signal in both sets
    /// initiates shutdown and is delivered to subscribers.
    #[must_use]
    pub fn notify_signals(mut self, set: SignalSet) -> Self {
        self.notify = set;
        self
    }

    /// Graceful budget for running hooks, measured from the start of the
    /// run. Hooks that start within it receive the caller's reason; it
    /// is enforced (a hook still running at the deadline is abandoned).
    ///
    /// On Windows, the OS kills a console process about 5 seconds after
    /// `CTRL_CLOSE_EVENT`; if console close matters, keep the budget
    /// (and the force budget) below that. `Duration::MAX` disables the
    /// deadline.
    #[must_use]
    pub fn graceful_timeout(mut self, d: Duration) -> Self {
        self.graceful_timeout = d;
        self
    }

    /// Total budget for running hooks, measured from the start of the
    /// run. Hooks not started when the graceful budget runs out start in
    /// the force phase, with [`ShutdownReason::Forced`], until this
    /// budget runs out; the rest are skipped. A value not above
    /// [`graceful_timeout`](Self::graceful_timeout) disables the force
    /// phase.
    #[must_use]
    pub fn force_timeout(mut self, d: Duration) -> Self {
        self.force_timeout = d;
        self
    }

    /// Exit the process immediately when a shutdown signal arrives after
    /// shutdown was already initiated: the "press Ctrl+C again to quit"
    /// convention. The exit status is the second signal's
    /// [`exit_code`](Signal::exit_code) (130 for SIGINT, 143 for
    /// SIGTERM). The exit is `std::process::exit`: destructors and
    /// remaining hooks do not run. Off by default.
    #[must_use]
    pub fn force_exit_on_second_signal(mut self, enabled: bool) -> Self {
        self.escalate = enabled;
        self
    }

    /// Register a [`ShutdownHook`]. Hooks may be added in any order;
    /// they run by descending priority, registration order breaking
    /// ties.
    #[must_use]
    pub fn hook<H: ShutdownHook>(mut self, h: H) -> Self {
        self.hooks.push(Box::new(h));
        self
    }

    /// Finalize into a [`Coordinator`]. Sorts the hooks once.
    #[must_use]
    pub fn build(self) -> Coordinator {
        let inner = Inner::new();
        let trigger = ShutdownTrigger::new(sync::Arc::clone(&inner));
        Coordinator {
            dispatcher: Arc::new(Dispatcher::new(trigger, self.signals, self.escalate)),
            runner: Arc::new(HookRunner::new(
                self.hooks,
                self.graceful_timeout,
                self.force_timeout,
            )),
            inner,
            signals: self.signals,
            notify: self.notify,
            graceful_timeout: self.graceful_timeout,
            force_timeout: self.force_timeout,
            escalate: self.escalate,
            installation: Mutex::new(None),
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
/// A value type: later state changes do not affect a snapshot already
/// taken. Fields are public for reading; the struct is
/// `#[non_exhaustive]` so fields can be added without a breaking change.
///
/// # Examples
///
/// ```
/// use signal_mod::{Coordinator, ShutdownReason};
///
/// let coord = Coordinator::builder().build();
/// let before = coord.statistics();
/// assert!(!before.initiated);
/// assert_eq!(before.hooks_registered, 0);
///
/// coord.trigger().trigger(ShutdownReason::Requested);
/// let after = coord.statistics();
/// assert!(after.initiated);
/// assert_eq!(after.reason, Some(ShutdownReason::Requested));
/// assert_ne!(before, after);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Statistics {
    /// `true` once shutdown has been initiated.
    pub initiated: bool,
    /// Reason the shutdown was initiated with; `Some` exactly when
    /// `initiated`.
    pub reason: Option<ShutdownReason>,
    /// Time since shutdown was initiated.
    pub elapsed: Option<Duration>,
    /// Number of hooks registered.
    pub hooks_registered: usize,
    /// Hooks that returned normally.
    pub hooks_completed: usize,
    /// Hooks that panicked.
    pub hooks_panicked: usize,
    /// Hooks that overran their budget and were abandoned.
    pub hooks_timed_out: usize,
    /// Hooks that never started because the force budget ran out.
    pub hooks_skipped: usize,
    /// Per-hook status, phase, run time and panic message, in execution
    /// order.
    pub hooks: Vec<HookOutcome>,
    /// `true` while signal listeners are installed.
    pub installed: bool,
    /// Live [`SubsystemGuard`](crate::SubsystemGuard)s.
    pub active_subsystems: usize,
}

#[cfg(all(test, not(loom)))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions

    use super::*;
    use crate::hook::hook_from_fn;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_builder_defaults_match_documentation() {
        let c = Coordinator::builder().build();
        assert_eq!(c.signals(), SignalSet::graceful());
        assert_eq!(c.notify_signals(), SignalSet::empty());
        assert_eq!(c.graceful_timeout(), Duration::from_secs(5));
        assert_eq!(c.force_timeout(), Duration::from_secs(10));
        assert!(!c.force_exit_on_second_signal());
        assert!(!c.is_installed());
        assert_eq!(c.backend(), None);
        assert_eq!(c.installed_signals(), SignalSet::empty());
        let stats = c.statistics();
        assert!(!stats.initiated);
        assert_eq!(stats.hooks_registered, 0);
        assert_eq!(stats.hooks_completed, 0);
        assert!(!stats.installed);
        assert_eq!(
            CoordinatorBuilder::default().build().signals(),
            SignalSet::graceful()
        );
    }

    #[test]
    fn test_hooks_run_in_priority_order() {
        let order = Arc::new(Mutex::new(Vec::<i32>::new()));
        let push = |p: i32| {
            let o = Arc::clone(&order);
            hook_from_fn(format!("p{p}"), p, move |_| lock(&o).push(p))
        };
        let c = Coordinator::builder()
            .hook(push(0))
            .hook(push(100))
            .hook(push(50))
            .build();
        assert_eq!(c.run_hooks(ShutdownReason::Requested), 3);
        assert_eq!(*lock(&order), vec![100, 50, 0]);
        assert_eq!(c.statistics().hooks_completed, 3);
    }

    #[test]
    fn test_unwind_from_hook_counts_as_panicked_not_completed() {
        let c = Coordinator::builder()
            .hook(hook_from_fn("boom", 0, |_| panic!("boom")))
            .build();
        assert_eq!(c.run_hooks(ShutdownReason::Requested), 0);
        let stats = c.statistics();
        assert_eq!(stats.hooks_panicked, 1);
        assert_eq!(stats.hooks_completed, 0);
        assert_eq!(stats.hooks[0].panic_message.as_deref(), Some("boom"));
    }

    #[test]
    fn test_debug_output_does_not_block_and_names_fields() {
        let c = Coordinator::builder()
            .hook(hook_from_fn("h", 0, |_| {}))
            .build();
        let text = format!("{c:?}");
        assert!(text.contains("hooks: 1"), "{text}");
        let b = format!("{:?}", Coordinator::builder());
        assert!(b.contains("CoordinatorBuilder"), "{b}");
    }

    #[test]
    fn test_uninstall_when_not_installed_returns_false() {
        let c = Coordinator::builder().build();
        assert!(!c.uninstall());
    }

    #[test]
    fn test_simulate_signal_outside_configured_sets_is_ignored() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = Coordinator::builder()
            .signals(SignalSet::empty())
            .notify_signals(SignalSet::from(Signal::User1))
            .build();
        let mut sub = c.subscribe();
        assert!(!c.simulate_signal(Signal::Terminate));
        assert!(c.simulate_signal(Signal::User1));
        assert_eq!(sub.try_recv(), Some(Signal::User1));
        assert!(!c.token().is_initiated());
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[cfg(not(any(feature = "tokio", feature = "agnostic")))]
    #[test]
    fn test_install_without_backend_feature_returns_no_runtime() {
        let c = Coordinator::builder().build();
        assert!(matches!(c.install(), Err(Error::NoRuntime)));
        assert!(!c.is_installed());
    }

    #[cfg(all(feature = "tokio", not(feature = "agnostic")))]
    #[test]
    fn test_install_outside_tokio_runtime_returns_error_and_stays_uninstalled() {
        // SM-006 regression: 1.0.0 panicked here and left the
        // installed flag set.
        let c = Coordinator::builder().build();
        assert!(matches!(c.install(), Err(Error::OutsideTokioRuntime)));
        assert!(!c.is_installed());
        assert!(matches!(c.install(), Err(Error::OutsideTokioRuntime)));
    }
}
