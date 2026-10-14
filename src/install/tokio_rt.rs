//! Tokio back-end.
//!
//! - **Unix:** one `tokio::signal` listener task per signal. Registration
//!   is all-or-nothing: every stream is created before any task is
//!   spawned, so a failure part-way leaves nothing behind (1.0.0 left the
//!   earlier listeners running). Uninstalling aborts the tasks; because
//!   Tokio never removes its OS handler, [`restore`](super::restore)
//!   brings back the default action where that was the prior one.
//! - **Windows:** the shared console control handler in
//!   [`console`](super::console). A Tokio-installed and an
//!   agnostic-installed coordinator in one process therefore both see
//!   every event (with separate handlers, Windows would stop at the
//!   first one that claims the event).

use std::sync::Arc;

use tokio::runtime::Handle;
#[cfg(not(windows))]
use tokio::task::JoinHandle;

use crate::dispatch::Dispatcher;
use crate::error::Result;
use crate::signal::SignalSet;

/// Running listener tasks.
#[derive(Debug)]
pub(crate) struct Registration {
    #[cfg(not(windows))]
    tasks: Vec<JoinHandle<()>>,
    #[cfg(windows)]
    console: u64,
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
        #[cfg(not(windows))]
        for task in &self.tasks {
            task.abort();
        }
        #[cfg(windows)]
        super::console::unregister(self.console);
        #[cfg(unix)]
        super::restore::release(self.restored);
        sm_debug!(signals = ?self.covered, "tokio signal listeners removed");
    }
}

#[cfg(unix)]
pub(crate) fn install(
    handle: &Handle,
    dispatcher: &Arc<Dispatcher>,
    set: SignalSet,
) -> Result<Registration> {
    use tokio::signal::unix::{SignalKind, signal};

    let _context = handle.enter();
    // Read the prior dispositions before Tokio registers anything.
    let restored = super::restore::acquire(set, dispatcher.shutdown_set())
        .map_err(|(sig, e)| super::registration_error(sig, e))?;
    let mut streams = Vec::with_capacity(set.len());
    for sig in set {
        let Some(number) = sig.unix_number() else {
            continue;
        };
        let kind = SignalKind::from_raw(number);
        // `signal` panics instead of erroring when the runtime was built
        // without its I/O driver; turn that into a registration error.
        let created = std::panic::catch_unwind(move || signal(kind)).unwrap_or_else(|_| {
            Err(std::io::Error::other(
                "the Tokio runtime has no signal driver; build it with enable_io() or enable_all()",
            ))
        });
        match created {
            Ok(stream) => streams.push((sig, stream)),
            Err(e) => {
                super::restore::release(restored);
                return Err(super::registration_error(sig, e));
            }
        }
    }

    let tasks = streams
        .into_iter()
        .map(|(sig, mut stream)| {
            let dispatcher = Arc::clone(dispatcher);
            handle.spawn(async move {
                while stream.recv().await.is_some() {
                    dispatcher.deliver(sig);
                }
            })
        })
        .collect();
    sm_debug!(signals = ?set, "tokio signal listeners installed");
    Ok(Registration {
        tasks,
        covered: set,
        restored,
    })
}

#[cfg(windows)]
pub(crate) fn install(
    _handle: &Handle,
    dispatcher: &Arc<Dispatcher>,
    set: SignalSet,
) -> Result<Registration> {
    let covered = set.available_on_current_platform();
    // One OS call covers every event, so a failure is not attributable to
    // a single signal.
    let console = super::console::register(dispatcher, covered)
        .map_err(crate::error::Error::ListenerThread)?;
    sm_debug!(signals = ?covered, "console control handler registered (tokio back-end)");
    Ok(Registration { console, covered })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn install(
    _handle: &Handle,
    _dispatcher: &Arc<Dispatcher>,
    _set: SignalSet,
) -> Result<Registration> {
    // No signal source on this platform: an explicit, empty
    // installation (`Coordinator::installed_signals` reports it).
    sm_warn!("no signal source on this platform; install() covers no signals");
    Ok(Registration {
        tasks: Vec::new(),
        covered: SignalSet::empty(),
    })
}
