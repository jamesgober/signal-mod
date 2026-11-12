//! Reading a signal's current OS disposition (Unix).
//!
//! The one place outside the Windows console handler that needs FFI:
//! `sigaction` with a null new action only reads the disposition. Used
//! by [`restore`](super::restore) to decide whether signal-mod may
//! restore the default action later.

// One read-only `sigaction` call; justified at the call site.
#![allow(unsafe_code)]

use std::io;
use std::mem::MaybeUninit;

/// A signal's disposition, as `sigaction` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// `SIG_DFL`: the default action.
    Default,
    /// `SIG_IGN`: ignored (for example SIGHUP under `nohup`).
    Ignored,
    /// A handler installed by someone.
    Handled,
}

/// `number`'s current disposition.
///
/// # Errors
///
/// The OS error if `sigaction` rejects the signal number.
pub(crate) fn disposition(number: i32) -> io::Result<Disposition> {
    let mut old = MaybeUninit::<libc::sigaction>::zeroed();
    // SAFETY: a null `act` makes `sigaction` a pure query: nothing is
    // installed or changed. `old.as_mut_ptr()` points to writable,
    // properly aligned storage for one `struct sigaction` that outlives
    // the call.
    let rc = unsafe { libc::sigaction(number, core::ptr::null(), old.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the storage was zero-initialised (a valid bit pattern for
    // this plain C struct of integers, masks and a handler address) and
    // `sigaction` returned success, so it now holds the disposition.
    let old = unsafe { old.assume_init() };
    Ok(if old.sa_sigaction == libc::SIG_DFL {
        Disposition::Default
    } else if old.sa_sigaction == libc::SIG_IGN {
        Disposition::Ignored
    } else {
        Disposition::Handled
    })
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn test_disposition_reports_untouched_signal_as_default() {
        // Nothing in the unit-test process handles SIGXCPU.
        assert!(matches!(
            disposition(libc::SIGXCPU),
            Ok(Disposition::Default)
        ));
    }

    #[test]
    fn test_disposition_reports_rust_runtime_sigpipe_as_ignored() {
        // The Rust runtime sets SIGPIPE to SIG_IGN before `main`.
        assert!(matches!(
            disposition(libc::SIGPIPE),
            Ok(Disposition::Ignored)
        ));
    }

    #[test]
    fn test_disposition_rejects_invalid_number() {
        assert!(disposition(-1).is_err());
    }
}
