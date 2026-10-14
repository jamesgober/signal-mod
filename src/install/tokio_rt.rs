//! Tokio back-end: one `tokio::signal` listener task per signal.
//!
//! Registration is all-or-nothing: every stream is created before any
//! task is spawned, so a failure part-way leaves nothing behind (1.0.0
//! left the earlier listeners running).
//!
//! Uninstalling aborts the tasks, which drops the streams. On Windows,
//! Tokio's console handler then reports "not handled" and the OS default
//! (terminate) applies again. On Unix, Tokio never removes its handler,
//! so [`restore`](super::restore) arms the default action instead.

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
        super::restore::release(self.covered);
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
        let stream = created.map_err(|e| super::registration_error(sig, e))?;
        streams.push((sig, stream));
    }

    super::restore::acquire(set).map_err(|(sig, e)| super::registration_error(sig, e))?;

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
    })
}

#[cfg(windows)]
pub(crate) fn install(
    handle: &Handle,
    dispatcher: &Arc<Dispatcher>,
    set: SignalSet,
) -> Result<Registration> {
    use crate::signal::Signal;
    use tokio::signal::windows;

    // One boxed future factory per console event; created eagerly so a
    // failure leaves nothing spawned.
    type Listener = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;
    let mut listeners: Vec<Listener> = Vec::new();

    macro_rules! listen {
        ($sig:expr, $factory:path) => {{
            let mut stream = $factory().map_err(|e| super::registration_error($sig, e))?;
            let dispatcher = Arc::clone(dispatcher);
            listeners.push(Box::pin(async move {
                while stream.recv().await.is_some() {
                    dispatcher.deliver($sig);
                }
            }));
        }};
    }

    let covered = set.available_on_current_platform();
    if covered.contains(Signal::Interrupt) {
        listen!(Signal::Interrupt, windows::ctrl_c);
    }
    if covered.contains(Signal::Quit) {
        listen!(Signal::Quit, windows::ctrl_break);
    }
    if covered.contains(Signal::Terminate) {
        listen!(Signal::Terminate, windows::ctrl_close);
    }
    if covered.contains(Signal::Hangup) {
        listen!(Signal::Hangup, windows::ctrl_logoff);
        listen!(Signal::Hangup, windows::ctrl_shutdown);
    }

    let tasks = listeners.into_iter().map(|l| handle.spawn(l)).collect();
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
