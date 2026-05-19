# mod-signal - API Reference

> Authoritative offline mirror of the rustdoc-generated API for
> `mod-signal`. Cross-reference [`REPS.md`](../REPS.md) section 4 for
> the design contract.

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

## `Signal`

Cross-platform signal identifier.

| Variant     | Unix     | Windows                |
| ----------- | -------- | ---------------------- |
| `Terminate` | SIGTERM  | `CTRL_CLOSE_EVENT`     |
| `Interrupt` | SIGINT   | `CTRL_C_EVENT`         |
| `Quit`      | SIGQUIT  | `CTRL_BREAK_EVENT`     |
| `Hangup`    | SIGHUP   | `CTRL_SHUTDOWN_EVENT`  |
| `Pipe`      | SIGPIPE  | inert                  |
| `User1`     | SIGUSR1  | inert                  |
| `User2`     | SIGUSR2  | inert                  |

Helpers: `description`, `unix_number`, `is_unix_only`,
`available_on_current_platform`. The `ALL` const exposes the variants
in canonical order.

## `SignalSet`

Bit-packed, `Copy`, const-constructible. Named constructors:

- `empty()` / `all()`
- `graceful()` - `Terminate | Interrupt | Hangup` (default)
- `standard()` - `graceful() | Quit`

Operators: `with`, `without`, `contains`, `is_empty`, `len`, `iter`.
Implements `IntoIterator` for `SignalSet` and `&SignalSet`.

## `ShutdownReason`

Reason carried with a shutdown trigger.

Variants:

- `Signal(Signal)`
- `Requested`
- `Forced`
- `Timeout`
- `Error`

Helpers: `description`, `is_signal`, `Display`.

## `ShutdownToken` / `ShutdownTrigger`

Cloneable observer / initiator handles. The token surfaces:

| Method                                             | Available with        |
| -------------------------------------------------- | --------------------- |
| `is_initiated`                                     | always                |
| `reason`                                           | always                |
| `elapsed`                                          | always                |
| `wait_blocking`                                    | always                |
| `wait_blocking_timeout`                            | always                |
| `wait().await`                                     | `tokio` or `async-std`|

The trigger surfaces:

| Method         | Returns | Behavior                                  |
| -------------- | ------- | ----------------------------------------- |
| `trigger`      | `bool`  | `true` if this call performed the transition |
| `is_initiated` | `bool`  | shared read of the atomic                 |

## `ShutdownHook`

```rust
pub trait ShutdownHook: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn priority(&self) -> i32 { 0 }
    fn run(&self, reason: ShutdownReason);
}

pub fn hook_from_fn<F>(name: impl Into<String>, priority: i32, f: F) -> FnHook<F>
where
    F: Fn(ShutdownReason) + Send + Sync + 'static;
```

## `Coordinator`

```rust
impl Coordinator {
    pub fn builder() -> CoordinatorBuilder;
    pub fn token(&self) -> ShutdownToken;
    pub fn trigger(&self) -> ShutdownTrigger;
    pub fn signals(&self) -> SignalSet;
    pub fn graceful_timeout(&self) -> Duration;
    pub fn force_timeout(&self) -> Duration;
    pub fn is_installed(&self) -> bool;
    pub fn statistics(&self) -> Statistics;
    pub fn run_hooks(&self, reason: ShutdownReason) -> usize;
    pub fn install(&self) -> Result<()>;
}
```

`install` dispatches by feature:

1. `tokio` (default) - spawns `tokio` tasks. Caller must be inside a
   tokio runtime context.
2. `async-std` - spawns `async-std` tasks. Caller must be inside an
   async-std runtime context. Windows path uses `ctrlc` (only
   `Signal::Interrupt` is covered).
3. `ctrlc-fallback` - synchronous `ctrlc` handler (`Signal::Interrupt`
   only).
4. None of the above - `install` returns `Error::NoRuntime`.

`run_hooks` sorts hooks descending by priority, honors the graceful
timeout budget, and returns the number of hooks that completed.

## `CoordinatorBuilder`

```rust
impl CoordinatorBuilder {
    pub fn new() -> Self;
    pub fn signals(self, set: SignalSet) -> Self;
    pub fn graceful_timeout(self, d: Duration) -> Self;
    pub fn force_timeout(self, d: Duration) -> Self;
    pub fn hook<H: ShutdownHook>(self, h: H) -> Self;
    pub fn build(self) -> Coordinator;
}
```

Defaults: `SignalSet::graceful()`, 5s graceful, 10s force, no hooks.

## `Statistics`

Public snapshot type returned by `Coordinator::statistics`:

```rust
pub struct Statistics {
    pub initiated: bool,
    pub reason: Option<ShutdownReason>,
    pub hooks_registered: usize,
    pub hooks_completed: usize,
    pub elapsed: Option<Duration>,
}
```

## `Error`

`#[non_exhaustive]` enum:

- `AlreadyInstalled` - `install` called twice.
- `SignalRegistration { signal, source }` - platform rejected
  registration.
- `InvalidState(&'static str)` - reserved for future use.
- `Timeout(&'static str)` - reserved for future use.
- `NoRuntime` - no back-end feature available for `install`.

`impl Display + std::error::Error`. `source()` returns the underlying
`io::Error` for `SignalRegistration`.

## Performance

Measured on a single reference platform (Windows 11,
`x86_64-pc-windows-msvc`, Rust 1.95.0, `cargo bench --bench
shutdown_bench`). Rerun locally to validate on yours. Lower is
better.

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

- `is_initiated` is a single relaxed atomic load and compiles to one
  `mov` on x86-64. Sub-nanosecond timing reflects the throughput of
  that one instruction.
- `trigger` on the already-initiated path is the failing
  `compare_exchange` plus the redundant-trigger branch; no lock is
  acquired and no broadcast is sent.
- `run_hooks` scales linearly in the number of hooks. The sort runs
  once per call and is `O(n log n)` but dominated by per-hook
  dispatch at these sizes.
- `wait_blocking_timeout` with a very small timeout (1 microsecond)
  measures Windows timer granularity (15 ms minimum), not the
  primitive. Realistic timeouts (>= 1 ms on Linux, >= 16 ms on
  Windows) behave as expected.

## Cargo features

| Feature          | Default | Effect                                                 |
| ---------------- | ------- | ------------------------------------------------------ |
| `std`            | yes     | Enables std-dependent items (always on currently).     |
| `tokio`          | yes     | Spawns signal listeners on the tokio runtime.          |
| `async-std`      | no      | Spawns signal listeners on async-std.                  |
| `ctrlc-fallback` | no      | Adds synchronous `ctrlc` fallback for `install`.       |

The combination `tokio` + `async-std` resolves to the `tokio` path
(matches `proc-daemon` convention).

## Copyright

Copyright (C) 2026 James Gober. Licensed under Apache-2.0.
