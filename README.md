<h1 align="center">
    <strong>signal-mod</strong>
    <br>
    <sup><sub>CROSS-PLATFORM OS SIGNAL HANDLING FOR RUST</sub></sup>
</h1>

<p align="center">
    <a href="https://crates.io/crates/signal-mod"><img alt="crates.io" src="https://img.shields.io/crates/v/signal-mod.svg"></a>
    <a href="https://crates.io/crates/signal-mod"><img alt="downloads" src="https://img.shields.io/crates/d/signal-mod.svg"></a>
    <a href="https://docs.rs/signal-mod"><img alt="docs.rs" src="https://docs.rs/signal-mod/badge.svg"></a>
    <a href="https://github.com/jamesgober/signal-mod/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/jamesgober/signal-mod/actions/workflows/ci.yml/badge.svg"></a>
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md" title="MSRV"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.75%2B-blue"></a>
</p>

<p align="center">
    One API for SIGTERM / SIGINT / SIGHUP / SIGQUIT / SIGPIPE / SIGUSR1 / SIGUSR2
    and the Windows console control events, with priority-ordered graceful
    shutdown hooks and optional Tokio and async-std adapters.
</p>


## Why signal-mod

A long-running Rust service needs three things from the OS-signal
layer that the standard library does not provide:

1. **A single API across Linux, macOS, and Windows.** Today this
   means picking between `ctrlc` (cross-platform but limited to
   Ctrl+C), `signal-hook` (Unix only, no Windows console-event
   coverage), or `tokio::signal` / `async-std` (runtime-specific,
   and the two streams have different shapes). `signal-mod`
   collapses that choice to one `Signal` enum and one `Coordinator`.
2. **Cloneable handles for observers and initiators.** Subsystems
   need to know when shutdown happened; admin endpoints, fatal-error
   sites, and supervisors need to be able to ask for it. Passing
   the same handle for both is a capability leak; `signal-mod`
   splits them into `ShutdownToken` (observe) and `ShutdownTrigger`
   (initiate).
3. **Priority-ordered cleanup with a real timeout.** Real services
   need to close listeners before draining workers before flushing
   caches before releasing pool resources. `signal-mod` runs hooks
   in descending priority under a configurable graceful budget, and
   is panic-safe across hooks (one bad hook does not abort the
   rest).


## What it does

- **One API** for SIGTERM / SIGINT / SIGHUP / SIGQUIT / SIGPIPE /
  SIGUSR1 / SIGUSR2 and the Windows console control events
  (`CTRL_C`, `CTRL_BREAK`, `CTRL_CLOSE`, `CTRL_SHUTDOWN`).
- **Graceful shutdown orchestration** with cloneable observer and
  initiator handles you can pass independently up and down a
  supervision tree.
- **Priority-ordered shutdown hooks** with a configurable graceful
  timeout budget. Hook panics are caught per-hook and do not abort
  the rest of the sequence.
- **Runtime-agnostic substrate** with optional adapters for `tokio`
  and `async-std`, plus a synchronous `ctrlc-fallback` for non-async
  code.


<hr>

## Install

Add `signal-mod` to `Cargo.toml`:

```toml
[dependencies]
signal-mod = "1.0"
```

<br>

Default features (`std + tokio`) are the right choice for a
Tokio-driven service. To opt in to a different runtime adapter,
disable defaults and pick one feature:

```toml
# async-std runtime
signal-mod = { version = "1.0", default-features = false, features = ["std", "async-std"] }

# Synchronous Ctrl+C only, no async runtime
signal-mod = { version = "1.0", default-features = false, features = ["std", "ctrlc-fallback"] }
```


## Features

| Feature          | Default | Description                                                                                |
| ---------------- | ------- | ------------------------------------------------------------------------------------------ |
| `std`            | yes     | Enables std-dependent items. Reserved for a future `no_std` story.                         |
| `tokio`          | yes     | Tokio runtime adapter; enables async `wait()` and signal listeners via `tokio::signal`.    |
| `async-std`      | no      | async-std adapter; uses `signal-hook-async-std` on Unix and `ctrlc` on Windows.            |
| `ctrlc-fallback` | no      | Synchronous `ctrlc` handler for `Signal::Interrupt` when no async runtime is enabled.      |

When both `tokio` and `async-std` are enabled, `tokio` wins (so a
workspace that pulls both transitively still gets a single
back-end).

<br>

## Quick start

```rust
use std::time::Duration;
use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, SignalSet};

#[tokio::main]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .graceful_timeout(Duration::from_secs(5))
        .hook(hook_from_fn("close-listener", 1000, |_| {
            // Stop accepting new requests.
        }))
        .hook(hook_from_fn("drain-queues", 500, |_| {
            // Finish in-flight work.
        }))
        .hook(hook_from_fn("flush-logs", 100, |reason| {
            eprintln!("flushing logs: {reason}");
        }))
        .build();

    coord.install()?;

    let token = coord.token();
    token.wait().await;

    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    let ran = coord.run_hooks(reason);
    eprintln!("ran {ran} shutdown hook(s)");
    Ok(())
}
```

<br>

## Examples

Seven runnable examples ship in [`examples/`](examples/):

