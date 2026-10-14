//! Outcome of running the shutdown hooks.

use core::fmt;
use std::sync::Arc;
use std::time::Duration;

use crate::reason::ShutdownReason;

/// Where a hook is in its lifecycle.
///
/// # Examples
///
/// ```
/// use signal_mod::HookStatus;
///
/// assert!(HookStatus::Completed.is_finished());
/// assert!(!HookStatus::Running.is_finished());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HookStatus {
    /// Not started yet.
    Pending,
    /// Started and still inside its run budget.
    Running,
    /// Returned normally.
    Completed,
    /// Panicked; the message is in [`HookOutcome::panic_message`].
    Panicked,
    /// Exceeded its own [`timeout`](crate::ShutdownHook::timeout) or the
    /// coordinator's budget. It was abandoned and may still be running
    /// on its thread.
    TimedOut,
    /// Never started: the force budget ran out before its turn.
    Skipped,
}

impl HookStatus {
    /// `true` for the terminal states (`Completed`, `Panicked`,
    /// `TimedOut`, `Skipped`).
    #[must_use]
    pub const fn is_finished(self) -> bool {
        !matches!(self, Self::Pending | Self::Running)
    }

    pub(crate) const fn to_u8(self) -> u8 {
        match self {
            Self::Pending => 0,
            Self::Running => 1,
            Self::Completed => 2,
            Self::Panicked => 3,
            Self::TimedOut => 4,
            Self::Skipped => 5,
        }
    }

    pub(crate) const fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::Running,
            2 => Self::Completed,
            3 => Self::Panicked,
            4 => Self::TimedOut,
            5 => Self::Skipped,
            _ => Self::Pending,
        }
    }
}

impl fmt::Display for HookStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Panicked => "panicked",
            Self::TimedOut => "timed out",
            Self::Skipped => "skipped",
        })
    }
}

/// Which budget a hook started under.
///
/// # Examples
///
/// ```
/// use signal_mod::HookPhase;
///
/// assert_ne!(HookPhase::Graceful, HookPhase::Force);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HookPhase {
    /// Started within the graceful budget, with the caller's reason.
    Graceful,
    /// Started after the graceful budget elapsed, with
    /// [`ShutdownReason::Forced`].
    Force,
}

/// What happened to one hook.
///
/// Part of a [`RunReport`] and of
/// [`Statistics::hooks`](crate::Statistics::hooks).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct HookOutcome {
    /// [`ShutdownHook::name`](crate::ShutdownHook::name).
    pub name: Arc<str>,
    /// [`ShutdownHook::priority`](crate::ShutdownHook::priority).
    pub priority: i32,
    /// Lifecycle state.
    pub status: HookStatus,
    /// Phase the hook started in; `None` if it never started.
    pub phase: Option<HookPhase>,
    /// Run time: until return or panic for finished hooks, until the
    /// deadline for timed-out ones. `None` if it never started or is
    /// still running.
    pub duration: Option<Duration>,
    /// Panic payload rendered as text, for `Panicked` hooks whose
    /// payload was a string.
    pub panic_message: Option<String>,
}

/// Result of running the shutdown hooks, returned by
/// [`Coordinator::run_hooks_with_report`](crate::Coordinator::run_hooks_with_report)
/// and [`Coordinator::run_hooks_async`](crate::Coordinator::run_hooks_async).
///
/// Every call on the same coordinator returns the report of the one
/// hook run that actually happened.
///
/// # Examples
///
/// ```
/// use signal_mod::{hook_from_fn, Coordinator, HookStatus, ShutdownReason};
///
/// let coord = Coordinator::builder()
///     .hook(hook_from_fn("ok", 10, |_| {}))
///     .hook(hook_from_fn("boom", 0, |_| panic!("disk on fire")))
///     .build();
///
/// let report = coord.run_hooks_with_report(ShutdownReason::Requested);
/// assert_eq!(report.completed(), 1);
/// assert_eq!(report.panicked(), 1);
/// assert!(!report.is_clean());
/// let boom = &report.outcomes()[1];
/// assert_eq!(boom.status, HookStatus::Panicked);
/// assert_eq!(boom.panic_message.as_deref(), Some("disk on fire"));
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RunReport {
    reason: ShutdownReason,
    outcomes: Vec<HookOutcome>,
    elapsed: Duration,
}

