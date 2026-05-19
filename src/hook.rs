//! Shutdown hook trait and closure adapter.

use crate::reason::ShutdownReason;

/// A unit of cleanup work to run during shutdown.
///
/// Hooks are registered on the [`CoordinatorBuilder`](crate::CoordinatorBuilder)
/// and executed by [`Coordinator::run_hooks`](crate::Coordinator::run_hooks)
/// in descending priority order. Within a priority, insertion order
/// is preserved.
///
/// The `run` method is synchronous. Hooks that need to do async work
/// should hold a runtime handle and bridge with `Handle::block_on`
/// (Tokio) or `async_std::task::block_on`.
pub trait ShutdownHook: Send + Sync + 'static {
    /// Name of the hook for diagnostics.
    fn name(&self) -> &str;

    /// Higher priority hooks run first. Defaults to `0`.
    fn priority(&self) -> i32 {
        0
    }

    /// Cleanup body. Called at most once per coordinator lifetime.
    fn run(&self, reason: ShutdownReason);
}

/// Closure-backed [`ShutdownHook`].
///
/// Construct with [`hook_from_fn`].
pub struct FnHook<F>
where
    F: Fn(ShutdownReason) + Send + Sync + 'static,
{
    name: String,
    priority: i32,
    f: F,
}

impl<F> FnHook<F>
where
    F: Fn(ShutdownReason) + Send + Sync + 'static,
{
    /// Construct directly. Most callers should use [`hook_from_fn`].
    pub fn new(name: impl Into<String>, priority: i32, f: F) -> Self {
        Self {
            name: name.into(),
            priority,
            f,
        }
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

    fn run(&self, reason: ShutdownReason) {
        (self.f)(reason);
    }
}

/// Convenience: wrap a closure as a [`ShutdownHook`].
///
/// # Example
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn fn_hook_records_invocations() {
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);
        let hook = hook_from_fn("bump", 0, move |_| {
            c.fetch_add(1, Ordering::Relaxed);
        });
        hook.run(ShutdownReason::Requested);
        hook.run(ShutdownReason::Requested);
        assert_eq!(counter.load(Ordering::Relaxed), 2);
        assert_eq!(hook.name(), "bump");
        assert_eq!(hook.priority(), 0);
    }
}
