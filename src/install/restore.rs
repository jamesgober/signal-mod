//! Default-disposition restoration on Unix.
//!
//! Neither Tokio nor signal-hook ever removes an OS-level signal
//! handler. Without help, a signal that arrives after the coordinator
//! that handled it was dropped is swallowed: SIGINT and SIGTERM stop
//! working for the rest of the process.
//!
//! For every signal a back-end installs, this module registers (once per
//! process) a signal-hook "conditional default" action guarded by a
//! flag. While at least one coordinator has the signal installed the
//! flag is off and the action does nothing. When the last one
//! uninstalls, the flag turns on and the next delivery runs the
//! signal's default action (terminate, or core dump for SIGQUIT), as if
//! no handler had ever been installed.
//!
//! SIGPIPE is excluded: the Rust runtime ignores it by default, and
//! "ignored" is what a leftover handler that does nothing already gives.
//!
//! The table is process-global because signal dispositions are; that
//! is the documented, unavoidable exception to the no-global-state rule.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::signal::{Signal, SignalSet};

struct Slot {
    restore_default: Arc<AtomicBool>,
    installs: usize,
}

static TABLE: Mutex<[Option<Slot>; 7]> = Mutex::new([const { None }; 7]);

fn index(sig: Signal) -> Option<usize> {
    Signal::ALL.iter().position(|s| *s == sig)
}

/// Signals this module manages within `set`.
fn managed(set: SignalSet) -> SignalSet {
    set.without(Signal::Pipe)
}

/// Record that a back-end now handles every signal in `set`.
///
/// # Errors
///
/// [`io::Error`] (with the failing signal) if the conditional default
/// action cannot be registered. Signals acquired before the failure are
/// released again, so the table is unchanged on error.
pub(crate) fn acquire(set: SignalSet) -> Result<(), (Signal, io::Error)> {
    let mut table = match TABLE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut done = SignalSet::empty();
    for sig in managed(set) {
        let (Some(i), Some(number)) = (index(sig), sig.unix_number()) else {
            continue;
        };
        let Some(entry) = table.get_mut(i) else {
            continue;
        };
        if entry.is_none() {
            let flag = Arc::new(AtomicBool::new(false));
            match signal_hook::flag::register_conditional_default(number, Arc::clone(&flag)) {
                Ok(_id) => {
                    *entry = Some(Slot {
                        restore_default: flag,
                        installs: 0,
                    });
                }
                Err(error) => {
                    release_locked(&mut table, done);
                    return Err((sig, error));
                }
            }
        }
        if let Some(slot) = entry.as_mut() {
            slot.installs += 1;
            slot.restore_default.store(false, Ordering::SeqCst);
        }
        done = done.with(sig);
    }
    Ok(())
}

/// Record that a back-end no longer handles the signals in `set`.
pub(crate) fn release(set: SignalSet) {
    let mut table = match TABLE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    release_locked(&mut table, managed(set));
}

fn release_locked(table: &mut [Option<Slot>; 7], set: SignalSet) {
    for sig in set {
        let Some(slot) = index(sig)
            .and_then(|i| table.get_mut(i))
            .and_then(Option::as_mut)
        else {
            continue;
        };
        slot.installs = slot.installs.saturating_sub(1);
        if slot.installs == 0 {
            slot.restore_default.store(true, Ordering::SeqCst);
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn installs(sig: Signal) -> usize {
        let table = match TABLE.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        index(sig)
            .and_then(|i| table.get(i))
            .and_then(Option::as_ref)
            .map_or(0, |slot| slot.installs)
    }

    #[test]
    fn test_restore_counts_nested_installs_and_skips_pipe() {
        let set = SignalSet::from(Signal::User2) | Signal::Pipe;
        assert!(acquire(set).is_ok());
        assert!(acquire(set).is_ok());
        assert_eq!(installs(Signal::User2), 2);
        assert_eq!(installs(Signal::Pipe), 0);
        release(set);
        assert_eq!(installs(Signal::User2), 1);
        release(set);
        assert_eq!(installs(Signal::User2), 0);
        // Releasing more than was acquired saturates at zero.
        release(set);
        assert_eq!(installs(Signal::User2), 0);
    }
}
