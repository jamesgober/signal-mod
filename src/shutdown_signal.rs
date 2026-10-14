//! [`shutdown_signal`]: the one-line integration for servers.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};

use crate::coord::Coordinator;
use crate::error::Result;
use crate::reason::ShutdownReason;
use crate::token::{ShutdownFuture, ShutdownToken};

/// Install handlers for [`SignalSet::graceful`](crate::SignalSet::graceful)
/// (SIGTERM, SIGINT, SIGHUP / Ctrl+C, console close) and return a future
/// that resolves when one arrives.
///
/// Made for `with_graceful_shutdown`-style APIs (axum, hyper, tonic,
/// warp), which take a `Future<Output = ()> + Send + 'static`. The
/// handlers live inside the returned [`ShutdownSignal`]: once the server
/// drops it after the first signal, signals that were at their default
/// action get it back, so a second Ctrl+C terminates the process even if
/// draining hangs (see [`Coordinator::install`] for the exact rules).
///
/// For hooks, subscriptions, or escalation, build a [`Coordinator`]
/// instead and pass `coord.token().wait()` to the server.
///
/// # Errors
///
/// As [`Coordinator::install`]: with only the `tokio` feature it must be
/// called inside a Tokio runtime.
///
/// # Examples
///
/// ```no_run
/// # async fn app() -> std::io::Result<()> {
/// let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
/// let app = axum::Router::new();
/// axum::serve(listener, app)
///     .with_graceful_shutdown(signal_mod::shutdown_signal().map_err(std::io::Error::other)?)
///     .await
/// # }
/// ```
pub fn shutdown_signal() -> Result<ShutdownSignal> {
    let coordinator = Coordinator::builder().build();
    coordinator.install()?;
    let wait = coordinator.token().wait();
    Ok(ShutdownSignal { coordinator, wait })
}

/// Future returned by [`shutdown_signal`]. Resolves once a graceful
/// shutdown signal is delivered; owns the signal handlers until dropped.
#[must_use = "the handlers are removed when this is dropped"]
pub struct ShutdownSignal {
    coordinator: Coordinator,
    wait: ShutdownFuture,
}

impl ShutdownSignal {
    /// The signal (as a reason) that resolved the future, if one has
    /// arrived.
    #[must_use]
    pub fn reason(&self) -> Option<ShutdownReason> {
        self.coordinator.token().reason()
    }

    /// An observer handle for the same shutdown, for other tasks.
    #[must_use]
    pub fn token(&self) -> ShutdownToken {
        self.coordinator.token()
    }
}

impl fmt::Debug for ShutdownSignal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShutdownSignal")
            .field("reason", &self.reason())
            .finish_non_exhaustive()
    }
}

impl Future for ShutdownSignal {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Pin::new(&mut self.get_mut().wait).poll(cx)
    }
}
