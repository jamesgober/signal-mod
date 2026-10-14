//! Error type and `Result` alias.

use core::fmt;
use std::io;

use crate::signal::Signal;

/// Errors produced by `signal-mod` operations.
///
/// Every variant says what failed and what to do about it; see the
/// per-variant documentation.
///
/// # Examples
///
/// ```
/// use signal_mod::{Coordinator, Error};
///
/// let coord = Coordinator::builder().build();
/// match coord.install() {
///     Ok(()) => {}
///     Err(Error::AlreadyInstalled) => unreachable!("first install"),
///     Err(e) => eprintln!("running without signal handlers: {e}"),
/// }
/// ```
#[non_exhaustive]
#[derive(Debug)]
pub enum Error {
    /// [`Coordinator::install`](crate::Coordinator::install) was called
    /// while this coordinator already had handlers installed. The
    /// existing installation is untouched. Call
    /// [`uninstall`](crate::Coordinator::uninstall) first to replace
    /// it.
    AlreadyInstalled,

    /// The platform rejected registration of a specific signal. Nothing
    /// was installed (registration is all-or-nothing). `source` carries
    /// the OS error; typical causes are a signal the platform forbids
    /// handling, or a Tokio runtime built without its I/O driver
    /// (`enable_io` / `enable_all`).
    SignalRegistration {
        /// Which signal failed to register.
        signal: Signal,
        /// The underlying OS error.
        source: io::Error,
    },

    /// Never produced. Kept so that 1.x code matching on it still
    /// compiles; will be removed in 3.0.
    #[deprecated(since = "2.0.0", note = "never constructed; will be removed in 3.0")]
    InvalidState(&'static str),

    /// Never produced. Kept so that 1.x code matching on it still
    /// compiles; will be removed in 3.0.
    #[deprecated(since = "2.0.0", note = "never constructed; will be removed in 3.0")]
    Timeout(&'static str),

    /// No back-end feature is enabled, so there is nothing that could
    /// attach signal handlers. Enable `tokio` (default) or `agnostic`
    /// (also reachable through the `smol`, `ctrlc-fallback` and
    /// `async-std` aliases). Programmatic shutdown through
    /// [`ShutdownTrigger`](crate::ShutdownTrigger) keeps working.
    NoRuntime,

    /// Only the `tokio` back-end is enabled and
    /// [`Coordinator::install`](crate::Coordinator::install) was called
    /// outside a Tokio runtime. Call it from inside the runtime
    /// (`#[tokio::main]`, `Runtime::block_on`, or after
    /// `Handle::enter`), or enable the `agnostic` feature, which needs
    /// no runtime. Nothing was installed.
    OutsideTokioRuntime,

    /// The agnostic back-end could not start its listener thread
    /// (`std::thread::Builder::spawn` failed, usually from resource
    /// exhaustion). Nothing was installed; the call can be retried.
    ListenerThread(io::Error),
}

impl fmt::Display for Error {
    #[allow(deprecated)] // must still render the deprecated variants
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => {
                f.write_str("signal handlers already installed for this coordinator")
            }
            Self::SignalRegistration { signal, source } => {
                write!(f, "failed to register handler for {signal}: {source}")
            }
            Self::InvalidState(s) => write!(f, "invalid coordinator state: {s}"),
            Self::Timeout(s) => write!(f, "operation timed out: {s}"),
            Self::NoRuntime => f.write_str(
                "no signal back-end feature enabled; enable `tokio` or `agnostic` \
                 (aliases: `smol`, `ctrlc-fallback`, `async-std`)",
            ),
            Self::OutsideTokioRuntime => f.write_str(
                "install() was called outside a Tokio runtime and the `agnostic` \
                 feature is not enabled; call it inside the runtime",
            ),
            Self::ListenerThread(source) => {
                write!(f, "failed to start the signal listener thread: {source}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SignalRegistration { source, .. } | Self::ListenerThread(source) => Some(source),
            _ => None,
        }
    }
}

/// Convenience alias for `Result<T, signal_mod::Error>`.
pub type Result<T> = core::result::Result<T, Error>;

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use std::error::Error as _;

    fn os_error() -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, "denied by test")
    }

    #[test]
    #[allow(deprecated)] // exercises the deprecated variants on purpose
    fn test_error_display_names_the_failure_for_every_variant() {
        let cases: Vec<(Error, &str)> = vec![
            (Error::AlreadyInstalled, "already installed"),
            (
                Error::SignalRegistration {
                    signal: Signal::Terminate,
                    source: os_error(),
                },
                "failed to register handler for Terminate",
            ),
            (Error::InvalidState("x"), "invalid coordinator state: x"),
            (Error::Timeout("y"), "operation timed out: y"),
            (Error::NoRuntime, "no signal back-end feature enabled"),
            (Error::OutsideTokioRuntime, "outside a Tokio runtime"),
            (
                Error::ListenerThread(os_error()),
                "failed to start the signal listener thread",
            ),
        ];
        for (err, needle) in cases {
            let text = err.to_string();
            assert!(text.contains(needle), "{text:?} lacks {needle:?}");
        }
    }

    #[test]
    #[allow(deprecated)] // exercises the deprecated variants on purpose
    fn test_error_source_is_the_os_error_only_where_one_exists() {
        let reg = Error::SignalRegistration {
            signal: Signal::Quit,
            source: os_error(),
        };
        assert_eq!(
            reg.source().map(ToString::to_string).as_deref(),
            Some("denied by test")
        );
        assert!(Error::ListenerThread(os_error()).source().is_some());
        assert!(Error::AlreadyInstalled.source().is_none());
        assert!(Error::NoRuntime.source().is_none());
        assert!(Error::OutsideTokioRuntime.source().is_none());
        assert!(Error::InvalidState("x").source().is_none());
        assert!(Error::Timeout("x").source().is_none());
    }

    #[test]
    fn test_error_is_send_sync_static() {
        fn assert_bounds<T: Send + Sync + 'static>() {}
        assert_bounds::<Error>();
    }
}
