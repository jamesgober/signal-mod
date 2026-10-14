//! Graceful shutdown of an axum HTTP server.
//!
//! The server stops accepting connections when the token resolves and
//! drains in-flight requests; the hooks then run with their own budget.
//! For servers that need nothing else, `signal_mod::shutdown_signal()`
//! is a drop-in future for `with_graceful_shutdown` (see `minimal`).
//!
//! Run with:
//!
//! ```text
//! cargo run --example axum_server
//! ```
//!
//! then `curl http://127.0.0.1:3000/` and press Ctrl+C.

use axum::{Router, routing::get};
use signal_mod::{Coordinator, ShutdownReason, hook_from_fn};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let coord = Coordinator::builder()
        .force_exit_on_second_signal(true)
        .hook(hook_from_fn("flush-metrics", 0, |_| {
            println!("metrics flushed");
        }))
        .build();
    coord.install().map_err(std::io::Error::other)?;

    let app = Router::new().route("/", get(|| async { "hello\n" }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    println!("listening on http://127.0.0.1:3000; Ctrl+C to stop");

    axum::serve(listener, app)
        .with_graceful_shutdown(coord.token().wait())
        .await?;

    let reason = coord.token().reason().unwrap_or(ShutdownReason::Requested);
    let report = coord.run_hooks_async(reason).await;
    println!("{report}");
    Ok(())
}

/// The one-liner form.
#[allow(dead_code)] // shown for reference; `main` uses the full form
async fn minimal(app: Router) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let shutdown = signal_mod::shutdown_signal().map_err(std::io::Error::other)?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}
