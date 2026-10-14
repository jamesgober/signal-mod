//! Tokio back-end.
//!
//! - **Unix:** one `tokio::signal` listener task per signal. Registration
//!   is all-or-nothing: every stream is created before any task is
//!   spawned, so a failure part-way leaves nothing behind (1.0.0 left the
//!   earlier listeners running). Uninstalling aborts the tasks; because
//!   Tokio never removes its OS handler, [`restore`](super::restore)
//!   brings back the default action where that was the prior one.
//! - **Windows:** one `tokio::signal::windows` listener task per console
//!   event. Tokio's console handler broadcasts each event to every Tokio
//!   listener in the process, so coordinators and the application's own
//!   `tokio::signal` listeners all see it. Uninstalling drops the
//!   streams; once nothing listens, Tokio's handler declines the event
//!   and the OS default (terminate) applies.

use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::task::JoinHandle;

use crate::dispatch::Dispatcher;
use crate::error::Result;
use crate::signal::SignalSet;

/// Running listener tasks.
#[derive(Debug)]
pub(crate) struct Registration {
    tasks: Vec<JoinHandle<()>>,
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
        for task in &self.tasks {
            task.abort();
        }
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
    handle: &Handle,
    dispatcher: &Arc<Dispatcher>,
    set: SignalSet,
) -> Result<Registration> {
    let covered = set.available_on_current_platform();
    let tasks = super::windows_events::streams(covered)?
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
    sm_debug!(signals = ?covered, "tokio console-event listeners installed");
    Ok(Registration { tasks, covered })
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
