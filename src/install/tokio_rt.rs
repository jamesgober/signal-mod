//! Tokio back-end for [`Coordinator::install`](crate::Coordinator::install).
//!
//! Spawns one background task per configured signal. Each task drives
//! the corresponding `tokio::signal` stream and calls
//! [`ShutdownTrigger::trigger`](crate::ShutdownTrigger::trigger) when
//! a signal is delivered. Tasks live until the runtime shuts them down.

use crate::coord::Coordinator;
use crate::error::{Error, Result};
use crate::reason::ShutdownReason;
use crate::signal::{Signal, SignalSet};
use crate::token::ShutdownTrigger;

/// Install Tokio-backed signal listeners for the configured set.
///
/// Must be called from inside a Tokio runtime context (`tokio::main`,
/// `tokio::test`, or an explicit `Runtime::handle().enter()` block);
/// otherwise `tokio::spawn` panics.
pub(crate) fn install(coord: &Coordinator) -> Result<()> {
    let trigger = coord.trigger();
    let set = coord.signals();
    install_inner(set, trigger)
}

#[cfg(unix)]
fn install_inner(set: SignalSet, trigger: ShutdownTrigger) -> Result<()> {
    use tokio::signal::unix::{signal, SignalKind};

    fn reg(sig: Signal, kind: SignalKind, set: SignalSet, trigger: &ShutdownTrigger) -> Result<()> {
        if !set.contains(sig) {
            return Ok(());
        }
        let mut stream = signal(kind).map_err(|e| Error::SignalRegistration {
            signal: sig,
            source: e,
        })?;
        let t = trigger.clone();
        tokio::spawn(async move {
            while stream.recv().await.is_some() {
                let _ = t.trigger(ShutdownReason::Signal(sig));
            }
        });
        Ok(())
    }

    reg(Signal::Terminate, SignalKind::terminate(), set, &trigger)?;
    reg(Signal::Interrupt, SignalKind::interrupt(), set, &trigger)?;
    reg(Signal::Quit, SignalKind::quit(), set, &trigger)?;
    reg(Signal::Hangup, SignalKind::hangup(), set, &trigger)?;
    reg(Signal::Pipe, SignalKind::pipe(), set, &trigger)?;
    reg(Signal::User1, SignalKind::user_defined1(), set, &trigger)?;
    reg(Signal::User2, SignalKind::user_defined2(), set, &trigger)?;
    Ok(())
}

#[cfg(windows)]
fn install_inner(set: SignalSet, trigger: ShutdownTrigger) -> Result<()> {
    use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_shutdown};

    macro_rules! spawn_listener {
        ($sig:expr, $factory:expr) => {{
            if set.contains($sig) {
                let mut s = $factory().map_err(|e| Error::SignalRegistration {
                    signal: $sig,
                    source: e,
                })?;
                let t = trigger.clone();
                tokio::spawn(async move {
                    while s.recv().await.is_some() {
                        let _ = t.trigger(ShutdownReason::Signal($sig));
                    }
                });
            }
        }};
    }

    spawn_listener!(Signal::Interrupt, ctrl_c);
    spawn_listener!(Signal::Quit, ctrl_break);
    spawn_listener!(Signal::Terminate, ctrl_close);
    spawn_listener!(Signal::Hangup, ctrl_shutdown);
    // Pipe / User1 / User2 are inert on Windows; skip silently.
    let _ = trigger;
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn install_inner(_set: SignalSet, _trigger: ShutdownTrigger) -> Result<()> {
    // Unknown platform. We have no signal back-end; treat install as
    // a successful no-op so the rest of the API stays usable.
    Ok(())
}
