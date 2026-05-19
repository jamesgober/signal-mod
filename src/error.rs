//! Error type and `Result` alias.

use core::fmt;
use std::io;

use crate::signal::Signal;

/// Errors produced by `mod-signal` operations.
#[non_exhaustive]
#[derive(Debug)]
pub enum Error {
    /// `Coordinator::install` was called twice on the same
    /// coordinator. Signal handler installation is idempotent within
    /// a coordinator and the second call is rejected.
    AlreadyInstalled,

    /// The platform rejected registration of a specific signal.
    SignalRegistration {
        /// Which signal failed to register.
        signal: Signal,
        /// The underlying OS error.
        source: io::Error,
    },

    /// The coordinator was used in a state that disallows the
    /// requested operation. Carries a static description.
    InvalidState(&'static str),

    /// A timed operation exceeded its budget. Carries a static
    /// description of which operation.
    Timeout(&'static str),

    /// `Coordinator::install` was called with no async-runtime
    /// feature and no `ctrlc-fallback` feature enabled, so there is
    /// no back-end available to attach signal handlers.
    NoRuntime,
}

impl fmt::Display for Error {
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
                "no async-runtime feature and no ctrlc-fallback feature enabled; \
                 enable `tokio`, `async-std`, or `ctrlc-fallback`",
            ),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SignalRegistration { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Convenience alias for `Result<T, mod_signal::Error>`.
pub type Result<T> = core::result::Result<T, Error>;
