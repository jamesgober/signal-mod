//! Hook ordering, the enforced graceful budget, and the force phase.
//!
//! A hung hook is abandoned when the graceful budget runs out; the hooks
//! after it still run, in the force phase, with `ShutdownReason::Forced`.
//!
//! Run with:
//!
//! ```text
//! cargo run --example priority_hooks
//! ```

use std::time::Duration;

use signal_mod::{Coordinator, ShutdownReason, hook_from_fn};

fn main() {
    let coord = Coordinator::builder()
        .graceful_timeout(Duration::from_millis(300))
        .force_timeout(Duration::from_secs(2))
        .hook(hook_from_fn("close-listener", 1000, |r| {
            println!("close-listener (1000): stop accepting requests [{r}]");
        }))
        .hook(hook_from_fn("drain-queues", 500, |r| {
            println!("drain-queues (500): stuck on a dead peer [{r}]");
            std::thread::sleep(Duration::from_secs(30));
        }))
        // Equal priority: registration order is kept.
        .hook(hook_from_fn("flush-cache", 100, |r| {
            println!("flush-cache (100): persist hot keys [{r}]");
        }))
        .hook(hook_from_fn("flush-logs", 100, |r| {
            println!("flush-logs (100): fsync [{r}]");
        }))
        .build();

    coord.trigger().trigger(ShutdownReason::Requested);
    let report = coord.run_hooks_with_report(ShutdownReason::Requested);
    println!("\n{report}");
    for hook in report.outcomes() {
        println!(
            "  {:<15} {:<10} phase={:?} took={:?}",
            hook.name, hook.status, hook.phase, hook.duration
        );
    }
    // The abandoned `drain-queues` thread does not keep the process alive.
    std::process::exit(0);
}
