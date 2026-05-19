//! Hook priority ordering and the graceful timeout budget.
//!
//! Shows that hooks run in descending priority order, that
//! insertion order is preserved within a priority, and that the
//! graceful timeout bounds the total work done by `run_hooks`.
//!
//! Run with:
//!
//! ```text
//! cargo run --example priority_hooks
//! ```

use std::time::Duration;

use signal_mod::{hook_from_fn, Coordinator, ShutdownReason};

fn main() {
    let coord = Coordinator::builder()
        .graceful_timeout(Duration::from_secs(2))
        // High-priority hooks run first.
        .hook(hook_from_fn("close-listener", 1000, |_| {
            println!("close-listener (1000): stop accepting new requests");
        }))
        .hook(hook_from_fn("drain-queues", 500, |_| {
            println!("drain-queues (500): finish in-flight work");
        }))
        // Equal priority: insertion order preserved.
        .hook(hook_from_fn("flush-cache", 100, |_| {
            println!("flush-cache (100): persist hot keys");
        }))
        .hook(hook_from_fn("flush-logs", 100, |_| {
            println!("flush-logs (100): rotate and fsync log files");
        }))
        // Lowest priority runs last.
        .hook(hook_from_fn("release-resources", 0, |_| {
            println!("release-resources (0): close handles, free pools");
        }))
        .build();

    println!("triggering programmatic shutdown");
    coord.trigger().trigger(ShutdownReason::Requested);
    let ran = coord.run_hooks(ShutdownReason::Requested);
    let stats = coord.statistics();
    println!(
        "ran {ran} of {} registered hook(s); cumulative completions = {}",
        stats.hooks_registered, stats.hooks_completed,
    );
}
