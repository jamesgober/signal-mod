//! Minimum-viable graceful shutdown example.
//!
//! Run with:
//!
//! ```text
//! cargo run --example graceful_shutdown
//! ```
//!
//! Then press Ctrl+C. The coordinator will observe the signal, run
//! the registered hook, and the program exits cleanly.

#[cfg(feature = "tokio")]
use std::time::Duration;

#[cfg(feature = "tokio")]
use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, SignalSet};

#[cfg(feature = "tokio")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .graceful_timeout(Duration::from_secs(2))
        .hook(hook_from_fn("flush-logs", 100, |reason: ShutdownReason| {
            println!("flush-logs hook ran: reason={reason}");
        }))
        .hook(hook_from_fn(
            "close-connections",
            50,
            |reason: ShutdownReason| {
                println!("close-connections hook ran: reason={reason}");
            },
        ))
        .build();

    coord.install()?;

    println!("running; press Ctrl+C to exit");

    let token = coord.token();
    token.wait().await;

    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    let ran = coord.run_hooks(reason);

    println!("ran {ran} hook(s); exiting");
    Ok(())
}

#[cfg(not(feature = "tokio"))]
fn main() {
    eprintln!("This example requires the `tokio` feature.");
}
