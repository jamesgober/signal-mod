//! Synchronous program, no async runtime.
//!
//! The `agnostic` back-end needs no runtime, so a plain `fn main` can
//! install handlers and block on the token. (The 1.x feature name
//! `ctrlc-fallback` is an alias for it.)
//!
//! Run with:
//!
//! ```text
//! cargo run --example sync_blocking --no-default-features --features std,agnostic
//! ```

#[cfg(feature = "agnostic")]
fn main() -> signal_mod::Result<()> {
    use std::time::Duration;

    use signal_mod::{Coordinator, ShutdownReason, SignalSet, hook_from_fn};

    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .graceful_timeout(Duration::from_secs(2))
        .hook(hook_from_fn("save-state", 100, |reason| {
            println!("save-state hook ran (reason={reason})");
        }))
        .build();

    coord.install()?;
    println!("running on the main thread; press Ctrl+C to exit");

    let token = coord.token();
    // Parks the thread on a condition variable: no runtime, no polling.
    token.wait_blocking();

    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    let ran = coord.run_hooks(reason);
    println!("ran {ran} hook(s); exiting");
    std::process::exit(reason.exit_code());
}

#[cfg(not(feature = "agnostic"))]
fn main() {
    eprintln!("This example needs: --no-default-features --features std,agnostic");
}
