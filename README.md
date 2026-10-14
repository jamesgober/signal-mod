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
    <a href="https://github.com/rust-lang/rfcs/blob/master/text/2495-min-rust-version.md" title="MSRV"><img alt="MSRV" src="https://img.shields.io/badge/MSRV-1.85%2B-blue"></a>
    <a href="#license"><img alt="License" src="https://img.shields.io/badge/license-Apache--2.0%20OR%20MIT-blue.svg"></a>
</p>

<p align="center">
    One API for SIGTERM / SIGINT / SIGHUP / SIGQUIT / SIGPIPE / SIGUSR1 / SIGUSR2
    and the Windows console control events, with graceful-shutdown hooks
    that run once under an enforced deadline.
</p>


## Why signal-mod

A long-running Rust service needs more from the OS-signal layer than
the standard library provides:

1. **One API across Linux, macOS and Windows.** `ctrlc` covers Ctrl+C
   only, `signal-hook` is Unix only, and `tokio::signal` ties you to a
   runtime. `signal-mod` gives one `Signal` enum, per-target signal
   numbers, and back-ends for Tokio or any runtime (or none).
2. **Separate observer and initiator handles.** Subsystems need to see
   shutdown; admin endpoints and supervisors need to request it.
   `ShutdownToken` observes, `ShutdownTrigger` initiates.
3. **Cleanup that cannot hang the process.** Hooks run in priority order,
   once, on worker threads, under a graceful budget and a force budget
   that are enforced: a stuck hook is reported and abandoned, the
   remaining hooks still run, and you get a report of what happened.
4. **Not every signal means "stop".** SIGHUP can mean reload and SIGUSR1
   reopen logs. Notify signals go to subscriptions instead of starting a
   shutdown.


## What it does

- **Signals:** SIGTERM, SIGINT, SIGHUP, SIGQUIT, SIGPIPE, SIGUSR1,
  SIGUSR2 and `CTRL_C` / `CTRL_BREAK` / `CTRL_CLOSE` / `CTRL_LOGOFF` /
  `CTRL_SHUTDOWN`, with numbers taken from the target's `libc`.
- **Shutdown state:** cloneable tokens and triggers, runtime-agnostic
  `wait().await` (no polling), blocking waits, child scopes, subsystem
  guards with "wait until all workers stopped", `CancellationToken`
  interop.
- **Hooks:** priority order, run exactly once, graceful then force
  deadline, per-hook timeouts, panic capture with message, a
  `RunReport`, live per-hook progress in `Statistics`, and
  `run_hooks_async` that never blocks the executor.
- **Escalation:** opt-in "second Ctrl+C ends the process now" (the
  signal is re-raised on Unix, so shells see a real SIGINT death; exit
  status 130 / 143 on Windows), and `ShutdownReason::exit_code()` for the
  normal path.
- **Clean teardown:** dropping the coordinator removes its listeners.
  On Unix, shutdown signals that were at their default action before
  signal-mod get it back; signals that other code ignored or handled are
  left to that code.
- **Hook cooperation:** `HookContext::current()` tells a running hook its
  deadline and whether it has been abandoned.


<hr>

## Install

```toml
[dependencies]
signal-mod = "2"
```

Default features (`std`, `tokio`) suit a Tokio service. Other set-ups:

```toml
# smol, async-std, any other executor, or no runtime at all
signal-mod = { version = "2", default-features = false, features = ["std", "agnostic"] }

# Tokio plus structured diagnostics and CancellationToken interop
signal-mod = { version = "2", features = ["tracing", "tokio-util"] }
```


## Features

| Feature          | Default | Description                                                                                 |
| ---------------- | ------- | ------------------------------------------------------------------------------------------- |
| `std`            | yes     | Reserved for a future `no_std` core.                                                         |
| `tokio`          | yes     | `install()` inside a Tokio runtime uses `tokio::signal` listener tasks.                      |
| `agnostic`       | no      | `install()` anywhere: a listener thread (Windows without `tokio`: a console control handler). |
| `smol`           | no      | Alias of `agnostic`.                                                                        |
| `ctrlc-fallback` | no      | 1.x name, now an alias of `agnostic` (covers every signal, not just Ctrl+C).                |
| `async-std`      | no      | Deprecated alias of `agnostic`. async-std is discontinued; the crate is no longer pulled in. |
| `tracing`        | no      | `tracing` events for signals, installs, hooks (start / finish / panic / timeout), escalation. |
| `tokio-util`     | no      | `ShutdownToken::cancellation_token()` and `From<&ShutdownToken> for CancellationToken`.      |

