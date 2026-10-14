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

/// `true` if `number`'s disposition is currently `SIG_DFL`.
///
/// # Errors
///
/// The OS error if `sigaction` rejects the signal number.
pub(crate) fn is_default(number: i32) -> io::Result<bool> {
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
    Ok(old.sa_sigaction == libc::SIG_DFL)
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn test_is_default_reports_untouched_signal_as_default() {
        // Nothing in the unit-test process handles SIGXCPU.
        assert!(matches!(is_default(libc::SIGXCPU), Ok(true)));
    }

    #[test]
    fn test_is_default_rejects_invalid_number() {
        assert!(is_default(-1).is_err());
    }
}
