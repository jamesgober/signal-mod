//! Cross-platform OS signal handling and graceful-shutdown orchestration.
//!
//! `signal-mod` gives a program one API for SIGTERM, SIGINT, SIGHUP,
//! SIGQUIT, SIGPIPE, SIGUSR1, SIGUSR2 and the Windows console control
//! events, and the pieces a long-running service needs around them:
//!
//! - [`Signal`] / [`SignalSet`]: platform-neutral signal names with
//!   per-target numbers.
//! - [`Coordinator`]: installs the listeners, owns the shutdown state,
//!   and runs priority-ordered [`ShutdownHook`]s exactly once under an
//!   enforced graceful-then-force deadline, reporting what each hook did
//!   ([`RunReport`]).
//! - [`ShutdownToken`] (observe) and [`ShutdownTrigger`] (initiate):
//!   cheap cloneable handles with runtime-agnostic waiting, child
//!   scopes, and subsystem guards for "wait until every worker stopped".
//! - [`SignalSubscription`]: signals that should *not* stop the process,
//!   such as SIGHUP for "reload" or SIGUSR1 for "reopen logs".
//! - Opt-in "second Ctrl+C exits now" escalation, conventional exit codes
//!   ([`ShutdownReason::exit_code`]), and [`shutdown_signal`] for
//!   `with_graceful_shutdown` servers.
//!
//! # Quick start
//!
//! ```no_run
//! use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, SignalSet};
//! use std::time::Duration;
//!
//! #[tokio::main]
//! async fn main() -> signal_mod::Result<()> {
//!     let coord = Coordinator::builder()
//!         .signals(SignalSet::graceful())
//!         .graceful_timeout(Duration::from_secs(5))
//!         .force_exit_on_second_signal(true)
//!         .hook(hook_from_fn("flush-logs", 100, |reason| {
//!             eprintln!("shutting down: {reason}");
//!         }))
//!         .build();
//!
//!     coord.install()?;
//!
//!     let token = coord.token();
//!     token.wait().await;
//!
//!     let reason = token.reason().unwrap_or(ShutdownReason::Requested);
//!     let report = coord.run_hooks_async(reason).await;
//!     eprintln!("{report}");
//!     std::process::exit(reason.exit_code());
//! }
//! ```
//!
//! # Back-ends and features
//!
//! | Feature          | Default | What it does                                                        |
//! | ---------------- | ------- | ------------------------------------------------------------------- |
//! | `std`            | yes     | Reserved for a future `no_std` core.                                 |
//! | `tokio`          | yes     | [`Coordinator::install`] inside a Tokio runtime uses `tokio::signal`. |
//! | `agnostic`       | no      | A listener thread; works with any executor or none.                  |
//! | `smol`           | no      | Alias of `agnostic`.                                                 |
//! | `ctrlc-fallback` | no      | 1.x name, now an alias of `agnostic`.                                |
//! | `async-std`      | no      | Deprecated alias of `agnostic`; no longer depends on async-std.     |
//! | `tracing`        | no      | Structured diagnostics for signals, installs and hooks.              |
//! | `tokio-util`     | no      | `ShutdownToken::cancellation_token` interop.                        |
//!
//! Waiting ([`ShutdownToken::wait`]), subscriptions and
//! [`Coordinator::run_hooks_async`] are runtime-agnostic in every
//! configuration; the features only decide where signals come from.
//!
//! # Platform notes
//!
//! Signal numbers come from the target's `libc` (SIGUSR1 is 10 on Linux,
//! 30 on macOS and the BSDs). On Windows, `Terminate` is
//! `CTRL_CLOSE_EVENT` (the OS kills the process about 5 seconds later),
//! `Quit` is `CTRL_BREAK_EVENT`, and `Hangup` covers `CTRL_LOGOFF_EVENT`
//! and `CTRL_SHUTDOWN_EVENT` (delivered to services only); see
//! [`Signal`].
//!
//! The design contract lives in `REPS.md` and the full guide in
//! `docs/API.md` in the repository.

#![cfg_attr(docsrs, feature(doc_cfg))]
// Two modules opt out locally, each with `SAFETY:` justifications: the
// Windows console handler registration (`install/console.rs`) and a
// read-only `sigaction` query on Unix (`install/disposition.rs`).
#![deny(unsafe_code)]
// REPS requires `deny(warnings)`. It is lifted for docs.rs only, which
// builds the published crate once with that day's nightly: a lint added
// later must not leave a release without documentation.
#![cfg_attr(not(docsrs), deny(warnings))]
#![deny(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]
#![deny(unused_must_use)]
#![deny(unused_results)]
#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![deny(clippy::todo)]
#![deny(clippy::unimplemented)]
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]
#![deny(clippy::dbg_macro)]
#![deny(clippy::unreachable)]
#![deny(clippy::undocumented_unsafe_blocks)]
#![deny(clippy::missing_safety_doc)]
#![warn(clippy::pedantic)]

#[macro_use]
mod trace;

mod coord;
mod dispatch;
mod error;
mod hook;
mod install;
mod reason;
mod report;
mod runner;
mod shutdown_signal;
mod signal;
mod state;
mod sync;
mod token;
mod waitlist;

#[cfg(all(test, loom))]
mod loom_tests;

pub use crate::coord::{Coordinator, CoordinatorBuilder, Statistics};
pub use crate::dispatch::{Recv, SignalSubscription};
pub use crate::error::{Error, Result};
pub use crate::hook::{FnHook, HookContext, ShutdownHook, hook_from_fn};
pub use crate::install::Backend;
pub use crate::reason::ShutdownReason;
pub use crate::report::{HookOutcome, HookPhase, HookStatus, RunReport};
pub use crate::runner::RunHooksFuture;
pub use crate::shutdown_signal::{ShutdownSignal, shutdown_signal};
pub use crate::signal::{Signal, SignalSet, SignalSetIter};
pub use crate::token::{
    ShutdownFuture, ShutdownToken, ShutdownTrigger, SubsystemGuard, SubsystemsFinished,
};

/// Crate version string, populated by Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// Compile and run every Rust block in the user-facing Markdown docs.
#[cfg(all(doctest, not(loom)))]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

#[cfg(all(doctest, not(loom)))]
#[doc = include_str!("../docs/API.md")]
struct ApiGuideDoctests;

#[cfg(all(doctest, not(loom)))]
#[doc = include_str!("../docs/MIGRATION.md")]
struct MigrationGuideDoctests;
