//! Reload on SIGHUP, reopen logs on SIGUSR1, stop on SIGTERM / Ctrl+C.
//!
//! Notify signals reach `SignalSubscription`s without starting a
//! shutdown, so one coordinator handles both kinds.
//!
//! Run with:
//!
//! ```text
//! cargo run --example reload_on_sighup
//! ```
//!
//! then, from another terminal on Unix: `kill -HUP <pid>`,
//! `kill -USR1 <pid>`, and finally `kill <pid>` or Ctrl+C.

use signal_mod::{Coordinator, Signal, SignalSet};

#[tokio::main]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful() - Signal::Hangup)
        .notify_signals(SignalSet::empty().with(Signal::Hangup).with(Signal::User1))
        .build();
    coord.install()?;
    println!(
        "pid {} listening for {:?}",
        std::process::id(),
        coord.installed_signals()
    );

    let token = coord.token();
    let mut events = coord.subscribe();
    loop {
        tokio::select! {
            () = token.wait() => break,
            Some(sig) = events.recv() => match sig {
                Signal::Hangup => println!("reloading configuration"),
                Signal::User1 => println!("reopening log files"),
                other => println!("received {other}"),
            },
        }
    }
    println!("stopping: {:?}", token.reason());
    Ok(())
}
