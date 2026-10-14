//! Programmatic shutdown without OS signals.
//!
//! `ShutdownTrigger::trigger` is what an HTTP `/shutdown` admin
//! endpoint, a supervisor, or a fatal-error branch calls. No signal
//! back-end is needed for this path.
//!
//! Run with:
//!
//! ```text
//! cargo run --example programmatic_shutdown
//! ```

use std::time::Duration;

use signal_mod::{Coordinator, ShutdownReason, hook_from_fn};

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let coord = Coordinator::builder()
        .graceful_timeout(Duration::from_millis(500))
        .hook(hook_from_fn("flush", 0, |reason| {
            println!("flush hook ran with reason={reason}");
        }))
        .build();

    let trigger = coord.trigger();
    let token = coord.token();

    // A supervisor task asks for shutdown from somewhere else.
    let supervisor = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let first = trigger.trigger(ShutdownReason::Requested);
        println!("supervisor: trigger returned {first}");
    });

    println!("main: waiting for programmatic shutdown");
    token.wait().await;
    println!("main: shutdown observed; running hooks");

    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    let report = coord.run_hooks_async(reason).await;
    println!("main: {report}");
    supervisor.await.ok();
}