impl RunReport {
    pub(crate) fn new(
        reason: ShutdownReason,
        outcomes: Vec<HookOutcome>,
        elapsed: Duration,
    ) -> Self {
        Self {
            reason,
            outcomes,
            elapsed,
        }
    }

    /// Reason passed to the hook run (hooks in the force phase received
    /// [`ShutdownReason::Forced`] instead).
    #[must_use]
    pub const fn reason(&self) -> ShutdownReason {
        self.reason
    }

    /// One entry per hook, in execution (priority) order.
    #[must_use]
    pub fn outcomes(&self) -> &[HookOutcome] {
        &self.outcomes
    }

    /// Wall-clock time the run took.
    #[must_use]
    pub const fn elapsed(&self) -> Duration {
        self.elapsed
    }

    fn count(&self, status: HookStatus) -> usize {
        self.outcomes.iter().filter(|o| o.status == status).count()
    }

    /// Hooks that returned normally.
    #[must_use]
    pub fn completed(&self) -> usize {
        self.count(HookStatus::Completed)
    }

    /// Hooks that panicked.
    #[must_use]
    pub fn panicked(&self) -> usize {
        self.count(HookStatus::Panicked)
    }

    /// Hooks that overran their budget and were abandoned.
    #[must_use]
    pub fn timed_out(&self) -> usize {
        self.count(HookStatus::TimedOut)
    }

    /// Hooks that never started.
    #[must_use]
    pub fn skipped(&self) -> usize {
        self.count(HookStatus::Skipped)
    }

    /// Hooks that started in the force phase.
    #[must_use]
    pub fn forced(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| o.phase == Some(HookPhase::Force))
            .count()
    }

    /// `true` if every hook completed (vacuously true with no hooks).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.outcomes
            .iter()
            .all(|o| o.status == HookStatus::Completed)
    }
}

impl fmt::Display for RunReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} hook(s) in {:?}: {} completed, {} panicked, {} timed out, {} skipped",
            self.outcomes.len(),
            self.elapsed,
            self.completed(),
            self.panicked(),
            self.timed_out(),
            self.skipped()
        )
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn outcome(status: HookStatus, phase: Option<HookPhase>) -> HookOutcome {
        HookOutcome {
            name: Arc::from("h"),
            priority: 0,
            status,
            phase,
            duration: None,
            panic_message: None,
        }
    }

    #[test]
    fn test_status_round_trips_through_u8() {
        for s in [
            HookStatus::Pending,
            HookStatus::Running,
            HookStatus::Completed,
            HookStatus::Panicked,
            HookStatus::TimedOut,
            HookStatus::Skipped,
        ] {
            assert_eq!(HookStatus::from_u8(s.to_u8()), s);
            assert_ne!(s.to_string(), "");
        }
        assert_eq!(HookStatus::from_u8(200), HookStatus::Pending);
    }

    #[test]
    fn test_report_counts_each_status() {
        let report = RunReport::new(
            ShutdownReason::Requested,
            vec![
                outcome(HookStatus::Completed, Some(HookPhase::Graceful)),
                outcome(HookStatus::Panicked, Some(HookPhase::Graceful)),
                outcome(HookStatus::TimedOut, Some(HookPhase::Graceful)),
                outcome(HookStatus::Completed, Some(HookPhase::Force)),
                outcome(HookStatus::Skipped, None),
            ],
            Duration::from_millis(3),
        );
        assert_eq!(report.completed(), 2);
        assert_eq!(report.panicked(), 1);
        assert_eq!(report.timed_out(), 1);
        assert_eq!(report.skipped(), 1);
        assert_eq!(report.forced(), 1);
        assert!(!report.is_clean());
        assert_eq!(report.reason(), ShutdownReason::Requested);
        assert_eq!(report.elapsed(), Duration::from_millis(3));
        let text = report.to_string();
        assert!(text.starts_with("5 hook(s)"), "{text}");
        assert!(text.contains("2 completed"));
    }

    #[test]
    fn test_empty_report_is_clean() {
        let report = RunReport::new(ShutdownReason::Requested, Vec::new(), Duration::ZERO);
        assert!(report.is_clean());
        assert_eq!(report.outcomes().len(), 0);
    }
}
