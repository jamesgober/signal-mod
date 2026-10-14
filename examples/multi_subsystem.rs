//! One coordinator, many subsystems: fan-out on shutdown, then wait
//! until every subsystem has actually stopped.
//!
//! Each subsystem holds a `SubsystemGuard` while it runs. After shutdown
//! is initiated, `subsystems_finished()` resolves once every guard is
//! dropped, which is the point where it is safe to run the final hooks.
//!
//! Run with:
//!
//! ```text
//! cargo run --example multi_subsystem
//! ```

use std::time::Duration;

use signal_mod::{Coordinator, ShutdownReason, ShutdownToken};

async fn subsystem(name: &'static str, token: ShutdownToken, drain: Duration) {
    let _running = token.subsystem_guard();
    println!("[{name}] running");
    token.wait().await;
    println!("[{name}] draining for {drain:?}");
    tokio::time::sleep(drain).await;
    println!("[{name}] stopped");
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let coord = Coordinator::builder().build();
    let token = coord.token();

    for (name, drain) in [
        ("http", Duration::from_millis(80)),
        ("db", Duration::from_millis(40)),
        ("metrics", Duration::from_millis(10)),
    ] {
        drop(tokio::spawn(subsystem(name, token.clone(), drain)));
    }
    tokio::task::yield_now().await;

    // In a real service: `coord.install()?` and an OS signal.
    let trigger = coord.trigger();
    drop(tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        trigger.trigger(ShutdownReason::Requested);
    }));

    token.wait().await;
    match tokio::time::timeout(Duration::from_secs(5), token.subsystems_finished()).await {
        Ok(()) => println!("all subsystems stopped"),
        Err(_) => println!("{} subsystem(s) still running", token.active_subsystems()),
    }
    println!("elapsed since trigger: {:?}", coord.statistics().elapsed);
}
