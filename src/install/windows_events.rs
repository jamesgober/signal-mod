//! Tokio's Windows console-event streams, shared by both back-ends when
//! the `tokio` feature is enabled.
//!
//! Tokio registers one console control handler per process and
//! broadcasts every event to all of its streams. Going through it (rather
//! than registering a second handler) keeps signal-mod from hiding events
//! from the application's own `tokio::signal` listeners, and lets
//! coordinators of both back-ends all receive every event. The streams
//! need no runtime: the Tokio back-end drives them from tasks, the
//! agnostic back-end polls them from its listener thread. Tokio's handler
//! also keeps the OS handler thread parked for `CTRL_CLOSE` /
//! `CTRL_LOGOFF` / `CTRL_SHUTDOWN`, which gives the program the OS grace
//! period (about 5 seconds) to shut down.

use core::task::{Context, Poll};

use tokio::signal::windows::{CtrlBreak, CtrlC, CtrlClose, CtrlLogoff, CtrlShutdown};

use crate::error::Result;
use crate::signal::{Signal, SignalSet};

/// One of Tokio's console-event streams.
pub(crate) enum Stream {
    C(CtrlC),
    Break(CtrlBreak),
    Close(CtrlClose),
    Logoff(CtrlLogoff),
    Shutdown(CtrlShutdown),
}

impl Stream {
    pub(crate) fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<()>> {
        match self {
            Self::C(s) => s.poll_recv(cx),
            Self::Break(s) => s.poll_recv(cx),
            Self::Close(s) => s.poll_recv(cx),
            Self::Logoff(s) => s.poll_recv(cx),
            Self::Shutdown(s) => s.poll_recv(cx),
        }
    }

    pub(crate) async fn recv(&mut self) -> Option<()> {
        core::future::poll_fn(|cx| self.poll_recv(cx)).await
    }
}

/// Create a stream for every console event that maps to a signal in
/// `covered`. All-or-nothing: on error nothing stays registered.
pub(crate) fn streams(covered: SignalSet) -> Result<Vec<(Signal, Stream)>> {
    use tokio::signal::windows;

    let reg = |sig: Signal| move |e| super::registration_error(sig, e);
    let mut streams = Vec::new();
    if covered.contains(Signal::Interrupt) {
        streams.push((
            Signal::Interrupt,
            Stream::C(windows::ctrl_c().map_err(reg(Signal::Interrupt))?),
        ));
    }
    if covered.contains(Signal::Quit) {
        streams.push((
            Signal::Quit,
            Stream::Break(windows::ctrl_break().map_err(reg(Signal::Quit))?),
        ));
    }
    if covered.contains(Signal::Terminate) {
        streams.push((
            Signal::Terminate,
            Stream::Close(windows::ctrl_close().map_err(reg(Signal::Terminate))?),
        ));
    }
    if covered.contains(Signal::Hangup) {
        streams.push((
            Signal::Hangup,
            Stream::Logoff(windows::ctrl_logoff().map_err(reg(Signal::Hangup))?),
        ));
        streams.push((
            Signal::Hangup,
            Stream::Shutdown(windows::ctrl_shutdown().map_err(reg(Signal::Hangup))?),
        ));
    }
    Ok(streams)
}
