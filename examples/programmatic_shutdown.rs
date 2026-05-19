//! Programmatic shutdown without OS signals.
//!
//! Demonstrates calling `ShutdownTrigger::trigger` directly. This is
//! the pattern an HTTP `/shutdown` admin endpoint, a supervisory
//! parent process, or a fatal-error branch would use.
//!
//! Run with:
//!
//! ```text
//! cargo run --example programmatic_shutdown
//! ```

#[cfg(feature = "tokio")]
use std::time::Duration;

#[cfg(feature = "tokio")]
use signal_mod::{hook_from_fn, Coordinator, ShutdownReason};

#[cfg(feature = "tokio")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .graceful_timeout(Duration::from_millis(500))
        .hook(hook_from_fn("flush", 0, |reason: ShutdownReason| {
            println!("flush hook fired with reason={reason}");
        }))
        .build();

    let trigger = coord.trigger();
    let token = coord.token();

    // Simulate a supervisor task: after a brief delay, trip shutdown
    // from a different task.
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let initiated = trigger.trigger(ShutdownReason::Requested);
        println!("supervisor: trigger returned {initiated}");
    });

    println!("main: waiting for programmatic shutdown");
    token.wait().await;
    println!("main: shutdown observed; running hooks");

    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    let ran = coord.run_hooks(reason);
    println!("main: ran {ran} hook(s); exiting cleanly");
    Ok(())
}

#[cfg(not(feature = "tokio"))]
fn main() {
    eprintln!("This example requires the `tokio` feature.");
}
