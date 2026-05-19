//! Multi-subsystem fan-out: one coordinator, many observer tasks.
//!
//! Each "subsystem" gets its own `ShutdownToken` clone and runs an
//! independent main loop. When the coordinator's shutdown is
//! initiated, every subsystem wakes from its own `wait().await`
//! simultaneously.
//!
//! Run with:
//!
//! ```text
//! cargo run --example multi_subsystem
//! ```

#[cfg(feature = "tokio")]
use std::time::Duration;

#[cfg(feature = "tokio")]
use signal_mod::{Coordinator, ShutdownReason, ShutdownToken};

#[cfg(feature = "tokio")]
async fn subsystem(name: &'static str, token: ShutdownToken) {
    println!("[{name}] starting; waiting for shutdown");
    token.wait().await;
    println!(
        "[{name}] shutdown observed (reason={})",
        token.reason().unwrap_or(ShutdownReason::Requested)
    );
}

#[cfg(feature = "tokio")]
#[tokio::main(flavor = "current_thread")]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder().build();

    let workers: Vec<&'static str> = vec!["http", "db", "metrics", "scheduler"];
    let mut handles = Vec::new();
    for name in workers {
        let token = coord.token();
        handles.push(tokio::spawn(subsystem(name, token)));
    }

    // Drive shutdown after a short delay so every subsystem is in its
    // wait loop. In a real app this would be `coord.install()?` plus
    // a real OS signal.
    let trigger = coord.trigger();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        trigger.trigger(ShutdownReason::Requested);
    });

    for h in handles {
        h.await.expect("subsystem task panicked");
    }

    let stats = coord.statistics();
    println!("done. elapsed since trigger: {:?}", stats.elapsed);
    Ok(())
}

#[cfg(not(feature = "tokio"))]
fn main() {
    eprintln!("This example requires the `tokio` feature.");
}
