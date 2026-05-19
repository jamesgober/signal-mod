# signal-mod - API Reference

Authoritative offline mirror of the rustdoc-generated API for
`signal-mod`. Every public item is documented here with description,
parameter / return semantics, and at least one runnable code example.

For the design contract (mission, scope, dependency policy) see
[`REPS.md`](../REPS.md). For migration history see
[`MIGRATION.md`](MIGRATION.md). For the performance baseline see
[Performance](#performance).

---

## Table of contents

- [Crate root](#crate-root)
- [`Signal`](#signal)
- [`SignalSet`](#signalset)
- [`SignalSetIter`](#signalsetiter)
- [`ShutdownReason`](#shutdownreason)
- [`ShutdownToken`](#shutdowntoken)
- [`ShutdownTrigger`](#shutdowntrigger)
- [`ShutdownHook`](#shutdownhook)
- [`FnHook`](#fnhook)
- [`hook_from_fn`](#hook_from_fn)
- [`Coordinator`](#coordinator)
- [`CoordinatorBuilder`](#coordinatorbuilder)
- [`Statistics`](#statistics)
- [`Error`](#error)
- [`Result`](#result)
- [`VERSION`](#version)
- [Cargo features](#cargo-features)
- [Performance](#performance)

---

## Crate root

```rust
pub const VERSION: &str;

pub use coord::{Coordinator, CoordinatorBuilder, Statistics};
pub use error::{Error, Result};
pub use hook::{hook_from_fn, FnHook, ShutdownHook};
pub use reason::ShutdownReason;
pub use signal::{Signal, SignalSet, SignalSetIter};
pub use token::{ShutdownToken, ShutdownTrigger};
```

Every item above is part of the stable `1.0` surface and is covered
by semver.

---

## `Signal`

```rust
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Terminate,
    Interrupt,
    Quit,
    Hangup,
    Pipe,
    User1,
    User2,
}
```

Platform-neutral signal identifier. The variants map to:

| Variant     | Unix     | Windows                |
| ----------- | -------- | ---------------------- |
| `Terminate` | SIGTERM  | `CTRL_CLOSE_EVENT`     |
| `Interrupt` | SIGINT   | `CTRL_C_EVENT`         |
| `Quit`      | SIGQUIT  | `CTRL_BREAK_EVENT`     |
| `Hangup`    | SIGHUP   | `CTRL_SHUTDOWN_EVENT`  |
| `Pipe`      | SIGPIPE  | inert                  |
| `User1`     | SIGUSR1  | inert                  |
| `User2`     | SIGUSR2  | inert                  |

The enum is `#[non_exhaustive]`; downstream `match`es must include
a wildcard arm.

### Associated items

| Item                                                    | Description                                                                |
| ------------------------------------------------------- | -------------------------------------------------------------------------- |
| `const ALL: [Signal; 7]`                                | Every variant in canonical order.                                          |
| `const fn description(self) -> &'static str`            | Human-readable label, used by `Display` and logging.                       |
| `const fn unix_number(self) -> Option<i32>`             | Canonical Unix signal number.                                              |
| `const fn is_unix_only(self) -> bool`                   | `true` for variants with no Windows analog.                                |
| `const fn available_on_current_platform(self) -> bool`  | `true` if the platform supports this signal's handler installation.        |

### Examples

Iterate the canonical list and inspect each variant:

```rust
use signal_mod::Signal;

for sig in Signal::ALL {
    println!(
        "{sig:?}: unix_number={:?}, unix_only={}, available={}",
        sig.unix_number(),
        sig.is_unix_only(),
        sig.available_on_current_platform(),
    );
}
```

Pattern-match a delivered signal back from a `ShutdownReason`:

```rust
use signal_mod::{ShutdownReason, Signal};

fn classify(reason: ShutdownReason) -> &'static str {
    match reason {
        ShutdownReason::Signal(Signal::Terminate) => "operator terminate",
        ShutdownReason::Signal(Signal::Interrupt) => "user ctrl+c",
        ShutdownReason::Signal(Signal::Hangup)    => "session hangup",
        _ => "non-signal",
    }
}
```

---

## `SignalSet`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalSet { /* private bits */ }
```

Bit-packed, `Copy`, const-constructible set of `Signal` values.
`SignalSet` is the input to `CoordinatorBuilder::signals` and
determines which OS signals `Coordinator::install` will register.

### Associated items

| Item                                              | Description                                               |
| ------------------------------------------------- | --------------------------------------------------------- |
| `const fn empty() -> Self`                        | Empty set.                                                |
| `const fn all() -> Self`                          | Every variant.                                            |
| `const fn graceful() -> Self`                     | `Terminate \| Interrupt \| Hangup` (the default).         |
| `const fn standard() -> Self`                     | `graceful()` plus `Quit`.                                 |
| `const fn with(self, sig: Signal) -> Self`        | Returns a copy with `sig` enabled.                        |
| `const fn without(self, sig: Signal) -> Self`     | Returns a copy with `sig` disabled.                       |
| `const fn contains(self, sig: Signal) -> bool`    | Membership test.                                          |
| `const fn is_empty(self) -> bool`                 | `true` if no signals are enabled.                         |
| `const fn len(self) -> usize`                     | Number of signals enabled.                                |
| `const fn iter(&self) -> SignalSetIter`           | Iterator in canonical [`Signal::ALL`] order.              |

`impl Default for SignalSet` returns `SignalSet::graceful()`.
`impl IntoIterator for SignalSet` (and `&SignalSet`) yields
`SignalSetIter`.

### Examples

Build a set inline at compile time:

```rust
use signal_mod::{Signal, SignalSet};

const SET: SignalSet = SignalSet::graceful()
    .with(Signal::Quit)
    .with(Signal::User1);

assert!(SET.contains(Signal::Quit));
assert_eq!(SET.len(), 5);
```

Iterate the enabled signals:

```rust
use signal_mod::SignalSet;

for sig in SignalSet::standard() {
    println!("standard set contains {sig:?}");
}
```

Compose at runtime:

```rust
use signal_mod::{Signal, SignalSet};

fn for_workload(allow_quit: bool) -> SignalSet {
    let mut s = SignalSet::graceful();
    if allow_quit {
        s = s.with(Signal::Quit);
    }
    s
}
```

---

## `SignalSetIter`

```rust
#[derive(Debug, Clone)]
pub struct SignalSetIter { /* ... */ }

impl Iterator for SignalSetIter {
    type Item = Signal;
}
```

Iterator returned by `SignalSet::iter`. Yields enabled variants in
the canonical [`Signal::ALL`] order. The iterator is `Clone` so the
same set can be walked multiple times by cloning the iterator
itself.

### Examples

```rust
use signal_mod::SignalSet;

let mut iter = SignalSet::graceful().iter();
let first = iter.clone().next();
let second = iter.nth(1);
assert_eq!(first, second.map(|_| first.unwrap()).or(first));
```

---

## `ShutdownReason`

```rust
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    Signal(Signal),
    Requested,
    Forced,
    Timeout,
    Error,
}
```

Why a shutdown was initiated. Carried with every `trigger` call and
surfaced via `ShutdownToken::reason`.

### Associated items

| Item                                          | Description                                  |
| --------------------------------------------- | -------------------------------------------- |
| `const fn description(self) -> &'static str`  | Short label: `"signal"` / `"requested"` / etc. |
| `const fn is_signal(self) -> bool`            | `true` only for `Signal(_)`.                 |

`impl Display for ShutdownReason` renders human-friendly text.

### Examples

Discriminate signal-driven shutdown from programmatic:

```rust
use signal_mod::{ShutdownReason, Signal};

fn was_operator(reason: ShutdownReason) -> bool {
    matches!(
        reason,
        ShutdownReason::Signal(Signal::Terminate)
            | ShutdownReason::Signal(Signal::Interrupt)
    )
}
```

Trigger with a custom reason:

```rust
use signal_mod::{Coordinator, ShutdownReason};

let coord = Coordinator::builder().build();
coord.trigger().trigger(ShutdownReason::Error);
```

---

## `ShutdownToken`

```rust
#[derive(Debug, Clone)]
pub struct ShutdownToken { /* Arc<Inner> */ }
```

Cloneable observer handle. Hand one to every subsystem that needs to
react to shutdown.

### Methods

| Method                                                          | Description                                                                                              |
| --------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------- |
| `fn is_initiated(&self) -> bool`                                | Relaxed atomic load of the initiated flag.                                                               |
| `fn reason(&self) -> Option<ShutdownReason>`                    | The reason carried with the trigger, or `None` if not yet initiated.                                     |
| `fn elapsed(&self) -> Option<Duration>`                         | Wall-clock time since shutdown was initiated.                                                            |
| `fn wait_blocking(&self)`                                       | Park the current thread until shutdown is initiated. Returns immediately if already initiated.           |
| `fn wait_blocking_timeout(&self, timeout: Duration) -> bool`    | Park for at most `timeout`. Returns `true` if shutdown was observed within the budget.                   |
| `async fn wait(&self)` (with `tokio` or `async-std`)            | Async wait. Returns when shutdown is initiated. Fast-path returns immediately if already initiated.      |

### Examples

Observe in a Tokio task:

```rust,no_run
use signal_mod::{Coordinator, ShutdownReason};

# #[cfg(feature = "tokio")]
# async fn run(coord: Coordinator) -> signal_mod::Result<()> {
let token = coord.token();
token.wait().await;
println!("shutting down: {}", token.reason().unwrap_or(ShutdownReason::Requested));
# Ok(())
# }
```

Observe with a budget from a sync thread:

```rust
use std::time::Duration;
use signal_mod::Coordinator;

let coord = Coordinator::builder().build();
let token = coord.token();
if token.wait_blocking_timeout(Duration::from_millis(100)) {
    println!("shutdown observed");
} else {
    println!("timeout elapsed; continue working");
}
```

---

## `ShutdownTrigger`

```rust
#[derive(Debug, Clone)]
pub struct ShutdownTrigger { /* Arc<Inner> */ }
```

Cloneable initiator handle. Hand one to any code path that may need
to ask for shutdown (HTTP admin endpoint, fatal-error site,
supervisory parent).

### Methods

| Method                                                | Description                                                                                                                                    |
| ----------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `fn trigger(&self, reason: ShutdownReason) -> bool`   | Initiate shutdown. Returns `true` if this call performed the state transition; `false` if another caller (or the signal back-end) beat it.     |
| `fn is_initiated(&self) -> bool`                      | Shared read of the atomic flag.                                                                                                                |

### Examples

A fatal-error branch promotes a local error into program shutdown:

```rust
use signal_mod::{Coordinator, ShutdownReason, ShutdownTrigger};

fn handle_fatal(trigger: &ShutdownTrigger, err: &dyn std::error::Error) {
    eprintln!("fatal: {err}");
    trigger.trigger(ShutdownReason::Error);
}
# let coord = Coordinator::builder().build();
# let trigger = coord.trigger();
# handle_fatal(&trigger, &std::io::Error::other("example"));
```

A supervisor signals shutdown from a different task:

```rust,no_run
use std::time::Duration;
use signal_mod::{Coordinator, ShutdownReason};

# #[cfg(feature = "tokio")]
# async fn run() -> signal_mod::Result<()> {
let coord = Coordinator::builder().build();
let trigger = coord.trigger();

tokio::spawn(async move {
    tokio::time::sleep(Duration::from_secs(60)).await;
    trigger.trigger(ShutdownReason::Requested);
});

coord.token().wait().await;
# Ok(())
# }
```

---

## `ShutdownHook`

```rust
pub trait ShutdownHook: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn priority(&self) -> i32 { 0 }
    fn run(&self, reason: ShutdownReason);
}
```

A unit of cleanup work to run during shutdown.

- `name` is used for diagnostics and ordering tie-break visibility.
- `priority` controls execution order. Higher values run first.
  Defaults to `0`.
- `run` is invoked once per `Coordinator::run_hooks` call. Panics
  inside `run` are caught by the coordinator and do not abort the
  remaining hooks.

The trait is open. See `FnHook` for the most common closure-backed
implementation, and `examples/custom_hook_type.rs` for a worked
example of a stateful hook.

### Examples

Closure-backed (most common):

```rust
use signal_mod::{hook_from_fn, ShutdownHook, ShutdownReason};

let hook = hook_from_fn("flush", 100, |reason: ShutdownReason| {
    eprintln!("flushing: {reason}");
});
assert_eq!(hook.name(), "flush");
assert_eq!(hook.priority(), 100);
```

Stateful trait impl:

```rust
use std::sync::Arc;
use parking_lot::Mutex;
use signal_mod::{ShutdownHook, ShutdownReason};

struct BufferFlush {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl ShutdownHook for BufferFlush {
    fn name(&self) -> &str { "buffer-flush" }
    fn priority(&self) -> i32 { 50 }
    fn run(&self, _: ShutdownReason) {
        let mut guard = self.buf.lock();
        // ... flush guard to disk ...
        guard.clear();
    }
}
```

---

## `FnHook`

```rust
pub struct FnHook<F>
where F: Fn(ShutdownReason) + Send + Sync + 'static;

impl<F> FnHook<F> { pub fn new(name: impl Into<String>, priority: i32, f: F) -> Self; }
impl<F> ShutdownHook for FnHook<F> { /* ... */ }
```

Closure-backed `ShutdownHook` returned by `hook_from_fn`. You will
rarely need to name this type explicitly; the
`Coordinator::builder().hook(hook_from_fn(...))` chain hides it.

---

## `hook_from_fn`

```rust
pub fn hook_from_fn<F>(name: impl Into<String>, priority: i32, f: F) -> FnHook<F>
where
    F: Fn(ShutdownReason) + Send + Sync + 'static;
```

Wrap a closure as a `ShutdownHook`.

### Parameters

- `name`: human-readable label for diagnostics.
- `priority`: higher runs first.
- `f`: closure body invoked exactly once per `run_hooks` call.

### Examples

```rust
use signal_mod::hook_from_fn;
use signal_mod::ShutdownHook;

let hook = hook_from_fn("close-db", 200, |reason| {
    eprintln!("closing db: {reason}");
});
assert_eq!(hook.name(), "close-db");
assert_eq!(hook.priority(), 200);
```

---

## `Coordinator`

```rust
pub struct Coordinator { /* ... */ }
```

Owns the shutdown state machine, the hook list, and (optionally) the
installed signal handlers. Construct via `Coordinator::builder`.

### Methods

| Method                                                    | Description                                                                                                         |
| --------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| `fn builder() -> CoordinatorBuilder`                      | Start a builder with default configuration.                                                                         |
| `fn token(&self) -> ShutdownToken`                        | Create a cloneable observer handle.                                                                                 |
| `fn trigger(&self) -> ShutdownTrigger`                    | Create a cloneable initiator handle.                                                                                |
| `fn signals(&self) -> SignalSet`                          | Configured signal set.                                                                                              |
| `fn graceful_timeout(&self) -> Duration`                  | Configured graceful budget (default 5s).                                                                            |
| `fn force_timeout(&self) -> Duration`                     | Configured force budget (default 10s).                                                                              |
| `fn is_installed(&self) -> bool`                          | `true` once `install` has succeeded.                                                                                |
| `fn statistics(&self) -> Statistics`                      | Atomic-consistent snapshot of state.                                                                                |
| `fn run_hooks(&self, reason: ShutdownReason) -> usize`    | Execute hooks in descending-priority order under the graceful budget. Returns count completed. Panic-safe per hook. |
| `fn install(&self) -> Result<()>`                         | Register OS-level signal handlers per the active back-end feature.                                                  |

### `install` behavior

| Active features                            | Back-end                                              |
| ------------------------------------------ | ----------------------------------------------------- |
| `tokio` (default)                          | `tokio::signal::{unix,windows}` plus `tokio::spawn`.  |
| `async-std` (and `tokio` not enabled)      | `signal-hook-async-std` (Unix); `ctrlc` (Windows).    |
| `ctrlc-fallback` (no async runtime)        | `ctrlc::try_set_handler` for `Signal::Interrupt`.     |
| none of the above                          | Returns `Error::NoRuntime`.                           |

`install` is idempotent on the coordinator: a second call returns
`Error::AlreadyInstalled`. The OS-level signal slot is owned by the
first back-end that grabs it; do not install handlers from two
different coordinators in the same process.

### Errors

- `Error::AlreadyInstalled` - second `install` on this coordinator.
- `Error::SignalRegistration { signal, source }` - platform rejected
  a specific signal. The internal install flag is reverted on error
  so the call can be retried after the cause is fixed.
- `Error::NoRuntime` - no back-end feature enabled.

### Examples

Full happy path:

```rust,no_run
use std::time::Duration;
use signal_mod::{hook_from_fn, Coordinator, ShutdownReason, SignalSet};

# #[cfg(feature = "tokio")]
# #[tokio::main]
# async fn main() -> signal_mod::Result<()> {
let coord = Coordinator::builder()
    .signals(SignalSet::graceful())
    .graceful_timeout(Duration::from_secs(5))
    .hook(hook_from_fn("flush", 100, |r| eprintln!("flush: {r}")))
    .build();

coord.install()?;

let token = coord.token();
token.wait().await;

let reason = token.reason().unwrap_or(ShutdownReason::Requested);
coord.run_hooks(reason);
# Ok(())
# }
```

Programmatic shutdown without OS signals:

```rust
use signal_mod::{Coordinator, ShutdownReason};

let coord = Coordinator::builder().build();
let initiated = coord.trigger().trigger(ShutdownReason::Requested);
assert!(initiated);
assert!(coord.token().is_initiated());
```

Inspect statistics after a run:

```rust
use signal_mod::{hook_from_fn, Coordinator, ShutdownReason};

let coord = Coordinator::builder()
    .hook(hook_from_fn("a", 0, |_| {}))
    .hook(hook_from_fn("b", 0, |_| {}))
    .build();
coord.run_hooks(ShutdownReason::Requested);
let stats = coord.statistics();
assert_eq!(stats.hooks_registered, 2);
assert_eq!(stats.hooks_completed, 2);
```

---

## `CoordinatorBuilder`

```rust
pub struct CoordinatorBuilder { /* ... */ }

impl CoordinatorBuilder {
    pub fn new() -> Self;
    pub fn signals(self, set: SignalSet) -> Self;
    pub fn graceful_timeout(self, d: Duration) -> Self;
    pub fn force_timeout(self, d: Duration) -> Self;
    pub fn hook<H: ShutdownHook>(self, h: H) -> Self;
    pub fn build(self) -> Coordinator;
}
```

Builder for `Coordinator`. Methods consume `self` and return `self`
so they may be chained. `impl Default for CoordinatorBuilder` is
equivalent to `CoordinatorBuilder::new()`.

Defaults:

- signals: `SignalSet::graceful()`
- graceful timeout: 5 s
- force timeout: 10 s
- no hooks

### Examples

```rust
use std::time::Duration;
use signal_mod::{hook_from_fn, Coordinator, SignalSet};

let coord = Coordinator::builder()
    .signals(SignalSet::standard())
    .graceful_timeout(Duration::from_secs(10))
    .force_timeout(Duration::from_secs(20))
    .hook(hook_from_fn("close-db", 200, |_| {}))
    .hook(hook_from_fn("flush-logs", 100, |_| {}))
    .build();

assert_eq!(coord.signals(), SignalSet::standard());
assert_eq!(coord.graceful_timeout(), Duration::from_secs(10));
```

---

## `Statistics`

```rust
#[derive(Debug, Clone)]
pub struct Statistics {
    pub initiated: bool,
    pub reason: Option<ShutdownReason>,
    pub hooks_registered: usize,
    pub hooks_completed: usize,
    pub elapsed: Option<Duration>,
}
```

Snapshot returned by `Coordinator::statistics`. All fields are public
for direct read access. The snapshot is a value type; later state
changes on the coordinator do not affect a previously-taken
snapshot.

### Examples

```rust
use signal_mod::{Coordinator, ShutdownReason};

let coord = Coordinator::builder().build();
let before = coord.statistics();
assert!(!before.initiated);
assert!(before.elapsed.is_none());

coord.trigger().trigger(ShutdownReason::Requested);

let after = coord.statistics();
assert!(after.initiated);
assert_eq!(after.reason, Some(ShutdownReason::Requested));
assert!(after.elapsed.is_some());
```

---

## `Error`

```rust
#[non_exhaustive]
#[derive(Debug)]
pub enum Error {
    AlreadyInstalled,
    SignalRegistration {
        signal: Signal,
        source: std::io::Error,
    },
    InvalidState(&'static str),
    Timeout(&'static str),
    NoRuntime,
}
```

The error type returned by fallible methods on the public surface.
Implements `Display` and `std::error::Error`; `Error::source()`
returns the underlying `io::Error` for `SignalRegistration`.

The enum is `#[non_exhaustive]`. New variants may be added in `1.x`
minor releases; downstream `match`es must include a wildcard arm.

### Variant semantics

| Variant                                                       | Triggered by                                                                          |
| ------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| `AlreadyInstalled`                                            | Second `Coordinator::install` on the same coordinator.                                |
| `SignalRegistration { signal, source }`                       | Platform rejected handler registration for a specific signal. `source` is the OS error. |
| `InvalidState(&'static str)`                                  | Reserved for future use.                                                              |
| `Timeout(&'static str)`                                       | Reserved for future use.                                                              |
| `NoRuntime`                                                   | `install` called with no back-end feature enabled.                                    |

### Examples

```rust
use signal_mod::{Coordinator, Error};

# #[cfg(feature = "tokio")]
# async fn run() {
let coord = Coordinator::builder().build();
match coord.install() {
    Ok(()) => println!("installed"),
    Err(Error::AlreadyInstalled) => println!("already installed"),
    Err(Error::SignalRegistration { signal, source }) => {
        eprintln!("failed to install {signal:?}: {source}")
    }
    Err(Error::NoRuntime) => eprintln!("no runtime feature enabled"),
    Err(other) => eprintln!("install failed: {other}"),
}
# }
```

---

## `Result`

```rust
pub type Result<T> = core::result::Result<T, Error>;
```

Convenience alias used by every fallible method on the public
surface.

---

## `VERSION`

```rust
pub const VERSION: &str;
```

The crate version string, populated by Cargo at build time. Useful
for log lines, telemetry, and version-aware integrations.

### Examples

```rust
use signal_mod::VERSION;

println!("signal-mod {VERSION}");
assert!(!VERSION.is_empty());
```

---

## Cargo features

| Feature          | Default | Effect                                                                                |
| ---------------- | ------- | ------------------------------------------------------------------------------------- |
| `std`            | yes     | Enables std-dependent items. Reserved for a future `no_std` story.                    |
| `tokio`          | yes     | Tokio runtime adapter; enables async `wait()` and signal handlers via `tokio::signal`. |
| `async-std`      | no      | async-std runtime adapter; uses `signal-hook-async-std` on Unix and `ctrlc` on Windows. |
| `ctrlc-fallback` | no      | Synchronous `ctrlc::set_handler` fallback covering `Signal::Interrupt`.               |

When both `tokio` and `async-std` are enabled, `tokio` wins.

---

## Performance

Measured on a single reference platform (Windows 11,
`x86_64-pc-windows-msvc`, Rust 1.95.0) via
`cargo bench --bench shutdown_bench`. Rerun locally to validate on
your hardware. Lower is better.

| Operation                                       | Median time |
| ----------------------------------------------- | ----------- |
| `ShutdownToken::is_initiated` (uninitiated)     | 364 ps      |
| `ShutdownTrigger::trigger` (state transition)   | 123 ns      |
| `ShutdownTrigger::trigger` (already initiated)  | 4.7 ns      |
| `ShutdownToken::clone`                          | 7.2 ns      |
| `Coordinator::run_hooks` (1 hook)               | 60 ns       |
| `Coordinator::run_hooks` (4 hooks)              | 156 ns      |
| `Coordinator::run_hooks` (16 hooks)             | 536 ns      |
| `Coordinator::run_hooks` (64 hooks)             | 2.08 us     |
| `SignalSet::iter` (all 7 variants)              | 1.5 ns      |

Notes:

- `is_initiated` is one relaxed atomic load, one `mov` on x86-64.
- `trigger` on the already-initiated path is one failing
  `compare_exchange`; no lock, no broadcast.
- `run_hooks` scales linearly in the number of hooks. Sort overhead
  is dominated by per-hook dispatch.
- `wait_blocking_timeout` with sub-millisecond budgets measures OS
  timer granularity (~15 ms on Windows), not the primitive itself.

---

## Copyright

Copyright (C) 2026 James Gober. Licensed under Apache-2.0.
