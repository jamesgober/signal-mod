//! Back-end signal-handler installers.
//!
//! This module routes [`Coordinator::install`](crate::Coordinator::install)
//! to the right back-end at compile time, based on the active Cargo
//! features:
//!
//! | Feature           | Back-end                                           |
//! | ----------------- | -------------------------------------------------- |
//! | `tokio`           | [`tokio_rt`] - `tokio::signal` + `tokio::spawn`.   |
//! | `async-std`       | [`async_std_rt`] - `signal-hook-async-std` + ctrlc.|
//! | `ctrlc-fallback`  | [`ctrlc_sync`] - synchronous `ctrlc::set_handler`. |
//! | none of the above | [`Coordinator::install`] returns `Error::NoRuntime`. |
//!
//! `tokio` takes precedence over `async-std` when both are enabled.

#[cfg(feature = "tokio")]
pub(crate) mod tokio_rt;

#[cfg(all(feature = "async-std", not(feature = "tokio")))]
pub(crate) mod async_std_rt;

#[cfg(all(
    feature = "ctrlc-fallback",
    not(feature = "tokio"),
    not(feature = "async-std")
))]
pub(crate) mod ctrlc_sync;
