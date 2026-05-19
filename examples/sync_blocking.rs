//! Synchronous (no async runtime) blocking-wait pattern.
//!
//! Useful for CLI tools and short-lived utilities that do not need a
//! Tokio or async-std runtime. Build with the `ctrlc-fallback`
//! feature so `Coordinator::install` can register the Ctrl+C handler
//! without an async runtime.
//!
//! Run with:
//!
//! ```text
//! cargo run --example sync_blocking --no-default-features --features "std ctrlc-fallback"
//! ```

#[cfg(all(
    feature = "ctrlc-fallback",
    not(feature = "tokio"),
    not(feature = "async-std")
))]
use std::time::Duration;

#[cfg(all(
    feature = "ctrlc-fallback",
    not(feature = "tokio"),
    not(feature = "async-std")
))]
use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, SignalSet};

#[cfg(all(
    feature = "ctrlc-fallback",
    not(feature = "tokio"),
    not(feature = "async-std")
))]
fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .graceful_timeout(Duration::from_secs(2))
        .hook(hook_from_fn("save-state", 100, |reason| {
            println!("save-state hook fired (reason={reason})");
        }))
        .build();

    coord.install()?;

    println!("running on the main thread; press Ctrl+C to exit");

    let token = coord.token();
    // The blocking wait parks the current thread on a Condvar; no
    // runtime, no polling, no spinning.
    token.wait_blocking();

    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    let ran = coord.run_hooks(reason);
    println!("ran {ran} hook(s); exiting");
    Ok(())
}

#[cfg(any(
    feature = "tokio",
    feature = "async-std",
    not(feature = "ctrlc-fallback")
))]
fn main() {
    eprintln!("This example requires --no-default-features --features \"std ctrlc-fallback\".");
}
