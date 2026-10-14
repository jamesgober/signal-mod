//! Shutdown hook trait and closure adapter.

use core::fmt;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use crate::reason::ShutdownReason;
use crate::runner::{HookControl, current_hook};

/// A unit of cleanup work to run during shutdown.
///
/// Hooks are registered on the [`CoordinatorBuilder`](crate::CoordinatorBuilder)
/// and executed by [`Coordinator::run_hooks`](crate::Coordinator::run_hooks)
/// (or its async and report variants) in descending priority order;
/// within a priority, registration order is preserved.
///
/// # Execution contract
///
/// - [`run`](Self::run) is called **at most once** per coordinator, no
///   matter how many times or from how many threads the hooks are run.
/// - It runs on a dedicated worker thread, not on the caller's thread,
///   so the coordinator can enforce deadlines. When the hooks are run
///   from inside a Tokio runtime (with the `tokio` feature), that
///   runtime's context is entered on the worker thread, so async
///   cleanup can bridge with
///   `tokio::runtime::Handle::current().block_on(..)`. Prefer
///   [`Coordinator::run_hooks_async`](crate::Coordinator::run_hooks_async)
///   from async code: it leaves the runtime's threads free to drive the
///   futures the hook waits on.
/// - A hook that overruns its [`timeout`](Self::timeout) or the
///   coordinator's budget is reported as timed out and abandoned; it
///   keeps running on its thread while the remaining hooks proceed.
///   Hooks must therefore not rely on a later hook not having started.
/// - A panic inside `run` is caught, reported with its message, and
///   does not stop the remaining hooks.
/// - Inside `run`, [`HookContext::current`] gives the hook's deadline and
///   tells it when it has been abandoned, so long-running cleanup can
///   stop early. An abandoned hook that holds a lock the rest of the
///   program needs keeps holding it until it returns: keep critical
///   sections in hooks short, or check
///   [`HookContext::is_abandoned`] between steps.
///
/// # Examples
///
/// ```
/// use signal_mod::{Coordinator, ShutdownHook, ShutdownReason};
/// use std::time::Duration;
///
/// struct FlushCache;
///
/// impl ShutdownHook for FlushCache {
///     fn name(&self) -> &str {
///         "flush-cache"
///     }
///     fn priority(&self) -> i32 {
///         50
///     }
///     fn timeout(&self) -> Option<Duration> {
///         Some(Duration::from_secs(2))
///     }
///     fn run(&self, _reason: ShutdownReason) {
///         // write dirty entries back ...
///     }
/// }
///
/// let coord = Coordinator::builder().hook(FlushCache).build();
/// assert_eq!(coord.run_hooks(ShutdownReason::Requested), 1);
/// ```
pub trait ShutdownHook: Send + Sync + 'static {
    /// Name of the hook, used in reports and diagnostics.
    fn name(&self) -> &str;

    /// Higher priority hooks run first. Defaults to `0`.
    fn priority(&self) -> i32 {
        0
    }

    /// Longest this hook may run before it is reported as timed out and
    /// the next hook starts. `None` (the default) bounds it only by the
    /// coordinator's remaining budget.
    fn timeout(&self) -> Option<Duration> {
        None
    }

    /// Cleanup body. Called at most once per coordinator.
    ///
    /// `reason` is the reason passed to `run_hooks`, or
    /// [`ShutdownReason::Forced`] if the hook starts after the graceful
    /// budget has elapsed (the force phase).
    fn run(&self, reason: ShutdownReason);
}

/// The deadline and abandonment state of the hook running on the
/// current thread.
///
/// Obtained with [`HookContext::current`] from inside
/// [`ShutdownHook::run`]. Hooks cannot be stopped from the outside; this
/// is how a hook cooperates: it can size its work to
/// [`remaining`](Self::remaining) and stop once
/// [`is_abandoned`](Self::is_abandoned) turns `true` (the run has
/// reported it `TimedOut` and moved on).
///
/// # Examples
///
/// ```
/// use signal_mod::{Coordinator, HookContext, ShutdownReason, hook_from_fn};
/// use std::time::Duration;
///
/// let coord = Coordinator::builder()
///     .hook(
///         hook_from_fn("flush-batches", 0, |_| {
///             let ctx = HookContext::current().expect("inside a hook");
///             for _batch in 0..3 {
///                 if ctx.is_abandoned() {
///                     return; // the run gave up on us; stop touching state
///                 }
///                 // flush one batch ...
///             }
///             assert!(ctx.remaining().is_some());
///         })
///         .with_timeout(Duration::from_secs(5)),
///     )
///     .build();
/// assert_eq!(coord.run_hooks(ShutdownReason::Requested), 1);
/// assert!(HookContext::current().is_none()); // not inside a hook
/// ```
#[derive(Clone)]
pub struct HookContext {
    control: Arc<HookControl>,
}

