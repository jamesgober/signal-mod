//! Implementing `ShutdownHook` directly.
//!
//! `hook_from_fn` covers one-off closures. Implementing the trait lets a
//! hook own state (a connection pool, a file handle), set its own
//! timeout, and be reused across services.
//!
//! Run with:
//!
//! ```text
//! cargo run --example custom_hook_type
//! ```

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use signal_mod::{Coordinator, ShutdownHook, ShutdownReason};

/// Counts its invocations so the run-once guarantee is visible.
struct CountedHook {
    label: String,
    priority: i32,
    counter: Arc<AtomicUsize>,
}

impl ShutdownHook for CountedHook {
    fn name(&self) -> &str {
        &self.label
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    fn timeout(&self) -> Option<Duration> {
        Some(Duration::from_secs(1))
    }

    fn run(&self, reason: ShutdownReason) {
        let previous = self.counter.fetch_add(1, Ordering::Relaxed);
        println!(
            "[{}] ran (priority={}, earlier runs={previous}, reason={reason})",
            self.label, self.priority,
        );
    }
}

fn main() {
    let counter = Arc::new(AtomicUsize::new(0));
    let coord = Coordinator::builder()
        .hook(CountedHook {
            label: "primary".into(),
            priority: 200,
            counter: Arc::clone(&counter),
        })
        .hook(CountedHook {
            label: "secondary".into(),
            priority: 0,
            counter: Arc::clone(&counter),
        })
        .build();

    coord.run_hooks(ShutdownReason::Requested);
    // A second call reports the first run; no hook runs twice.
    coord.run_hooks(ShutdownReason::Forced);

    println!("total hook runs = {}", counter.load(Ordering::Relaxed));
}
