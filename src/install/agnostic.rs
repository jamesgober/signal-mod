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
//! - **Windows:** a console control handler (`SetConsoleCtrlHandler`).
//!   The OS calls it on a fresh thread per event, so no listener thread
//!   is needed. It tells the five console events apart, and for
//!   `CTRL_CLOSE` / `CTRL_LOGOFF` / `CTRL_SHUTDOWN` it keeps the handler
//!   thread parked after delivering, because the OS terminates the
//!   process as soon as the handler returns: parking gives the program
//!   the OS grace period (about 5 seconds) to shut down. When no
//!   installed coordinator covers an event, the handler declines it and
//!   the OS default action (terminate) applies.

use std::sync::Arc;

use crate::dispatch::Dispatcher;
use crate::error::Result;
use crate::signal::SignalSet;

/// A live agnostic installation.
#[derive(Debug)]
pub(crate) struct Registration {
    stop: Stop,
    covered: SignalSet,
}

impl Registration {
    pub(crate) fn covered(&self) -> SignalSet {
        self.covered
    }

    pub(crate) fn uninstall(self) {
        self.stop.stop();
        #[cfg(unix)]
        super::restore::release(self.covered);
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

    // Creating the iterator only allocates its self-pipe; a failure is
    // resource exhaustion, not a property of any one signal.
    let mut signals = Signals::new(core::iter::empty::<i32>()).map_err(Error::ListenerThread)?;
    for sig in set {
        if let Some(number) = sig.unix_number() {
            if let Err(e) = signals.add_signal(number) {
                signals.handle().close();
                return Err(super::registration_error(sig, e));
            }
        }
    }
    if let Err((sig, e)) = super::restore::acquire(set) {
        signals.handle().close();
        return Err(super::registration_error(sig, e));
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
            })
        }
        Err(e) => {
            handle.close();
            super::restore::release(set);
            Err(Error::ListenerThread(e))
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
        console::unregister(self.0);
    }
}

#[cfg(windows)]
pub(crate) fn install(dispatcher: &Arc<Dispatcher>, set: SignalSet) -> Result<Registration> {
    use crate::signal::Signal;

    let covered = set.available_on_current_platform();
    let id = console::register(dispatcher, covered).map_err(|e| {
        let first = covered.iter().next().unwrap_or(Signal::Interrupt);
        super::registration_error(first, e)
    })?;
    sm_debug!(signals = ?covered, "console control handler registered");
    Ok(Registration {
        stop: Stop(id),
        covered,
    })
}

#[cfg(windows)]
mod console {
    //! The process-wide console control handler.
    //!
    //! `SetConsoleCtrlHandler` takes a bare function pointer with no
    //! context argument, so the handler finds its coordinators through a
    //! process-global registry. That global is unavoidable and is only
    //! touched under its mutex.

    // The single FFI call into `SetConsoleCtrlHandler` below; it carries
    // a `SAFETY:` justification. Everything else in the crate stays safe.
    #![allow(unsafe_code)]

