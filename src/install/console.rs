//! The process-wide Windows console control handler, shared by both
//! back-ends.
//!
//! `SetConsoleCtrlHandler` takes a bare function pointer with no
//! context argument, so the handler finds its coordinators through a
//! process-global registry. That global is unavoidable and is only
//! touched under its mutex.
//!
//! Windows calls console handlers last-registered first and stops at the
//! first one that returns `TRUE`. Routing every coordinator, Tokio or
//! agnostic, through this one handler is what lets all of them receive
//! each event (two separate handlers would hide events from each other).
//! It tells the five console events apart, and for `CTRL_CLOSE` /
//! `CTRL_LOGOFF` / `CTRL_SHUTDOWN` it keeps the OS handler thread parked
//! after delivering, because the OS terminates the process as soon as the
//! handler returns: parking gives the program the OS grace period (about
//! 5 seconds) to shut down. When no installed coordinator covers an
//! event, the handler declines it and the next handler (ultimately the
//! OS default, terminate) runs.

// The single FFI call into `SetConsoleCtrlHandler` below; it carries
// a `SAFETY:` justification. Everything else in the crate stays safe.
#![allow(unsafe_code)]

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

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
