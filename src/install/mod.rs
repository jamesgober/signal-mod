//! Signal back-ends behind [`Coordinator::install`](crate::Coordinator::install).
//!
//! | Feature    | Back-end                                                    |
//! | ---------- | ----------------------------------------------------------- |
//! | `tokio`    | [`tokio_rt`]: one `tokio::signal` listener task per signal. |
//! | `agnostic` | [`agnostic`]: a listener thread (signal-hook on Unix, Tokio's console-event streams on Windows when `tokio` is enabled), or signal-mod's own console control handler on Windows without `tokio`. |
//!
//! Selection happens at install time: with `tokio` enabled and a Tokio
//! runtime current, the Tokio back-end is used; otherwise the agnostic
//! back-end if enabled; otherwise an error explains what is missing.
//! (1.x chose at compile time and let `tokio` silently win.)
//!
//! Every back-end funnels deliveries into
//! [`Dispatcher::deliver`](crate::dispatch::Dispatcher::deliver) and
//! returns a [`Registration`] that removes the listeners again.

use std::sync::Arc;

use crate::dispatch::Dispatcher;
use crate::error::Result;
use crate::signal::SignalSet;

#[cfg(all(unix, any(feature = "tokio", feature = "agnostic")))]
pub(crate) mod restore;

#[cfg(all(unix, any(feature = "tokio", feature = "agnostic")))]
pub(crate) mod disposition;

#[cfg(all(windows, feature = "agnostic", not(feature = "tokio")))]
pub(crate) mod console;

#[cfg(all(windows, feature = "tokio"))]
pub(crate) mod windows_events;

#[cfg(feature = "tokio")]
pub(crate) mod tokio_rt;

#[cfg(feature = "agnostic")]
pub(crate) mod agnostic;

/// Which back-end an installation uses.
///
/// # Examples
///
/// ```
/// use signal_mod::{Backend, Coordinator, SignalSet};
///
/// let coord = Coordinator::builder().signals(SignalSet::empty()).build();
/// assert_eq!(coord.backend(), None);
/// if coord.install().is_ok() {
///     // Outside a Tokio runtime only the agnostic back-end can install.
///     assert_eq!(coord.backend(), Some(Backend::Agnostic));
/// }
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Backend {
    /// `tokio::signal` listener tasks on the current Tokio runtime (Unix
    /// signal streams, or Windows console-event streams).
    Tokio,
    /// The `agnostic` back-end: a listener thread (signal-hook on Unix,
    /// Tokio's console-event streams on Windows when the `tokio` feature
    /// is enabled), or signal-mod's own console control handler on
    /// Windows without it.
    Agnostic,
}

/// Live listeners for one coordinator.
#[derive(Debug)]
pub(crate) enum Registration {
    #[cfg(feature = "tokio")]
    Tokio(tokio_rt::Registration),
    #[cfg(feature = "agnostic")]
    Agnostic(agnostic::Registration),
}

// With no back-end feature the enum has no variants and every `match`
// below is empty, which is exactly right for an uninhabited type.
impl Registration {
    pub(crate) fn backend(&self) -> Backend {
        match *self {
            #[cfg(feature = "tokio")]
            Self::Tokio(_) => Backend::Tokio,
            #[cfg(feature = "agnostic")]
            Self::Agnostic(_) => Backend::Agnostic,
        }
    }

    /// Signals the listeners actually cover.
    pub(crate) fn covered(&self) -> SignalSet {
        match *self {
            #[cfg(feature = "tokio")]
            Self::Tokio(ref r) => r.covered(),
            #[cfg(feature = "agnostic")]
            Self::Agnostic(ref r) => r.covered(),
        }
    }

    /// Stop the listeners and give the covered signals back to the OS
    /// default once no other coordinator handles them.
    pub(crate) fn uninstall(self) {
        match self {
            #[cfg(feature = "tokio")]
            Self::Tokio(r) => r.uninstall(),
            #[cfg(feature = "agnostic")]
            Self::Agnostic(r) => r.uninstall(),
        }
    }
}

/// Install listeners for `set`, routing deliveries to `dispatcher`.
#[allow(clippy::needless_pass_by_value, clippy::unnecessary_wraps)] // shape shared by every feature combination
pub(crate) fn install(dispatcher: &Arc<Dispatcher>, set: SignalSet) -> Result<Registration> {
    #[cfg(feature = "tokio")]
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        return tokio_rt::install(&handle, dispatcher, set).map(Registration::Tokio);
    }

    #[cfg(feature = "agnostic")]
    {
        agnostic::install(dispatcher, set).map(Registration::Agnostic)
    }

    #[cfg(all(feature = "tokio", not(feature = "agnostic")))]
    {
        let _ = (dispatcher, set); // only the agnostic back-end could use them here
        Err(crate::error::Error::OutsideTokioRuntime)
    }

    #[cfg(not(any(feature = "tokio", feature = "agnostic")))]
    {
        let _ = (dispatcher, set); // no back-end compiled in
        Err(crate::error::Error::NoRuntime)
    }
}

/// Map a registration failure for `sig` into the crate error.
#[cfg(any(
    all(unix, any(feature = "tokio", feature = "agnostic")),
    all(windows, feature = "tokio")
))]
pub(crate) fn registration_error(
    sig: crate::signal::Signal,
    source: std::io::Error,
) -> crate::error::Error {
    crate::error::Error::SignalRegistration {
        signal: sig,
        source,
    }
}
