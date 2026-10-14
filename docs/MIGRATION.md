# signal-mod - Migration Guide

This guide covers upgrading from `signal-mod 1.x` to `2.0`, and adopting
the crate from `ctrlc`, `signal-hook` or `tokio::signal`. For the
authoritative API see [`API.md`](API.md); for the design contract see
[`../REPS.md`](../REPS.md).

## Table of contents

- [1.x to 2.0 at a glance](#1x-to-20-at-a-glance)
- [Breaking changes](#breaking-changes)
- [Deprecations](#deprecations)
- [New capabilities worth adopting](#new-capabilities-worth-adopting)
- [Adopting signal-mod from other crates](#adopting-signal-mod-from-other-crates)
- [Coexisting with other signal handlers](#coexisting-with-other-signal-handlers)
- [Versioning policy](#versioning-policy)

## 1.x to 2.0 at a glance

```toml
[dependencies]
signal-mod = "2"
```

Most 1.x code compiles unchanged. The typical service:

```rust,no_run
use signal_mod::{Coordinator, ShutdownReason, SignalSet, hook_from_fn};

#[tokio::main]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .hook(hook_from_fn("flush", 0, |_| {}))
        .build();
    coord.install()?;
    let token = coord.token();
    token.wait().await;
    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    // 1.x: `coord.run_hooks(reason)` - still compiles and works.
    // 2.0: prefer the async form inside a runtime; it does not block it.
    let report = coord.run_hooks_async(reason).await;
    eprintln!("{report}");
    Ok(())
}
```

What changes for you depends on what you relied on. Read the next
section.

## Breaking changes

### MSRV 1.85, edition 2024

1.x supported Rust 1.75. 2.0 needs 1.85 (and is written in edition
2024). Pin `signal-mod = "1"` if you cannot upgrade the toolchain.

### Hooks run at most once

1.x: every `run_hooks` call re-ran every hook (its rustdoc suggested
calling it again with `Forced` as a second phase), while the
`ShutdownHook::run` docs promised "at most once". 2.0 keeps the promise:
the first call runs the hooks, every later call returns the report of
that run.

If you called `run_hooks` twice to get a forced second pass, use the
built-in force phase instead:

```rust
use std::time::Duration;
use signal_mod::Coordinator;

let coord = Coordinator::builder()
    .graceful_timeout(Duration::from_secs(5)) // hooks start with your reason until here
    .force_timeout(Duration::from_secs(8))    // then with ShutdownReason::Forced until here
    .build();
# drop(coord);
```

### The graceful budget is enforced; `force_timeout` is used

1.x only checked the budget between hooks, so one slow hook could run
forever, and `force_timeout` was stored but never used. 2.0 runs hooks on
a worker thread and abandons a hook still running at its deadline. Hooks
that had not started when the graceful budget ran out now run in a force
phase (bounded by `force_timeout`) instead of being skipped. With the
defaults (5 s / 10 s), a run returns within about 10 seconds whatever the
hooks do. To restore "no deadline", pass `Duration::MAX` to both.

### Hooks run on a worker thread

Hooks no longer run on the thread that calls `run_hooks`. `ShutdownHook`
was always `Send + Sync + 'static`, but a hook that read thread-local
state set by `main` must now capture that state instead. With the
`tokio` feature the caller's Tokio runtime is entered on the worker
thread, so `tokio::runtime::Handle::current()` keeps working.

### A panicking hook is not counted as completed

1.x counted a panicked hook in `run_hooks`'s return value and in
`Statistics::hooks_completed`. 2.0 reports it as `Panicked`, with its
message, and leaves it out of the completed count.

### `Statistics` is `#[non_exhaustive]` and has new fields

You can still read every field. Code that built a `Statistics` with a
struct literal, or destructured it exhaustively, must use `..`. New
fields: `hooks_panicked`, `hooks_timed_out`, `hooks_skipped`, `hooks`
(per-hook status and timings), `installed`, `active_subsystems`.

### `build()` starts a parked thread when hooks are registered

So that running the hooks does not pay for a thread spawn, a coordinator
with hooks parks one worker thread from `build()` until it runs its
hooks or is dropped. A process that `fork`s after `build()` does not
inherit it; the run in the child starts a fresh worker, so hooks still
run within the budget. Programs that daemonize should still build the
coordinator after forking.

### Back-end selection happens at `install()` time

1.x picked one back-end at compile time and let `tokio` silently win.
2.0 uses Tokio when a Tokio runtime is current and the agnostic back-end
otherwise (if enabled). `install()` outside a runtime with only `tokio`
enabled now returns `Error::OutsideTokioRuntime` instead of panicking.

### Feature changes

| 1.x feature      | 2.0                                                                                 |
| ---------------- | ----------------------------------------------------------------------------------- |
| `tokio`          | unchanged (no longer enables Tokio's `macros` and `time` features)                  |
| `async-std`      | deprecated alias of `agnostic`; no longer depends on async-std (discontinued)        |
| `ctrlc-fallback` | alias of `agnostic`; now covers every signal in the set, not just Ctrl+C              |
| (new) `agnostic` | runtime-agnostic back-end; also reachable as `smol`                                  |

If you depended on `signal-mod`'s `tokio` feature to enable
`tokio/macros` or `tokio/time` for your own code, enable them on your own
`tokio` dependency.

### Dropping the coordinator uninstalls its handlers

1.x left listener tasks running and signals swallowed after the
coordinator was dropped. 2.0 removes them. On Unix, shutdown signals
that were at their default action before signal-mod installed them get
it back (the next one terminates the process); signals that other code
ignored or handled first are left alone, and notify-only signals stay
absorbed. Keep the coordinator alive for as long as you want the
handlers (normally all of `main`); a function that builds a coordinator,
installs it and returns only a token now loses the handlers on return.

### `ShutdownToken::wait` returns a named future

`wait()` is no longer an `async fn`; it returns `ShutdownFuture`, which
is `'static` and works on every executor. `token.wait().await` is
unchanged. It is also available without any runtime feature.

### Signal numbers are per target

`Signal::unix_number()` now returns the compiled target's number (1.x
always returned Linux values: SIGUSR1 = 10 is SIGBUS on macOS). On
non-Unix targets it returns `None` for `User1` / `User2`.

### Windows mapping

`Hangup` now also covers `CTRL_LOGOFF_EVENT`. The agnostic back-end
(and the old `ctrlc-fallback` / `async-std` names) reports Ctrl+Break as
`Quit` and console close as `Terminate` instead of reporting everything
as `Interrupt`.

### `SignalSet`'s `Debug` output

It now lists the signals (`SignalSet {Terminate, Interrupt}`) instead of
the raw bits.

## Deprecations

| Item                                  | Replacement                          | Removal |
| ------------------------------------- | ------------------------------------ | ------- |
| `Error::InvalidState`, `Error::Timeout` | none: never produced               | 3.0     |
| feature `async-std`                   | feature `agnostic` (or `smol`)       | 3.0     |

## New capabilities worth adopting

- `run_hooks_async` from async code; `run_hooks_with_report` and
  `RunReport` for logging what each hook did.
- `ShutdownHook::timeout` / `FnHook::with_timeout` for per-hook bounds.
- `CoordinatorBuilder::force_exit_on_second_signal(true)`.
- `notify_signals` and `Coordinator::subscribe` for reload / log rotation.
- `ShutdownReason::exit_code()` for `std::process::exit`.
- `ShutdownToken::subsystem_guard` and `subsystems_finished` to wait for
  workers; `ShutdownToken::child` for per-connection or per-job scopes.
- `shutdown_signal()` for `with_graceful_shutdown`.
- `Coordinator::simulate_signal` in tests.
- The `tracing` feature for diagnostics.

## Adopting signal-mod from other crates

| Before                                              | After                                                                 |
| --------------------------------------------------- | --------------------------------------------------------------------- |
| `ctrlc::set_handler(\|\| flag.store(true))`         | `let coord = Coordinator::builder().build(); coord.install()?;`       |
| polling a shutdown flag                             | `coord.token().wait().await` / `wait_blocking()`                      |
| `signal_hook::iterator::Signals` for SIGHUP reload  | `notify_signals(..)` and `coord.subscribe()`                          |
| a `Vec<Box<dyn Fn()>>` cleanup list                 | `ShutdownHook`s run by `run_hooks_async`, with deadlines               |
| `tokio::signal::ctrl_c().await`                     | `signal_mod::shutdown_signal()?.await` (also SIGTERM and SIGHUP)      |
| `CancellationToken` passed to tasks                 | `ShutdownToken` (or `cancellation_token()` with `tokio-util`)          |

`tokio::signal::ctrl_c` covers only SIGINT / Ctrl+C. The default
`SignalSet::graceful()` adds SIGTERM (what `kill`, systemd, Docker and
Kubernetes send) and SIGHUP:

```rust,no_run
# #[tokio::main]
# async fn main() -> signal_mod::Result<()> {
let shutdown = signal_mod::shutdown_signal()?;
let token = shutdown.token();
shutdown.await;
eprintln!("stopping: {:?}", token.reason());
# Ok(())
# }
```

## Coexisting with other signal handlers

- **Other code using `tokio::signal` or `signal-hook` on Unix:**
  registrations compose; both see the signal while a coordinator is
  installed. If the other code registered its handler **before**
  signal-mod first installed that signal, signal-mod leaves the signal to
  it after the last coordinator is gone. If it registered **after**, and
  the signal is in a coordinator's shutdown set, the restored default
  action terminates the process on the next delivery once the last
  coordinator is gone: keep the coordinator installed for the life of
  the process in that case.
- **A framework that owns signals:** configure the coordinator with the
  sets you want (`signals(..)`, `notify_signals(..)`), do not call
  `install()`, and forward each signal the framework receives with
  `coord.simulate_signal(sig)`; it returns `false` for a signal in
  neither set. Alternatively call
  `coord.trigger().trigger(ShutdownReason::Signal(sig))`, which needs no
  configuration.
- **Your own `tokio::signal` listeners on Windows:** with signal-mod's
  `tokio` feature (the default), both signal-mod back-ends use Tokio's
  console-event streams, so your listeners and every coordinator see
  each event, as in 1.x. With `default-features = false` and only
  `agnostic`, signal-mod registers its own console handler; Windows
  calls the most recently registered handler first and stops at the
  first that claims the event, so your earlier Tokio listeners then
  stop seeing the events signal-mod covers. Enable signal-mod's `tokio`
  feature if your program uses `tokio::signal` itself.
- **Other Windows console handlers (the `ctrlc` crate, another
  library):** the same last-registered-first rule applies between
  signal-mod and them. Prefer one owner.

## Versioning policy

`signal-mod` follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html):
patch releases fix bugs, minor releases add API or opt-in features,
and anything that breaks code or behavior, including an MSRV raise, is a
major release with an entry in this guide. Deprecated items survive at
least one major version.
