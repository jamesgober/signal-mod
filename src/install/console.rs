//! The process-wide Windows console control handler of the agnostic
//! back-end, used only without the `tokio` feature (with it, both
//! back-ends go through Tokio's handler; see `windows_events`).
//!
//! `SetConsoleCtrlHandler` takes a bare function pointer with no
//! context argument, so the handler finds its coordinators through a
//! process-global registry. That global is unavoidable and is only
//! touched under its mutex.
//!
//! Windows calls console handlers last-registered first and stops at the
//! first one that returns `TRUE`. Routing every coordinator through this
//! one handler is what lets all of them receive each event (one handler
//! per coordinator would hide events from each other).
//! It tells the console events apart, and for `CTRL_CLOSE` /
//! `CTRL_SHUTDOWN` it keeps the OS handler thread parked after
//! delivering, because the OS terminates the process as soon as the
//! handler returns: parking gives the program the OS grace period (about
//! 5 seconds) to shut down. When no installed coordinator covers an
//! event, the handler declines it and the next handler (ultimately the
//! OS default, terminate) runs.
//!
//! `CTRL_LOGOFF` is always declined. It reaches services only, whenever
//! any user logs off, and the OS does not end a service for it; treating
//! it as a signal stopped services at every logoff and left one parked
//! OS thread behind each time.

// The single FFI call into `SetConsoleCtrlHandler` below; it carries
// a `SAFETY:` justification. Everything else in the crate stays safe.
#![allow(unsafe_code)]

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use windows_sys::Win32::System::Console::{
    CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, CTRL_SHUTDOWN_EVENT, SetConsoleCtrlHandler,
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
/// Whether the handler is registered with the OS. A failed registration
/// is retried by the next install instead of being remembered.
static HANDLER_INSTALLED: Mutex<bool> = Mutex::new(false);

fn listeners() -> std::sync::MutexGuard<'static, Vec<Entry>> {
    match LISTENERS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Map a console control event to its signal and whether the OS
/// terminates the process when the handler returns. `None` (declined)
/// for `CTRL_LOGOFF_EVENT` and unknown events.
pub(super) const fn classify(ctrl: u32) -> Option<(Signal, bool)> {
    match ctrl {
        CTRL_C_EVENT => Some((Signal::Interrupt, false)),
        CTRL_BREAK_EVENT => Some((Signal::Quit, false)),
        CTRL_CLOSE_EVENT | CTRL_SHUTDOWN_EVENT => Some((Signal::Terminate, true)),
        _ => None,
    }
}

pub(super) fn register(dispatcher: &Arc<Dispatcher>, covered: SignalSet) -> io::Result<u64> {
    {
        let mut installed = match HANDLER_INSTALLED.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if !*installed {
            // SAFETY: `handler` has exactly the `HandlerRoutine`
            // signature (`extern "system" fn(u32) -> BOOL`), is a plain
            // function that stays valid for the life of the process,
            // never unwinds (it catches panics), and touches only
            // thread-safe state. `SetConsoleCtrlHandler(_, TRUE)` has no
            // other preconditions and only appends to the process's
            // handler list.
            let ok = unsafe { SetConsoleCtrlHandler(Some(handler), TRUE) };
            if ok == FALSE {
                return Err(io::Error::last_os_error());
            }
            *installed = true;
        }
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
    use windows_sys::Win32::System::Console::CTRL_LOGOFF_EVENT;

    #[test]
    fn test_classify_maps_every_console_event() {
        assert_eq!(classify(CTRL_C_EVENT), Some((Signal::Interrupt, false)));
        assert_eq!(classify(CTRL_BREAK_EVENT), Some((Signal::Quit, false)));
        assert_eq!(classify(CTRL_CLOSE_EVENT), Some((Signal::Terminate, true)));
        assert_eq!(
            classify(CTRL_SHUTDOWN_EVENT),
            Some((Signal::Terminate, true))
        );
        assert_eq!(classify(CTRL_LOGOFF_EVENT), None);
        assert_eq!(classify(99), None);
    }

    // Review F-2: `CTRL_LOGOFF_EVENT` reaches services whenever any user
    // logs off. It was delivered as `Hangup` (in the default shutdown
    // set) and the handler thread then parked forever: the service
    // stopped, and every logoff leaked one OS thread. It must be declined
    // at once, even with a coordinator covering every signal.
    #[test]
    fn test_logoff_is_declined_without_parking_or_delivering() {
        use crate::state::Inner;
        use crate::token::ShutdownTrigger;

        let inner = Inner::new();
        let dispatcher = Arc::new(Dispatcher::new(
            ShutdownTrigger::new(crate::sync::Arc::clone(&inner)),
            SignalSet::all(),
            false,
        ));
        let id = register(&dispatcher, SignalSet::all()).unwrap_or_else(|e| panic!("{e}"));
        let (tx, rx) = std::sync::mpsc::channel();
        let _thread = std::thread::spawn(move || {
            let _sent = tx.send(handler(CTRL_LOGOFF_EVENT));
        });
        let answer = rx.recv_timeout(std::time::Duration::from_secs(5));
        unregister(id);
        assert_eq!(answer, Ok(FALSE), "logoff parked the handler thread");
        assert!(!inner.is_initiated(), "logoff initiated shutdown");
    }

    #[test]
    fn test_unknown_event_is_declined() {
        assert_eq!(handler(99), FALSE);
    }
}