    use std::io;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, OnceLock, Weak};

    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_LOGOFF_EVENT, CTRL_SHUTDOWN_EVENT,
        SetConsoleCtrlHandler,
    };
    use windows_sys::core::BOOL;

    use crate::dispatch::Dispatcher;
    use crate::signal::{Signal, SignalSet};

    const TRUE: BOOL = 1;
    const FALSE: BOOL = 0;

    struct Entry {
        id: u64,
        dispatcher: Weak<Dispatcher>,
        covered: SignalSet,
    }

    static LISTENERS: Mutex<Vec<Entry>> = Mutex::new(Vec::new());
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    /// Outcome of the one-time `SetConsoleCtrlHandler` call (raw OS
    /// error code on failure).
    static HANDLER: OnceLock<core::result::Result<(), i32>> = OnceLock::new();

    fn listeners() -> std::sync::MutexGuard<'static, Vec<Entry>> {
        match LISTENERS.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Map a console control event to its signal and whether the OS
    /// terminates the process when the handler returns.
    pub(super) const fn classify(ctrl: u32) -> Option<(Signal, bool)> {
        match ctrl {
            CTRL_C_EVENT => Some((Signal::Interrupt, false)),
            CTRL_BREAK_EVENT => Some((Signal::Quit, false)),
            CTRL_CLOSE_EVENT => Some((Signal::Terminate, true)),
            CTRL_LOGOFF_EVENT | CTRL_SHUTDOWN_EVENT => Some((Signal::Hangup, true)),
            _ => None,
        }
    }

    pub(super) fn register(dispatcher: &Arc<Dispatcher>, covered: SignalSet) -> io::Result<u64> {
        let installed = HANDLER.get_or_init(|| {
            // SAFETY: `handler` has exactly the `HandlerRoutine`
            // signature (`extern "system" fn(u32) -> BOOL`), is a plain
            // function that stays valid for the life of the process,
            // never unwinds (it catches panics), and touches only
            // thread-safe state. `SetConsoleCtrlHandler(_, TRUE)` has
            // no other preconditions and only appends to the process's
            // handler list.
            let ok = unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) };
            if ok == FALSE {
                Err(io::Error::last_os_error().raw_os_error().unwrap_or(-1))
            } else {
                Ok(())
            }
        });
        if let Err(code) = installed {
            return Err(io::Error::from_raw_os_error(*code));
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        listeners().push(Entry {
            id,
            dispatcher: Arc::downgrade(dispatcher),
            covered,
        });
        Ok(id)
    }

    pub(super) fn unregister(id: u64) {
        listeners().retain(|entry| entry.id != id && entry.dispatcher.strong_count() > 0);
    }

    /// Deliver `sig` to every live coordinator covering it. Returns
    /// whether anyone did.
    pub(super) fn dispatch(sig: Signal) -> bool {
        let targets: Vec<Arc<Dispatcher>> = listeners()
            .iter()
            .filter(|entry| entry.covered.contains(sig))
            .filter_map(|entry| entry.dispatcher.upgrade())
            .collect();
        for target in &targets {
            target.deliver(sig);
        }
        !targets.is_empty()
    }

    extern "system" fn handler(ctrl: u32) -> BOOL {
        let Some((sig, terminates)) = classify(ctrl) else {
            return FALSE;
        };
        // A panic must not unwind across the FFI boundary. A payload
        // here would come from an executor's waker; nothing useful can
        // be done with it on the OS handler thread.
        let delivered = std::panic::catch_unwind(|| dispatch(sig)).unwrap_or(false);
        if !delivered {
            // Nobody covers this event: let the next handler (or the
            // OS default, terminate) run.
            return FALSE;
        }
        if terminates {
            // Returning would terminate the process immediately. Park
            // instead; the process exits when `main` returns or when the
            // OS grace period ends.
            loop {
                std::thread::park();
            }
        }
        TRUE
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn test_classify_maps_every_console_event() {
            assert_eq!(classify(CTRL_C_EVENT), Some((Signal::Interrupt, false)));
            assert_eq!(classify(CTRL_BREAK_EVENT), Some((Signal::Quit, false)));
            assert_eq!(classify(CTRL_CLOSE_EVENT), Some((Signal::Terminate, true)));
            assert_eq!(classify(CTRL_LOGOFF_EVENT), Some((Signal::Hangup, true)));
            assert_eq!(classify(CTRL_SHUTDOWN_EVENT), Some((Signal::Hangup, true)));
            assert_eq!(classify(99), None);
        }

        #[test]
        fn test_unknown_event_is_declined() {
            assert_eq!(handler(99), FALSE);
        }
    }
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
        assert!(console::dispatch(Signal::Quit));
        assert_eq!(inner.reason(), Some(ShutdownReason::Signal(Signal::Quit)));
        reg.uninstall();
        assert!(!console::dispatch(Signal::Quit));
    }
}
