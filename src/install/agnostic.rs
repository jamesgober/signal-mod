//! Runtime-agnostic back-end.
//!
//! Deliveries feed the runtime-agnostic token, so this back-end works
//! under smol, async-std, any other executor, or plain threads.
//!
//! - **Unix:** one listener thread draining a signal-hook
//!   [`Signals`](signal_hook::iterator::Signals) iterator. Every signal
//!   is registered individually, so a failure names the signal that
//!   failed (1.0.0's async-std back-end blamed the first signal in the
//!   set).
//! - **Windows:** the process-wide console control handler in
//!   [`console`](super::console), shared with the Tokio back-end. The OS
//!   calls it on a fresh thread per event, so no listener thread is
//!   needed.

use std::sync::Arc;

use crate::dispatch::Dispatcher;
use crate::error::Result;
use crate::signal::SignalSet;

/// A live agnostic installation.
#[derive(Debug)]
pub(crate) struct Registration {
    stop: Stop,
    covered: SignalSet,
    /// Signals counted by [`restore::acquire`](super::restore::acquire).
    #[cfg(unix)]
    restored: SignalSet,
}

impl Registration {
    pub(crate) fn covered(&self) -> SignalSet {
        self.covered
    }

    pub(crate) fn uninstall(self) {
        self.stop.stop();
        #[cfg(unix)]
        super::restore::release(self.restored);
        sm_debug!(signals = ?self.covered, "agnostic signal listener removed");
    }
}

// ---------------------------------------------------------------- Unix

#[cfg(unix)]
#[derive(Debug)]
struct Stop(signal_hook::iterator::Handle);

#[cfg(unix)]
impl Stop {
    fn stop(&self) {
        // Closing the handle ends the iterator; the thread then exits.
        self.0.close();
    }
}

#[cfg(unix)]
pub(crate) fn install(dispatcher: &Arc<Dispatcher>, set: SignalSet) -> Result<Registration> {
    use crate::error::Error;
    use crate::signal::Signal;
    use signal_hook::iterator::Signals;

    // Read the prior dispositions before anything is registered.
    let restored = super::restore::acquire(set, dispatcher.shutdown_set())
        .map_err(|(sig, e)| super::registration_error(sig, e))?;
    let fail = |error: Error| {
        super::restore::release(restored);
        error
    };

    // Creating the iterator only allocates its self-pipe; a failure is
    // resource exhaustion, not a property of any one signal.
    let mut signals =
        Signals::new(core::iter::empty::<i32>()).map_err(|e| fail(Error::ListenerThread(e)))?;
    for sig in set {
        if let Some(number) = sig.unix_number() {
            if let Err(e) = signals.add_signal(number) {
                signals.handle().close();
                return Err(fail(super::registration_error(sig, e)));
            }
        }
    }

    let handle = signals.handle();
    let listener = Arc::clone(dispatcher);
    let spawned = std::thread::Builder::new()
        .name("signal-mod-signals".into())
        .spawn(move || {
            for number in signals.forever() {
                if let Some(sig) = Signal::from_unix_number(number) {
                    listener.deliver(sig);
                }
            }
        });
    match spawned {
        Ok(_detached) => {
            sm_debug!(signals = ?set, "agnostic signal listener thread started");
            Ok(Registration {
                stop: Stop(handle),
                covered: set,
                restored,
            })
        }
        Err(e) => {
            handle.close();
            Err(fail(Error::ListenerThread(e)))
        }
    }
}

// ------------------------------------------------------------- Windows

#[cfg(windows)]
#[derive(Debug)]
struct Stop(u64);

#[cfg(windows)]
impl Stop {
    fn stop(&self) {
        super::console::unregister(self.0);
    }
}

#[cfg(windows)]
pub(crate) fn install(dispatcher: &Arc<Dispatcher>, set: SignalSet) -> Result<Registration> {
    let covered = set.available_on_current_platform();
    // One OS call covers every event, so a failure is not attributable to
    // a single signal.
    let id = super::console::register(dispatcher, covered)
        .map_err(crate::error::Error::ListenerThread)?;
    sm_debug!(signals = ?covered, "console control handler registered");
    Ok(Registration {
        stop: Stop(id),
        covered,
    })
}

// ---------------------------------------------------------------- other

#[cfg(not(any(unix, windows)))]
#[derive(Debug)]
struct Stop;

#[cfg(not(any(unix, windows)))]
impl Stop {
    fn stop(&self) {}
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn install(_dispatcher: &Arc<Dispatcher>, _set: SignalSet) -> Result<Registration> {
    // No signal source on this platform: an explicit, empty
    // installation (`Coordinator::installed_signals` reports it).
    sm_warn!("no signal source on this platform; install() covers no signals");
    Ok(Registration {
        stop: Stop,
        covered: SignalSet::empty(),
    })
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::reason::ShutdownReason;
    use crate::signal::Signal;
    use crate::state::Inner;
    use crate::token::ShutdownTrigger;

    #[test]
    fn test_console_dispatch_reaches_registered_coordinator_until_uninstalled() {
        let inner = Inner::new();
        let dispatcher = Arc::new(Dispatcher::new(
            ShutdownTrigger::new(crate::sync::Arc::clone(&inner)),
            SignalSet::from(Signal::Quit),
            false,
        ));
        let reg = match install(&dispatcher, SignalSet::from(Signal::Quit)) {
            Ok(reg) => reg,
            Err(e) => panic!("install failed: {e}"),
        };
        assert_eq!(reg.covered(), SignalSet::from(Signal::Quit));
        assert!(super::super::console::dispatch(Signal::Quit));
        assert_eq!(inner.reason(), Some(ShutdownReason::Signal(Signal::Quit)));
        reg.uninstall();
        assert!(!super::super::console::dispatch(Signal::Quit));
    }
}
