//! Synchronous `ctrlc` back-end for
//! [`Coordinator::install`](crate::Coordinator::install).
//!
//! Selected when neither `tokio` nor `async-std` is enabled but
//! `ctrlc-fallback` is. The `ctrlc` crate registers a single
//! process-global handler for `SIGINT` / Ctrl+C; coverage is therefore
//! limited to [`Signal::Interrupt`]. Other variants in the configured
//! set are silently skipped.

use crate::coord::Coordinator;
use crate::error::{Error, Result};
use crate::reason::ShutdownReason;
use crate::signal::Signal;

/// Install a `ctrlc` handler for [`Signal::Interrupt`] if present in
/// the configured set.
pub(crate) fn install(coord: &Coordinator) -> Result<()> {
    let trigger = coord.trigger();
    if coord.signals().contains(Signal::Interrupt) {
        ctrlc::try_set_handler(move || {
            let _ = trigger.trigger(ShutdownReason::Signal(Signal::Interrupt));
        })
        .map_err(|e| Error::SignalRegistration {
            signal: Signal::Interrupt,
            source: std::io::Error::other(e),
        })?;
    }
    Ok(())
}
