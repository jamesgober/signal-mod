<h1 align="center">
    <strong>mod-signal</strong>
    <br>
    <sup><sub>CROSS-PLATFORM OS SIGNAL HANDLING FOR RUST</sub></sup>
</h1>

<p align="center">
    <a href="https://crates.io/crates/mod-signal"><img alt="crates.io" src="https://img.shields.io/crates/v/mod-signal.svg"></a>
    <a href="https://crates.io/crates/mod-signal"><img alt="downloads" src="https://img.shields.io/crates/d/mod-signal.svg"></a>
    <a href="https://docs.rs/mod-signal"><img alt="docs.rs" src="https://docs.rs/mod-signal/badge.svg"></a>
    <a href="https://github.com/jamesgober/mod-signal/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/jamesgober/mod-signal/actions/workflows/ci.yml/badge.svg"></a>
</p>

<p align="center">
    One API for SIGTERM / SIGINT / SIGHUP and Windows equivalents, with priority-ordered graceful shutdown hooks.
</p>

---

## Status

ACTIVE - pre-1.0 stabilization. Current release: `0.9.x`. API
freeze begins at `1.0.0-rc.1`. See `.dev/ROADMAP.md` for the path
to `1.0`.

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
use mod_signal::{Coordinator, ShutdownReason, SignalSet};
use std::time::Duration;

#[tokio::main]
async fn main() -> mod_signal::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .graceful_timeout(Duration::from_secs(5))
        .hook(mod_signal::hook_from_fn(
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

Copyright (C) 2026 James Gober.
