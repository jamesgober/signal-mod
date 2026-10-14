//! Restoring the default action on Unix, only where it was the default.
//!
//! Neither Tokio nor signal-hook ever removes an OS-level signal handler.
//! Without help, a shutdown signal that arrives after the coordinator
//! that handled it was dropped is swallowed: SIGINT and SIGTERM stop
//! working for the rest of the process.
//!
//! The first time any coordinator installs a signal, this module records
//! the signal's disposition **before** signal-mod (or Tokio on its
//! behalf) registers anything for it. Restoration is armed only for
//! signals that were at their default action then, and only while they
//! are used as **shutdown** signals:
//!
//! - A signal that was ignored (`SIG_IGN`, for example SIGHUP under
//!   `nohup`) or already had a handler (another library, the
//!   application's own `tokio::signal`) is never touched: once the last
//!   coordinator is gone it keeps whatever the other code expects.
//! - Notify-only signals (reload, log rotation) are never armed: after
//!   uninstall they are absorbed by the leftover handler, as with plain
//!   `tokio::signal`.
//! - SIGPIPE is never armed: the Rust runtime ignores it.
//!
//! For an armed signal, a signal-hook "conditional default" action
//! guarded by a flag is registered once per process. While at least one
//! coordinator has the signal in its shutdown set the flag is off; when
//! the last one uninstalls, the flag turns on and the next delivery runs
//! the default action (terminate, or core dump for SIGQUIT), as if no
//! handler had ever been installed.
//!
//! The table is process-global because signal dispositions are; that is
//! the documented, unavoidable exception to the no-global-state rule.

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::signal::{Signal, SignalSet};

#[derive(Default)]
struct Slot {
    /// Disposition was `SIG_DFL` when signal-mod first saw the signal.
    was_default: bool,
    /// The conditional default action, once registered.
    restore_default: Option<Arc<AtomicBool>>,
    /// Installed coordinators using the signal as a shutdown signal.
    installs: usize,
}

static TABLE: Mutex<[Option<Slot>; 7]> = Mutex::new([const { None }; 7]);

fn table() -> MutexGuard<'static, [Option<Slot>; 7]> {
    match TABLE.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn index(sig: Signal) -> Option<usize> {
    Signal::ALL.iter().position(|s| *s == sig)
}

/// Record the prior disposition of every signal in `all` not seen
/// before, then count every eligible signal of `shutdown` as installed.
///
/// Must run before the back-end registers anything for these signals.
/// Returns the signals that were counted; pass exactly that set to
/// [`release`].
///
/// # Errors
///
/// [`io::Error`] (with the failing signal) if a conditional default
/// action cannot be registered. Signals counted before the failure are
/// released again.
pub(crate) fn acquire(
    all: SignalSet,
    shutdown: SignalSet,
) -> Result<SignalSet, (Signal, io::Error)> {
    let mut table = table();
    for sig in all.without(Signal::Pipe) {
        let (Some(i), Some(number)) = (index(sig), sig.unix_number()) else {
            continue;
        };
        if let Some(entry) = table.get_mut(i) {
            if entry.is_none() {
                let was_default = match super::disposition::is_default(number) {
                    Ok(default) => default,
                    Err(error) => {
                        sm_warn!(signal = %sig, error = %error,
                            "could not read the signal disposition; it will not be restored");
                        drop(error);
                        false
                    }
                };
                *entry = Some(Slot {
                    was_default,
                    ..Slot::default()
                });
            }
        }
    }

    let mut counted = SignalSet::empty();
    for sig in (shutdown & all).without(Signal::Pipe) {
        let (Some(i), Some(number)) = (index(sig), sig.unix_number()) else {
            continue;
        };
        let Some(slot) = table.get_mut(i).and_then(Option::as_mut) else {
            continue;
        };
        if !slot.was_default {
            continue;
        }
        if slot.restore_default.is_none() {
            let flag = Arc::new(AtomicBool::new(false));
            match signal_hook::flag::register_conditional_default(number, Arc::clone(&flag)) {
                Ok(_id) => slot.restore_default = Some(flag),
                Err(error) => {
                    release_locked(&mut table, counted);
                    return Err((sig, error));
                }
            }
        }
        slot.installs += 1;
        if let Some(flag) = &slot.restore_default {
            flag.store(false, Ordering::SeqCst);
        }
        counted = counted.with(sig);
    }
    Ok(counted)
}

/// Undo an [`acquire`]: `counted` is the set it returned.
pub(crate) fn release(counted: SignalSet) {
    release_locked(&mut table(), counted);
}

fn release_locked(table: &mut [Option<Slot>; 7], counted: SignalSet) {
    for sig in counted {
        let Some(slot) = index(sig)
            .and_then(|i| table.get_mut(i))
            .and_then(Option::as_mut)
        else {
            continue;
        };
        slot.installs = slot.installs.saturating_sub(1);
        if slot.installs == 0 {
            if let Some(flag) = &slot.restore_default {
                flag.store(true, Ordering::SeqCst);
            }
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    fn installs(sig: Signal) -> usize {
        let table = table();
        index(sig)
            .and_then(|i| table.get(i))
            .and_then(Option::as_ref)
            .map_or(0, |slot| slot.installs)
    }

    #[test]
    fn test_restore_counts_shutdown_installs_and_skips_pipe_and_notify() {
        // USR2 is at its default action in the test process.
        let all = SignalSet::from(Signal::User2) | Signal::Pipe | Signal::User1;
        let shutdown = SignalSet::from(Signal::User2) | Signal::Pipe;
        let first = acquire(all, shutdown).unwrap_or_else(|(s, e)| panic!("{s}: {e}"));
        let second = acquire(all, shutdown).unwrap_or_else(|(s, e)| panic!("{s}: {e}"));
        assert_eq!(first, SignalSet::from(Signal::User2));
        assert_eq!(second, first);
        assert_eq!(installs(Signal::User2), 2);
        assert_eq!(installs(Signal::Pipe), 0);
        assert_eq!(installs(Signal::User1), 0); // notify-only
        release(first);
        assert_eq!(installs(Signal::User2), 1);
        release(second);
        assert_eq!(installs(Signal::User2), 0);
        release(second); // saturates
        assert_eq!(installs(Signal::User2), 0);
    }
}
