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
</p>

<p align="center">
    One API for SIGTERM / SIGINT / SIGHUP and Windows equivalents, with priority-ordered graceful shutdown hooks.
</p>


## What it does

Unified OS signal handling for Rust:

- **One API** for SIGTERM / SIGINT / SIGHUP / SIGQUIT / SIGPIPE /
  SIGUSR1 / SIGUSR2 and Windows console control events
  (`CTRL_C`, `CTRL_BREAK`, `CTRL_CLOSE`, `CTRL_SHUTDOWN`).
- **Graceful shutdown orchestration** with cloneable observer and
  initiator handles that you can pass independently up and down a
  supervision tree.
- **Priority-ordered shutdown hooks** with a configurable timeout
  ladder.
- **Runtime-agnostic substrate** with optional adapters for
  `tokio` and `async-std`, plus a synchronous `ctrlc-fallback`
  for non-async code.

## Quick start

```rust
use signal_mod::{Coordinator, ShutdownReason, SignalSet};
use std::time::Duration;

#[tokio::main]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .graceful_timeout(Duration::from_secs(5))
        .hook(signal_mod::hook_from_fn(
            "flush-logs",
            100,
            |reason| eprintln!("shutting down: {reason}"),
        ))
        .build();

    coord.install()?;

    let token = coord.token();
    token.wait().await;

    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    coord.run_hooks(reason);
    Ok(())
}
```

See [`examples/graceful_shutdown.rs`](examples/graceful_shutdown.rs)
for a complete runnable example, [`docs/API.md`](docs/API.md) for
the offline API reference, and [`docs/MIGRATION.md`](docs/MIGRATION.md)
for upgrade paths.

## Cargo features

| Feature          | Default | Effect                                                      |
| ---------------- | ------- | ----------------------------------------------------------- |
| `std`            | yes     | Enables std-dependent items.                                |
| `tokio`          | yes     | Spawns signal listeners on the `tokio` runtime.             |
| `async-std`      | no      | Spawns signal listeners on `async-std`.                     |
| `ctrlc-fallback` | no      | Adds synchronous `ctrlc` fallback for `Coordinator::install`. |

When both `tokio` and `async-std` are enabled, `tokio` wins.

## MSRV

Rust 1.75. Bumps require a minor version increment and a
`CHANGELOG.md` `### Changed` entry per `REPS.md` section 6.

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