With both `tokio` and `agnostic` enabled, `install()` uses Tokio inside
a Tokio runtime and the agnostic back-end elsewhere.


## Quick start

```rust,no_run
use std::time::Duration;
use signal_mod::{Coordinator, ShutdownReason, SignalSet, hook_from_fn};

#[tokio::main]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .graceful_timeout(Duration::from_secs(5))
        .force_timeout(Duration::from_secs(8))
        .force_exit_on_second_signal(true)
        .hook(hook_from_fn("close-listener", 1000, |_| {
            // Stop accepting new requests.
        }))
        .hook(hook_from_fn("drain-queues", 500, |_| {
            // Finish in-flight work.
        }))
        .hook(
            hook_from_fn("flush-logs", 100, |reason| eprintln!("flushing logs: {reason}"))
                .with_timeout(Duration::from_secs(1)),
        )
        .build();

    coord.install()?;

    let token = coord.token();
    token.wait().await;

    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    let report = coord.run_hooks_async(reason).await;
    eprintln!("{report}");
    std::process::exit(reason.exit_code());
}
```

### Reload on SIGHUP

```rust,no_run
use signal_mod::{Coordinator, Signal, SignalSet};

#[tokio::main]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful() - Signal::Hangup)
        .notify_signals(SignalSet::empty().with(Signal::Hangup).with(Signal::User1))
        .build();
    coord.install()?;

    let token = coord.token();
    let mut events = coord.subscribe();
    loop {
        tokio::select! {
            () = token.wait() => break,
            Some(sig) = events.recv() => match sig {
                Signal::Hangup => { /* reload configuration */ }
                Signal::User1 => { /* reopen log files */ }
                _ => {}
            },
        }
    }
    Ok(())
}
```

### axum / hyper `with_graceful_shutdown`

```rust,no_run
# async fn serve() -> std::io::Result<()> {
let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
axum::serve(listener, axum::Router::new())
    .with_graceful_shutdown(signal_mod::shutdown_signal().map_err(std::io::Error::other)?)
    .await
# }
```

### No runtime

```rust,no_run
// signal-mod = { version = "2", default-features = false, features = ["std", "agnostic"] }
use signal_mod::{Coordinator, ShutdownReason};

fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder().build();
    coord.install()?;
    coord.token().wait_blocking();
    let reason = coord.token().reason().unwrap_or(ShutdownReason::Requested);
    coord.run_hooks(reason);
    std::process::exit(reason.exit_code());
}
```


## How hooks run

```text
run start ─── graceful_timeout ──────────── force_timeout
  │  hooks start with your reason  │  remaining hooks start   │  rest
  │  (priority order)              │  with ShutdownReason::   │  skipped
  │                                │  Forced                  │
```

- A hook still running at its own `timeout()` or at its phase's deadline
  is reported `TimedOut` and abandoned; the next hook starts at once.
- A panicking hook is reported `Panicked` with its message.
- The first `run_hooks*` call runs the hooks; every other call waits for
  that run and returns the same `RunReport`. No hook runs twice.
- Hooks run on a worker thread with the caller's Tokio runtime entered,
  so `Handle::current().block_on(async_cleanup)` works from a hook when
  you use `run_hooks_async`.


## Examples

