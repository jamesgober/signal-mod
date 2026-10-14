//! Diagnostics macros.
//!
//! With the `tracing` feature these forward to the `tracing` macros of
//! the same name. Without it they expand to nothing and their arguments
//! are not evaluated, so call sites must not compute anything for the
//! log alone.

// Not every feature combination logs at every level.
#![allow(unused_macros)]

macro_rules! sm_debug {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        {
            tracing::debug!(target: "signal_mod", $($arg)*);
        }
    }};
}

macro_rules! sm_info {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        {
            tracing::info!(target: "signal_mod", $($arg)*);
        }
    }};
}

macro_rules! sm_warn {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        {
            tracing::warn!(target: "signal_mod", $($arg)*);
        }
    }};
}

macro_rules! sm_error {
    ($($arg:tt)*) => {{
        #[cfg(feature = "tracing")]
        {
            tracing::error!(target: "signal_mod", $($arg)*);
        }
    }};
}
