//! [`ShutdownReason`] taxonomy.

use core::fmt;

use crate::signal::Signal;

/// Why a shutdown was initiated.
///
/// Carried by [`ShutdownTrigger::trigger`](crate::ShutdownTrigger::trigger)
/// and surfaced to observers via [`ShutdownToken::reason`](crate::ShutdownToken::reason)
/// and to hooks via [`ShutdownHook::run`](crate::ShutdownHook::run).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShutdownReason {
    /// Originated from a delivered OS signal.
    Signal(Signal),
    /// Programmatic shutdown request (admin endpoint, parent
    /// supervisor, fatal-error branch, etc.).
    Requested,
    /// Graceful budget elapsed; remaining hooks were skipped.
    Forced,
    /// A timed operation exceeded its deadline.
    Timeout,
    /// Shutdown driven by an internal error condition.
    Error,
}

impl ShutdownReason {
    /// Short human-readable label.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Signal(_) => "signal",
            Self::Requested => "requested",
            Self::Forced => "forced",
            Self::Timeout => "timeout",
            Self::Error => "error",
        }
    }

    /// `true` if this reason was caused by a signal.
    #[must_use]
    pub const fn is_signal(self) -> bool {
        matches!(self, Self::Signal(_))
    }
}

impl fmt::Display for ShutdownReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Signal(s) => write!(f, "Signal({s})"),
            Self::Requested => f.write_str("Requested"),
            Self::Forced => f.write_str("Forced"),
            Self::Timeout => f.write_str("Timeout"),
            Self::Error => f.write_str("Error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn description_labels() {
        assert_eq!(ShutdownReason::Requested.description(), "requested");
        assert_eq!(ShutdownReason::Forced.description(), "forced");
        assert_eq!(ShutdownReason::Timeout.description(), "timeout");
        assert_eq!(ShutdownReason::Error.description(), "error");
        assert_eq!(
            ShutdownReason::Signal(Signal::Terminate).description(),
            "signal"
        );
    }

    #[test]
    fn is_signal_only_for_signal_variant() {
        assert!(ShutdownReason::Signal(Signal::Interrupt).is_signal());
        assert!(!ShutdownReason::Requested.is_signal());
        assert!(!ShutdownReason::Forced.is_signal());
    }

    #[test]
    fn display_renders_signal_label() {
        let s = format!("{}", ShutdownReason::Signal(Signal::Terminate));
        assert!(s.starts_with("Signal("));
    }
}
