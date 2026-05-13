//! Unified OS signal handling for Rust. SIGTERM / SIGINT / SIGHUP / SIGPIPE and Windows equivalents through one API. Graceful shutdown orchestration with priority ordering, timeout enforcement, and hook chaining. Replaces the patchwork of ctrlc + signal-hook with a clean runtime-agnostic substrate.
//!
//! # Status
//!
//! This crate is in early scaffolding. The public API is not yet
//! defined. See [the repository](https://github.com/jamesgober/mod-signal)
//! and `.dev/ROADMAP.md` for the milestone plan.

#![doc(html_root_url = "https://docs.rs/mod-signal")]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

/// Crate version string, populated by Cargo at build time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
