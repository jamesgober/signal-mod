//! Unified OS signal handling for Rust.
//!
//! `mod-signal` is a runtime-agnostic substrate for cross-platform
//! signal handling and graceful shutdown orchestration. It provides:
//!
//! - A platform-neutral [`Signal`] enum covering SIGTERM, SIGINT,
//!   SIGHUP, SIGQUIT, SIGPIPE, SIGUSR1, SIGUSR2 and their Windows
//!   console-event equivalents.
//! - A [`Coordinator`] that owns the shutdown state machine and runs
//!   priority-ordered [`ShutdownHook`]s under a configurable timeout
//!   ladder.
//! - Cloneable [`ShutdownToken`] (observer) and [`ShutdownTrigger`]
//!   (initiator) handles that can be passed independently through a
//!   program's supervision tree.
//! - Optional runtime adapters for `tokio` and `async-std` exposing
//!   `token.wait().await`.
//!
//! # Quick start
//!
//! ```no_run
//! use mod_signal::{Coordinator, ShutdownReason, SignalSet};
//! use std::time::Duration;
//!
//! # #[cfg(feature = "tokio")]
//! #[tokio::main]
//! async fn main() -> mod_signal::Result<()> {
//!     let coord = Coordinator::builder()
//!         .signals(SignalSet::graceful())
//!         .graceful_timeout(Duration::from_secs(5))
//!         .hook(mod_signal::hook_from_fn(
//!             "flush-logs",
//!             100,
//!             |reason| eprintln!("shutting down: {reason}"),
//!         ))
//!         .build();
//!
//!     coord.install()?;
//!
//!     let token = coord.token();
//!     token.wait().await;
//!
//!     let reason = token.reason().unwrap_or(ShutdownReason::Requested);
//!     coord.run_hooks(reason);
//!     Ok(())
//! }
//! # #[cfg(not(feature = "tokio"))]
//! # fn main() {}
//! ```
//!
//! See `.dev/DESIGN.md` and `REPS.md` for the design contract.

#![doc(html_root_url = "https://docs.rs/mod-signal")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod coord;
mod error;
mod hook;
mod reason;
mod signal;
mod state;
mod token;

pub use crate::coord::{Coordinator, CoordinatorBuilder, Statistics};
pub use crate::error::{Error, Result};
pub use crate::hook::{hook_from_fn, FnHook, ShutdownHook};
pub use crate::reason::ShutdownReason;
pub use crate::signal::{Signal, SignalSet, SignalSetIter};
pub use crate::token::{ShutdownToken, ShutdownTrigger};

/// Crate version string, populated by Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
