//! async-std back-end for [`Coordinator::install`](crate::Coordinator::install).
//!
//! Uses `signal-hook-async-std` on Unix to drive a single
//! `Signals` stream covering every configured Unix signal number,
//! and a synchronous `ctrlc::try_set_handler` on Windows because
//! async-std does not ship a native Windows signal stream.

use crate::coord::Coordinator;
use crate::error::{Error, Result};
use crate::reason::ShutdownReason;
use crate::signal::Signal;

/// Install async-std-backed signal listeners for the configured set.
///
/// On Unix, must be called from inside an async-std runtime (a
/// `#[async_std::main]` or `#[async_std::test]` function, or an
/// explicit `async_std::task::block_on` block).
pub(crate) fn install(coord: &Coordinator) -> Result<()> {
    let trigger = coord.trigger();
    let set = coord.signals();

    #[cfg(unix)]
    {
        use futures::stream::StreamExt;
        use signal_hook_async_std::Signals as SHSignals;

        let mut signum_to_variant: Vec<(i32, Signal)> = Vec::new();
        for sig in set.iter() {
            if let Some(n) = sig.unix_number() {
                signum_to_variant.push((n, sig));
            }
        }

        if signum_to_variant.is_empty() {
            return Ok(());
        }

        let nums: Vec<i32> = signum_to_variant.iter().map(|(n, _)| *n).collect();
        let signals = SHSignals::new(&nums).map_err(|e| {
            let first = signum_to_variant
                .first()
                .map(|(_, s)| *s)
                .unwrap_or(Signal::Terminate);
            Error::SignalRegistration {
                signal: first,
                source: e,
            }
        })?;
        let t = trigger.clone();
        async_std::task::spawn(async move {
            let mut signals = signals;
            while let Some(num) = signals.next().await {
                if let Some(sig) = signum_to_variant
                    .iter()
                    .find(|(n, _)| *n == num)
                    .map(|(_, s)| *s)
                {
                    let _ = t.trigger(ShutdownReason::Signal(sig));
                }
            }
        });
    }

    #[cfg(windows)]
    {
        if set.contains(Signal::Interrupt) {
            let t = trigger.clone();
            ctrlc::try_set_handler(move || {
                let _ = t.trigger(ShutdownReason::Signal(Signal::Interrupt));
            })
            .map_err(|e| Error::SignalRegistration {
                signal: Signal::Interrupt,
                source: std::io::Error::other(e),
            })?;
        }
        let _ = &trigger;
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (&trigger, set);
    }

    Ok(())
}