| File                                                            | Demonstrates                                                                 |
| --------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| [`graceful_shutdown.rs`](examples/graceful_shutdown.rs)         | The canonical install + wait + run_hooks pattern under Tokio.                |
| [`programmatic_shutdown.rs`](examples/programmatic_shutdown.rs) | Trigger shutdown without OS signals (HTTP admin endpoint, supervisor, etc.). |
| [`multi_subsystem.rs`](examples/multi_subsystem.rs)             | Multiple observer tasks fanning out from one coordinator.                    |
| [`sync_blocking.rs`](examples/sync_blocking.rs)                 | Synchronous CLI pattern using `ctrlc-fallback` without an async runtime.     |
| [`custom_signal_set.rs`](examples/custom_signal_set.rs)         | Building `SignalSet` values at compile time and at runtime.                  |
| [`priority_hooks.rs`](examples/priority_hooks.rs)               | Hook priority ordering and the graceful timeout budget.                      |
| [`custom_hook_type.rs`](examples/custom_hook_type.rs)           | Implementing the `ShutdownHook` trait directly for stateful hooks.           |

Run any example with:

```bash
cargo run --example graceful_shutdown
```

<br>

## API at a glance

| Type               | Role                                                                              |
| ------------------ | --------------------------------------------------------------------------------- |
| `Signal`           | Cross-platform signal identifier.                                                 |
| `SignalSet`        | Bit-packed `Copy` set; const constructors `empty`, `graceful`, `standard`, `all`. |
| `ShutdownReason`   | Why shutdown was initiated (signal, requested, forced, timeout, error).           |
| `ShutdownToken`    | Cloneable observer handle. `wait`, `wait_blocking`, `reason`, `elapsed`.          |
| `ShutdownTrigger`  | Cloneable initiator handle. `trigger(reason)`.                                    |
| `ShutdownHook`     | Trait for cleanup work. `name` + `priority` + `run`.                              |
| `Coordinator`      | Owns the state machine. `install`, `run_hooks`, `statistics`, `token`, `trigger`. |
| `Error`            | Error type for fallible methods.                                                  |

Full per-item reference with multiple code examples per use case
lives in [`docs/API.md`](docs/API.md).


<br>

`signal-mod` follows [Semantic Versioning]. Every item in the public
API is covered from `1.0.0` forward; pin to a minor (`"1.0"`) to
receive patch fixes automatically.

[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

<hr><br>

## Performance

Single-platform reference (Windows 11, Rust 1.95.0). Lower is
better; rerun `cargo bench --bench shutdown_bench` to validate on
your hardware.

| Operation                                       | Median time |
| ----------------------------------------------- | ----------- |
| `ShutdownToken::is_initiated` (uninitiated)     | 364 ps      |
| `ShutdownTrigger::trigger` (state transition)   | 123 ns      |
| `ShutdownTrigger::trigger` (already initiated)  | 4.7 ns      |
| `ShutdownToken::clone`                          | 7.2 ns      |
| `Coordinator::run_hooks` (16 hooks)             | 536 ns      |
| `Coordinator::run_hooks` (64 hooks)             | 2.08 us     |
| `SignalSet::iter` (all 7 variants)              | 1.5 ns      |

More detail and methodology notes in
[`docs/API.md#performance`](docs/API.md#performance).

<br>

## Platform support

| Platform | `tokio` | `async-std` | `ctrlc-fallback` |
| -------- | ------- | ----------- | ---------------- |
| Linux    | yes     | yes         | yes              |
| macOS    | yes     | yes         | yes              |
| Windows  | yes     | partial *   | yes              |

\* On Windows, the `async-std` back-end uses a synchronous `ctrlc`
handler for `Signal::Interrupt` only, because async-std does not
ship a native Windows signal stream. The full Windows console event
matrix (`CTRL_C`, `CTRL_BREAK`, `CTRL_CLOSE`, `CTRL_SHUTDOWN`) is
available through the `tokio` back-end.

<br>

## Testing

`signal-mod` ships with the following test surface, all run in CI on
Linux, macOS, and Windows:

- **21 unit tests** in `src/`.
- **7 integration tests** in [`tests/coordinator_integration.rs`](tests/coordinator_integration.rs).
- **10 property tests** (proptest, 256 cases each) in [`tests/property_tests.rs`](tests/property_tests.rs).
- **20 edge case tests** in [`tests/edge_cases.rs`](tests/edge_cases.rs).
- **6 stress tests** under concurrent triggers, many observers, and
  high-volume cloning in [`tests/stress.rs`](tests/stress.rs).
- **6 doctests** spanning the crate root, `Coordinator`,
  `CoordinatorBuilder`, `Statistics`, `Coordinator::run_hooks`,
  and `hook_from_fn`.

<br>

Run the full suite:

```bash
cargo test --all-features
```

Run benchmarks:

```bash
cargo bench --bench shutdown_bench
```

<br>

## MSRV

Rust 1.75. MSRV bumps require a minor version increment per
[`REPS.md`](REPS.md) section 6.

<br>

## Contributing

Issues and pull requests are welcome at
[github.com/jamesgober/signal-mod](https://github.com/jamesgober/signal-mod).
Style is enforced via `cargo fmt`; correctness via
`cargo clippy --all-targets --all-features -- -D warnings`.


<br>

## License

Licensed under the Apache License, Version 2.0. See [`LICENSE`](LICENSE)
for the full text.


<br>

<!-- COPYRIGHT
############################################# -->
<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>JAMES GOBER.</strong></sup>
</div>
