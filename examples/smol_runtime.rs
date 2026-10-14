//! signal-mod under smol, with no Tokio runtime involved.
//!
//! The `agnostic` back-end delivers signals without a runtime, and
//! `ShutdownToken::wait` / `run_hooks_async` are plain futures woken by
//! the shutdown itself, so any executor works.
//!
//! Run with:
//!
//! ```text
//! cargo run --example smol_runtime --no-default-features --features std,smol
//! ```

#[cfg(feature = "agnostic")]
fn main() -> signal_mod::Result<()> {
    use signal_mod::{Coordinator, ShutdownReason, hook_from_fn};

    smol::block_on(async {
        let coord = Coordinator::builder()
            .hook(hook_from_fn("close-db", 0, |reason| {
                println!("closing the database ({reason})");
            }))
            .build();
        coord.install()?;
        println!("running under smol; press Ctrl+C to exit");

        let token = coord.token();
        token.wait().await;

        let reason = token.reason().unwrap_or(ShutdownReason::Requested);
        let report = coord.run_hooks_async(reason).await;
        println!("{report}");
        Ok(())
    })
}

#[cfg(not(feature = "agnostic"))]
fn main() {
    eprintln!("This example needs: --no-default-features --features std,smol");
}