| File                                                            | Demonstrates                                                       |
| --------------------------------------------------------------- | ------------------------------------------------------------------ |
| [`graceful_shutdown.rs`](examples/graceful_shutdown.rs)         | install, wait, `run_hooks_async`, report, exit code, escalation     |
| [`reload_on_sighup.rs`](examples/reload_on_sighup.rs)           | notify signals and subscriptions next to shutdown                  |
| [`axum_server.rs`](examples/axum_server.rs)                     | `with_graceful_shutdown`, plus the `shutdown_signal()` one-liner   |
| [`multi_subsystem.rs`](examples/multi_subsystem.rs)             | subsystem guards and waiting for every worker to stop              |
| [`priority_hooks.rs`](examples/priority_hooks.rs)               | priority order, a hung hook, the force phase, the report           |
| [`programmatic_shutdown.rs`](examples/programmatic_shutdown.rs) | triggering shutdown without a signal                               |
| [`sync_blocking.rs`](examples/sync_blocking.rs)                 | no runtime (`agnostic`)                                            |
| [`smol_runtime.rs`](examples/smol_runtime.rs)                   | smol (`agnostic`)                                                  |
| [`custom_signal_set.rs`](examples/custom_signal_set.rs)         | `SignalSet` algebra and per-platform numbers                       |
| [`custom_hook_type.rs`](examples/custom_hook_type.rs)           | implementing `ShutdownHook`, run-once                              |

```bash
cargo run --example graceful_shutdown
cargo run --example sync_blocking --no-default-features --features std,agnostic
```


## API at a glance

| Item                                  | Role                                                                          |
| ------------------------------------- | ----------------------------------------------------------------------------- |
| `Signal`, `SignalSet`                 | Signal names, per-target numbers, exit codes; const set with `|`, `&`, `-`.   |
| `Coordinator`, `CoordinatorBuilder`   | Install / uninstall, handles, subscriptions, hook runs, statistics.            |
| `ShutdownToken`                       | Observe: `wait`, `wait_blocking`, `reason`, `child`, `subsystem_guard`.       |
| `ShutdownTrigger`                     | Initiate: `trigger(reason)`.                                                  |
| `ShutdownHook`, `hook_from_fn`        | Cleanup work with `name`, `priority`, `timeout`, `run`.                       |
| `RunReport`, `HookOutcome`            | What each hook did: status, phase, duration, panic message.                   |
| `Statistics`                          | Live snapshot, including per-hook progress.                                   |
| `SignalSubscription`                  | Notify (and shutdown) signals as they arrive.                                 |
| `shutdown_signal()`                   | One-line future for `with_graceful_shutdown`.                                 |
| `ShutdownReason`, `Error`             | Why shutdown started; what failed and what to do.                             |

Full reference with examples: [`docs/API.md`](docs/API.md). Upgrading
from 1.x: [`docs/MIGRATION.md`](docs/MIGRATION.md).

`signal-mod` follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).


## Performance

Measured with `cargo bench --bench shutdown_bench` (criterion) on an
AMD Ryzen 9 9950X3D, Windows 11, Rust 1.99.0, release profile, both
versions back to back on 2026-10-14. The machine was shared with other
builds (roughly 25-60 % CPU load), so treat the numbers as upper bounds
and rerun the bench on your hardware. 1.0.0 was measured with its own
bench file (criterion 0.5). The `SignalSet::iter` row compares both
crates inside one criterion 0.8 harness with the input behind
`black_box`; the two bench files' harness overheads differ by a few
nanoseconds.

| Operation                                        | 2.0.0         | 1.0.0    |
| ------------------------------------------------ | ------------- | -------- |
| `ShutdownToken::is_initiated` (not initiated)    | 390 ps        | 534 ps   |
| `ShutdownToken::reason` (initiated, lock-free)   | 690 ps        | n/a      |
| `ShutdownTrigger::trigger` (first, transition)   | 73 ns         | 165 ns   |
| `ShutdownTrigger::trigger` (already initiated)   | 2.0 ns        | 6.0 ns   |
| `ShutdownToken::clone`                           | 9.5 ns        | 8.9 ns   |
| poll a ready `wait()` future                     | 11 ns         | n/a      |
| `Coordinator::statistics` (16 hooks)             | 425 ns        | n/a      |
| subscription deliver + `try_recv`                | 86 ns         | n/a      |
| `SignalSet::iter` (all 7, same harness)          | 3.3-3.7 ns    | 2.9-3.7 ns |
| `Coordinator::run_hooks` (1 hook)                | 23 us         | 74 ns    |
| `Coordinator::run_hooks` (4 hooks)               | 25-200 us (*) | 198 ns   |
| `Coordinator::run_hooks` (64 hooks)              | 34 us         | 2.6 us   |
| `Coordinator::builder().build()` (no hooks)      | 193 ns        | n/a      |
| `Coordinator::builder().build()` (16 hooks)      | 48 us         | n/a      |
| `wait_blocking_timeout(1 us)` (not initiated)    | 15.6 ms       | 15.2 ms  |

