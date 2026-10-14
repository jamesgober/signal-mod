//! [`ShutdownReason`] taxonomy.

use core::fmt;

use crate::signal::Signal;

/// Why a shutdown was initiated.
///
/// Carried by [`ShutdownTrigger::trigger`](crate::ShutdownTrigger::trigger),
/// surfaced to observers via [`ShutdownToken::reason`](crate::ShutdownToken::reason)
/// and handed to hooks via [`ShutdownHook::run`](crate::ShutdownHook::run).
///
/// The crate itself produces [`Signal`](Self::Signal) (from an installed
/// handler) and [`Forced`](Self::Forced) (hooks that run in the force
/// phase of [`Coordinator::run_hooks`](crate::Coordinator::run_hooks)).
/// `Requested`, `Timeout` and `Error` are labels for callers to pass to
/// `trigger`.
///
/// # Examples
///
/// ```
/// use signal_mod::{ShutdownReason, Signal};
///
/// let reason = ShutdownReason::Signal(Signal::Terminate);
/// assert!(reason.is_signal());
/// assert_eq!(reason.signal(), Some(Signal::Terminate));
/// assert_eq!(reason.exit_code(), 143);
/// assert_eq!(ShutdownReason::Requested.exit_code(), 0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ShutdownReason {
    /// Originated from a delivered OS signal.
    Signal(Signal),
    /// Programmatic shutdown request (admin endpoint, parent
    /// supervisor, end of a batch job, and so on).
    Requested,
    /// The graceful budget elapsed. Hooks that had not started by then
    /// run in the force phase and receive this reason instead of the
    /// original one, so they can take a faster path.
    Forced,
    /// A timed operation exceeded its deadline (caller-supplied label).
    Timeout,
    /// Shutdown driven by an internal error condition (caller-supplied
    /// label).
    Error,
}

impl ShutdownReason {
    /// Short human-readable label.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::ShutdownReason;
    ///
    /// assert_eq!(ShutdownReason::Forced.description(), "forced");
    /// ```
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
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{ShutdownReason, Signal};
    ///
    /// assert!(ShutdownReason::Signal(Signal::Interrupt).is_signal());
    /// assert!(!ShutdownReason::Requested.is_signal());
    /// ```
    #[must_use]
    pub const fn is_signal(self) -> bool {
        matches!(self, Self::Signal(_))
    }

    /// The signal behind this reason, if any.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{ShutdownReason, Signal};
    ///
    /// assert_eq!(ShutdownReason::Signal(Signal::Quit).signal(), Some(Signal::Quit));
    /// assert_eq!(ShutdownReason::Error.signal(), None);
    /// ```
    #[must_use]
    pub const fn signal(self) -> Option<Signal> {
        match self {
            Self::Signal(sig) => Some(sig),
            _ => None,
        }
    }

    /// Conventional process exit status for this reason, for
    /// `std::process::exit` at the end of `main`:
    ///
    /// | Reason              | Exit code                                   |
    /// | ------------------- | ------------------------------------------- |
    /// | `Signal(sig)`       | [`sig.exit_code()`](Signal::exit_code) (128 + number; 130 for SIGINT, 143 for SIGTERM) |
    /// | `Requested`         | 0                                           |
    /// | `Forced`, `Timeout`, `Error` | 1                                  |
    ///
    /// Process supervisors use the 128 + n convention to tell "stopped
    /// by a signal" apart from "finished" and "failed".
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::{ShutdownReason, Signal};
    ///
    /// assert_eq!(ShutdownReason::Signal(Signal::Interrupt).exit_code(), 130);
    /// assert_eq!(ShutdownReason::Requested.exit_code(), 0);
    /// assert_eq!(ShutdownReason::Error.exit_code(), 1);
    /// ```
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self {
            Self::Signal(sig) => sig.exit_code(),
            Self::Requested => 0,
            Self::Forced | Self::Timeout | Self::Error => 1,
        }
    }

    /// Pack into a non-zero `u32` for lock-free storage.
    pub(crate) const fn encode(self) -> u32 {
        match self {
            Self::Requested => 1,
            Self::Forced => 2,
            Self::Timeout => 3,
            Self::Error => 4,
            Self::Signal(sig) => 0x100 | sig.bit() as u32,
        }
    }

    /// Inverse of [`encode`](Self::encode). `None` for `0` (unset) and
    /// for values `encode` never produces.
    pub(crate) fn decode(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::Requested),
            2 => Some(Self::Forced),
            3 => Some(Self::Timeout),
            4 => Some(Self::Error),
            c if c & 0x100 != 0 => Signal::ALL
                .iter()
                .copied()
                .find(|sig| u32::from(sig.bit()) == c & 0xFF)
                .map(Self::Signal),
            _ => None,
        }
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

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn all_reasons() -> Vec<ShutdownReason> {
        let mut v = vec![
            ShutdownReason::Requested,
            ShutdownReason::Forced,
            ShutdownReason::Timeout,
            ShutdownReason::Error,
        ];
        v.extend(Signal::ALL.iter().copied().map(ShutdownReason::Signal));
        v
    }

    #[test]
    fn test_reason_description_labels_are_stable() {
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
    fn test_reason_is_signal_only_for_signal_variant() {
        assert!(ShutdownReason::Signal(Signal::Interrupt).is_signal());
        assert!(!ShutdownReason::Requested.is_signal());
        assert!(!ShutdownReason::Forced.is_signal());
    }

    #[test]
    fn test_reason_display_renders_every_variant() {
        assert!(
            ShutdownReason::Signal(Signal::Terminate)
                .to_string()
                .starts_with("Signal(Terminate")
        );
        assert_eq!(ShutdownReason::Requested.to_string(), "Requested");
        assert_eq!(ShutdownReason::Forced.to_string(), "Forced");
        assert_eq!(ShutdownReason::Timeout.to_string(), "Timeout");
        assert_eq!(ShutdownReason::Error.to_string(), "Error");
    }

    #[test]
    fn test_reason_encode_decode_round_trips_every_variant() {
        for r in all_reasons() {
            let code = r.encode();
            assert_ne!(code, 0);
            assert_eq!(ShutdownReason::decode(code), Some(r));
        }
        assert_eq!(ShutdownReason::decode(0), None);
        assert_eq!(ShutdownReason::decode(99), None);
        assert_eq!(ShutdownReason::decode(0x100), None);
    }

    #[test]
    fn test_reason_exit_code_follows_convention() {
        assert_eq!(ShutdownReason::Requested.exit_code(), 0);
        assert_eq!(ShutdownReason::Forced.exit_code(), 1);
        assert_eq!(ShutdownReason::Timeout.exit_code(), 1);
        assert_eq!(ShutdownReason::Error.exit_code(), 1);
        assert_eq!(
            ShutdownReason::Signal(Signal::Terminate).exit_code(),
            Signal::Terminate.exit_code()
        );
    }

    #[test]
    fn test_reason_signal_extracts_only_signal_variant() {
        assert_eq!(
            ShutdownReason::Signal(Signal::Hangup).signal(),
            Some(Signal::Hangup)
        );
        assert_eq!(ShutdownReason::Timeout.signal(), None);
    }
}
