//! Implementing the `ShutdownHook` trait directly.
//!
//! `hook_from_fn` is the easiest path for one-off closures, but the
//! trait is open. Implementing it directly lets a hook own state
//! (e.g. a database handle), customize its `Debug` output, or share
//! logic across multiple instances.
//!
//! Run with:
//!
//! ```text
//! cargo run --example custom_hook_type
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use signal_mod::{Coordinator, ShutdownHook, ShutdownReason};

/// A hook that counts how many times it has been invoked.
///
/// Holds an `Arc<AtomicUsize>` so the counter can be observed
/// externally after `run_hooks` returns.
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

    fn run(&self, reason: ShutdownReason) {
        let prev = self.counter.fetch_add(1, Ordering::Relaxed);
        println!(
            "[{}] hook fired (priority={}, prev_count={}, reason={})",
            self.label, self.priority, prev, reason,
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
    coord.run_hooks(ShutdownReason::Forced);

    println!("counter final value = {}", counter.load(Ordering::Relaxed));
}
