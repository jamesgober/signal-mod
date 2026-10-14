//! The canonical service shape: install, wait, run the hooks, exit with
//! the conventional status.
//!
//! Run with:
//!
//! ```text
//! cargo run --example graceful_shutdown
//! ```
//!
//! Then press Ctrl+C. The coordinator observes the signal, runs the
//! hooks under the enforced budget, prints the report, and exits with
//! status 130 (128 + SIGINT). Press Ctrl+C twice to skip the hooks and
//! exit immediately.

use std::time::Duration;

use signal_mod::{Coordinator, ShutdownReason, SignalSet, hook_from_fn};

#[tokio::main]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .graceful_timeout(Duration::from_secs(2))
        .force_timeout(Duration::from_secs(4))
        .force_exit_on_second_signal(true)
        .hook(hook_from_fn("close-listener", 100, |reason| {
            println!("close-listener: stop accepting connections ({reason})");
        }))
        .hook(
            hook_from_fn("flush-logs", 50, |reason| {
                println!("flush-logs: fsync ({reason})");
            })
            .with_timeout(Duration::from_millis(500)),
        )
        .build();

    coord.install()?;
    println!("running; press Ctrl+C to exit");

    let token = coord.token();
    token.wait().await;

    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    let report = coord.run_hooks_async(reason).await;
    println!("{report}");
    for hook in report.outcomes() {
        println!(
            "  {:<16} {:<10} {:?}",
            hook.name, hook.status, hook.duration
        );
    }
    std::process::exit(reason.exit_code());
}