impl HookContext {
    /// The context of the hook running on this thread; `None` outside
    /// [`ShutdownHook::run`].
    #[must_use]
    pub fn current() -> Option<Self> {
        current_hook().map(|control| Self { control })
    }

    /// When this hook will be abandoned: the earlier of its own
    /// [`timeout`](ShutdownHook::timeout) and the end of the phase it
    /// started in. `None` if no deadline applies.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        self.control.deadline
    }

    /// Time left before [`deadline`](Self::deadline) (zero once it has
    /// passed); `None` if no deadline applies.
    #[must_use]
    pub fn remaining(&self) -> Option<Duration> {
        self.control
            .deadline
            .map(|d| d.saturating_duration_since(Instant::now()))
    }

    /// `true` once the run has given up on this hook (reported it
    /// `TimedOut` and moved on). The hook should return as soon as it
    /// can.
    #[must_use]
    pub fn is_abandoned(&self) -> bool {
        self.control.abandoned.load(Ordering::Acquire)
    }
}

impl fmt::Debug for HookContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookContext")
            .field("deadline", &self.control.deadline)
            .field("abandoned", &self.is_abandoned())
            .finish()
    }
}

/// Closure-backed [`ShutdownHook`].
///
/// Construct with [`hook_from_fn`] and optionally bound it with
/// [`with_timeout`](Self::with_timeout).
///
/// # Examples
///
/// ```
/// use signal_mod::{hook_from_fn, ShutdownHook};
/// use std::time::Duration;
///
/// let hook = hook_from_fn("close-db", 200, |_reason| {}).with_timeout(Duration::from_secs(1));
/// assert_eq!(hook.name(), "close-db");
/// assert_eq!(hook.timeout(), Some(Duration::from_secs(1)));
/// ```
pub struct FnHook<F>
where
    F: Fn(ShutdownReason) + Send + Sync + 'static,
{
    name: String,
    priority: i32,
    timeout: Option<Duration>,
    f: F,
}

impl<F> FnHook<F>
where
    F: Fn(ShutdownReason) + Send + Sync + 'static,
{
    /// Construct directly. Most callers use [`hook_from_fn`].
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{FnHook, ShutdownHook};
    ///
    /// let hook = FnHook::new("noop", 0, |_| {});
    /// assert_eq!(hook.priority(), 0);
    /// ```
    pub fn new(name: impl Into<String>, priority: i32, f: F) -> Self {
        Self {
            name: name.into(),
            priority,
            timeout: None,
            f,
        }
    }

    /// Bound this hook's run time (see [`ShutdownHook::timeout`]).
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }
}

impl<F> fmt::Debug for FnHook<F>
where
    F: Fn(ShutdownReason) + Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FnHook")
            .field("name", &self.name)
            .field("priority", &self.priority)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl<F> ShutdownHook for FnHook<F>
where
    F: Fn(ShutdownReason) + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    fn run(&self, reason: ShutdownReason) {
        (self.f)(reason);
    }
}

/// Wrap a closure as a [`ShutdownHook`].
///
/// # Examples
///
/// ```
/// use signal_mod::{hook_from_fn, ShutdownHook};
///
/// let hook = hook_from_fn("flush-logs", 100, |reason| {
///     eprintln!("shutting down: {reason}");
/// });
/// assert_eq!(hook.name(), "flush-logs");
/// assert_eq!(hook.priority(), 100);
/// ```
pub fn hook_from_fn<F>(name: impl Into<String>, priority: i32, f: F) -> FnHook<F>
where
    F: Fn(ShutdownReason) + Send + Sync + 'static,
{
    FnHook::new(name, priority, f)
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_fn_hook_run_invokes_closure_each_time_called_directly() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);
        let hook = hook_from_fn("bump", 0, move |_| {
            let _previous = c.fetch_add(1, Ordering::Relaxed);
        });
        hook.run(ShutdownReason::Requested);
        hook.run(ShutdownReason::Requested);
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        assert_eq!(hook.name(), "bump");
        assert_eq!(hook.priority(), 0);
        assert_eq!(hook.timeout(), None);
    }

    #[test]
    fn test_fn_hook_with_timeout_reports_it() {
        let hook = hook_from_fn("t", 1, |_| {}).with_timeout(Duration::from_millis(7));
        assert_eq!(hook.timeout(), Some(Duration::from_millis(7)));
        let text = format!("{hook:?}");
        assert!(text.contains("\"t\"") && text.contains("7ms"));
    }

    #[test]
    fn test_trait_default_priority_and_timeout() {
        struct Plain;
        impl ShutdownHook for Plain {
            fn name(&self) -> &'static str {
                "plain"
            }
            fn run(&self, _reason: ShutdownReason) {}
        }
        assert_eq!(Plain.priority(), 0);
        assert_eq!(Plain.timeout(), None);
        Plain.run(ShutdownReason::Requested);
        assert_eq!(Plain.name(), "plain");
    }
}