(*) Scheduler variance on Windows under load: the median moved between
29 us and 96 us across runs. `run_hooks` and `build()` were re-measured
on 2026-10-14 at about 70 % background load; the other rows are from
the back-to-back comparison run.

- The observer paths got faster: the reason is published in atomics, so
  `reason()` and a redundant `trigger()` are plain loads.
- `run_hooks` is slower by design, and this is the one tracked metric
  that regressed. Enforcing a deadline on arbitrary hook code needs the
  hooks to run on another thread (1.0.0 ran them inline and could not
  stop a hung one). A coordinator with hooks therefore parks a standby
  worker thread when it is built; a run costs one hand-off to it and one
  wake-up of the caller: tens of microseconds, once per coordinator run,
  with large scheduler variance on Windows. The thread spawn itself moved
  into `build()` (the 16-hook row). The maintainer accepted this
  regression as the justified cost of the enforced deadline. Hooks that finish in time
  add no context switch each, so 64 hooks cost about the same as one.
- `wait_blocking_timeout` with a sub-millisecond budget measures the
  Windows timer granularity (about 15.6 ms), not the primitive.


## Platform support

| Platform               | `tokio` | `agnostic` | Real-signal tests in CI |
| ---------------------- | ------- | ---------- | ----------------------- |
| Linux (x86_64, ARM64)  | yes     | yes        | yes                     |
| macOS (ARM64)          | yes     | yes        | yes                     |
| Windows (x86_64)       | yes     | yes        | yes (`CTRL_BREAK`)      |
| FreeBSD, NetBSD, i686  | yes     | yes        | compile and lint only   |

On Windows, `Pipe`, `User1` and `User2` are never delivered,
`CTRL_LOGOFF` / `CTRL_SHUTDOWN` reach services only, and the OS ends the
process about 5 seconds after `CTRL_CLOSE`; see the
[platform notes](docs/API.md#platform-behavior).


## Testing

Unit, integration, property (proptest), real-thread stress, loom model
checks of the state machine, and end-to-end real-signal tests that
deliver SIGUSR1 / SIGUSR2 / SIGHUP / SIGTERM / SIGINT / SIGPIPE (Unix)
and `CTRL_BREAK` (Windows) to child processes. CI runs them on Linux
(x86_64 and ARM64), macOS and Windows for the default, all-features and
no-default-features builds and for every feature on its own, plus MSRV,
AddressSanitizer, cargo-audit, cargo-deny and cargo-semver-checks.

```bash
cargo test --all-features
RUSTFLAGS="--cfg loom" cargo test --lib --release --no-default-features --features std loom
cargo bench --bench shutdown_bench
```


## MSRV

Rust 1.85. Raising it is a breaking change and needs a major version.


## Development

`Cargo.lock` is committed and `rust-toolchain.toml` pins the toolchain
used for lints, formatting and tests (currently 1.99.0, bumped
deliberately). Neither affects crates that depend on signal-mod: Cargo
ignores a dependency's lockfile, and the toolchain file only applies
inside this repository. A weekly `latest-deps` CI job runs against
freshly resolved dependencies to catch drift.


## Contributing

Issues and pull requests are welcome at
[github.com/jamesgober/signal-mod](https://github.com/jamesgober/signal-mod).
Style is enforced with `cargo fmt`; correctness with
`cargo clippy --all-targets --all-features -- -D warnings -W clippy::pedantic`.


## License

Licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution
intentionally submitted for inclusion in the work by you, as defined in
the Apache-2.0 license, shall be dual licensed as above, without any
additional terms or conditions.


<br>

<!-- COPYRIGHT
############################################# -->
<div align="center">
  <h2></h2>
  <sup>COPYRIGHT <small>&copy;</small> 2026 <strong>JAMES GOBER.</strong></sup>
</div>
