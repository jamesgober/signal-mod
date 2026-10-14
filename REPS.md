# signal-mod - Project Specification (REPS)

> Authoritative specification for the public API surface and design
> contract of `signal-mod`.

## 1. Identity

- **Crate name:** `signal-mod`
- **Author:** James Gober <me@jamesgober.com>
- **Repository:** https://github.com/jamesgober/signal-mod
- **License:** Apache-2.0
- **MSRV:** 1.85 (edition 2024)

## 2. Mission

Unified OS signal handling for Rust. SIGTERM / SIGINT / SIGHUP /
SIGQUIT / SIGPIPE / SIGUSR1 / SIGUSR2 and the Windows console control
events through one API. Graceful-shutdown orchestration with priority
ordering, an enforced graceful-then-force deadline, run-once hooks and a
report of what each hook did. A small, runtime-agnostic substrate that
heavier frameworks (such as `proc-daemon`) can build on.

## 3. Scope

`signal-mod` provides:

1. A cross-platform `Signal` enum with per-target signal numbers, and a
   const bit-set `SignalSet`.
2. A `Coordinator` that installs signal listeners (Tokio or
   runtime-agnostic back-end), owns the shutdown state, fans signals out
   to observers and subscribers, and runs priority-ordered shutdown hooks
   under an enforced deadline ladder.
3. Cloneable `ShutdownToken` (observer) and `ShutdownTrigger`
   (initiator) handles, with runtime-agnostic waiting, child scopes and
   subsystem guards.
4. Subscriptions for signals that do not mean "stop" (reload, log
   rotation).

Out of scope items are listed in section 12.

## 4. Public API

The public surface of `signal-mod 2.x.y`. Every item is re-exported from
the crate root and covered by semantic versioning (section 8).

### 4.1 Signals

```rust,ignore
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal { Terminate, Interrupt, Quit, Hangup, Pipe, User1, User2 }

impl Signal {
    pub const ALL: [Signal; 7];
    pub const fn description(self) -> &'static str;
    pub const fn unix_number(self) -> Option<i32>;       // per compiled target
    pub const fn from_unix_number(n: i32) -> Option<Signal>;
    pub const fn exit_code(self) -> i32;                 // 128 + number
    pub const fn is_unix_only(self) -> bool;
    pub const fn available_on_current_platform(self) -> bool;
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)] // Debug lists the signals
pub struct SignalSet { /* u16 */ }

impl SignalSet {
    pub const fn empty() -> Self;
    pub const fn all() -> Self;
    pub const fn graceful() -> Self;   // Terminate | Interrupt | Hangup (also Default)
    pub const fn standard() -> Self;   // graceful | Quit
    pub const fn with(self, sig: Signal) -> Self;
    pub const fn without(self, sig: Signal) -> Self;
    pub const fn union(self, other: Self) -> Self;
    pub const fn intersection(self, other: Self) -> Self;
    pub const fn difference(self, other: Self) -> Self;
    pub const fn contains(self, sig: Signal) -> bool;
    pub const fn is_empty(self) -> bool;
    pub const fn len(self) -> usize;
    pub const fn available_on_current_platform(self) -> Self;
    pub const fn iter(&self) -> SignalSetIter;
}
// BitOr / BitAnd / Sub (+ *Assign) with SignalSet or Signal, From<Signal>,
// FromIterator<Signal>, Extend<Signal>, IntoIterator (by value and by ref).

pub struct SignalSetIter; // ExactSizeIterator + FusedIterator, canonical order
```

### 4.2 Shutdown reason

```rust,ignore
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShutdownReason { Signal(Signal), Requested, Forced, Timeout, Error }

impl ShutdownReason {
    pub const fn description(self) -> &'static str;
    pub const fn is_signal(self) -> bool;
    pub const fn signal(self) -> Option<Signal>;
    pub const fn exit_code(self) -> i32;
}
```

### 4.3 Handles

```rust,ignore
#[derive(Debug, Clone)]
pub struct ShutdownToken;

impl ShutdownToken {
    pub fn is_initiated(&self) -> bool;
    pub fn reason(&self) -> Option<ShutdownReason>;
    pub fn elapsed(&self) -> Option<Duration>;
    pub fn wait(&self) -> ShutdownFuture;
    pub fn cancelled(&self) -> ShutdownFuture;
    pub fn wait_blocking(&self);
    pub fn wait_blocking_timeout(&self, timeout: Duration) -> bool;
    pub fn child(&self) -> (ShutdownTrigger, ShutdownToken);
    pub fn subsystem_guard(&self) -> SubsystemGuard;
    pub fn active_subsystems(&self) -> usize;
    pub fn subsystems_finished(&self) -> SubsystemsFinished;
    pub fn wait_subsystems_blocking(&self, timeout: Duration) -> bool;
    #[cfg(feature = "tokio-util")]
    pub fn cancellation_token(&self) -> tokio_util::sync::CancellationToken;
}
// IntoFuture for ShutdownToken and &ShutdownToken;
// From<&ShutdownToken> for CancellationToken (feature `tokio-util`).

pub struct ShutdownFuture;      // Future<Output = ()> + Send + 'static
pub struct SubsystemGuard;      // decrements on drop
pub struct SubsystemsFinished;  // Future<Output = ()> + Send + 'static

#[derive(Debug, Clone)]
pub struct ShutdownTrigger;

impl ShutdownTrigger {
    pub fn trigger(&self, reason: ShutdownReason) -> bool;
    pub fn is_initiated(&self) -> bool;
    pub fn token(&self) -> ShutdownToken;
}
```

### 4.4 Hooks and reports

```rust,ignore
pub trait ShutdownHook: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn priority(&self) -> i32 { 0 }
    fn timeout(&self) -> Option<Duration> { None }
    fn run(&self, reason: ShutdownReason);   // called at most once
}

pub struct FnHook<F>;                         // with_timeout(self, Duration) -> Self
pub fn hook_from_fn<F>(name: impl Into<String>, priority: i32, f: F) -> FnHook<F>;

#[non_exhaustive] pub struct RunReport;       // reason, outcomes, elapsed, counts, is_clean
#[non_exhaustive] pub struct HookOutcome {    // public fields
    pub name: Arc<str>, pub priority: i32, pub status: HookStatus,
    pub phase: Option<HookPhase>, pub duration: Option<Duration>,
    pub panic_message: Option<String>,
}
#[non_exhaustive] pub enum HookStatus { Pending, Running, Completed, Panicked, TimedOut, Skipped }
#[non_exhaustive] pub enum HookPhase { Graceful, Force }
pub struct RunHooksFuture;                    // Future<Output = RunReport> + Send + 'static
```

### 4.5 Coordinator

```rust,ignore
pub struct Coordinator;

impl Coordinator {
    pub fn builder() -> CoordinatorBuilder;
    pub fn token(&self) -> ShutdownToken;
    pub fn trigger(&self) -> ShutdownTrigger;
    pub fn install(&self) -> Result<()>;
    pub fn uninstall(&self) -> bool;                  // also on Drop
    pub fn is_installed(&self) -> bool;
    pub fn backend(&self) -> Option<Backend>;
    pub fn installed_signals(&self) -> SignalSet;
    pub fn subscribe(&self) -> SignalSubscription;
    pub fn simulate_signal(&self, sig: Signal) -> bool;
    pub fn run_hooks(&self, reason: ShutdownReason) -> usize;
    pub fn run_hooks_with_report(&self, reason: ShutdownReason) -> RunReport;
    pub fn run_hooks_async(&self, reason: ShutdownReason) -> RunHooksFuture;
    pub fn statistics(&self) -> Statistics;
    pub fn signals(&self) -> SignalSet;
    pub fn notify_signals(&self) -> SignalSet;
    pub fn graceful_timeout(&self) -> Duration;
    pub fn force_timeout(&self) -> Duration;
    pub fn force_exit_on_second_signal(&self) -> bool;
}

pub struct CoordinatorBuilder;   // signals, notify_signals, graceful_timeout,
                                 // force_timeout, force_exit_on_second_signal,
                                 // hook, build; Default

#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statistics {
    pub initiated: bool, pub reason: Option<ShutdownReason>, pub elapsed: Option<Duration>,
    pub hooks_registered: usize, pub hooks_completed: usize, pub hooks_panicked: usize,
    pub hooks_timed_out: usize, pub hooks_skipped: usize, pub hooks: Vec<HookOutcome>,
    pub installed: bool, pub active_subsystems: usize,
}

#[non_exhaustive] pub enum Backend { Tokio, Agnostic }

pub struct SignalSubscription;  // recv() -> Recv, try_recv, recv_blocking, recv_timeout, is_closed
pub struct Recv<'a>;            // Future<Output = Option<Signal>>

pub fn shutdown_signal() -> Result<ShutdownSignal>;
pub struct ShutdownSignal;      // Future<Output = ()>; reason(), token()
```

### 4.6 Errors and constants

```rust,ignore
#[non_exhaustive]
#[derive(Debug)]
pub enum Error {
    AlreadyInstalled,
    SignalRegistration { signal: Signal, source: std::io::Error },
    #[deprecated(since = "2.0.0")] InvalidState(&'static str),
    #[deprecated(since = "2.0.0")] Timeout(&'static str),
    NoRuntime,
    OutsideTokioRuntime,
    ListenerThread(std::io::Error),
}
pub type Result<T> = core::result::Result<T, Error>;
pub const VERSION: &str;
```

## 5. Safety contract

The crate is `#![deny(unsafe_code)]`. Signal installation is delegated to
audited crates: `tokio::signal`, and `signal-hook` for the Unix agnostic
back-end and default restoration. The one exception is a single FFI call
in the Windows agnostic back-end, `SetConsoleCtrlHandler` (through
`windows-sys`), in a module that opts out locally and carries a
`// SAFETY:` justification. The handler it registers never unwinds
across the FFI boundary (panics are caught).

## 6. MSRV policy

1.85, declared in `Cargo.toml` and checked in CI for the library with
every back-end. Raising it is a breaking change and requires a major
version. The repository's `rust-toolchain.toml` pins the development
toolchain separately (dev-dependencies such as criterion need newer).

## 7. Performance contract

- `ShutdownToken::is_initiated` is one `Acquire` load; `reason` and
  `elapsed` are lock-free loads; a redundant `trigger` is one load.
- The first `trigger` takes one uncontended lock, publishes with
  `Release`, and wakes waiters after releasing it.
- `wait()` never polls on a timer: the trigger wakes the task.
- Hooks are sorted once at `build()`. A hook run spawns one worker
  thread (plus a fresh one per timed-out hook) and does not context
  switch per hook while hooks finish in time.
- Every number in the README is produced by `benches/shutdown_bench.rs`.

## 8. Stability guarantees

Every public item re-exported from the crate root is covered by
semantic versioning:

- **Patch (`2.x.y`):** bug fixes, documentation, dependency bumps inside
  compatible ranges, CI changes. No public surface or behavior change
  beyond the fix.
- **Minor (`2.x.0`):** additions to the public surface, new opt-in
  features, deprecations, internal performance work.
- **Major (`3.0.0`):** removal or signature change of a public item,
  retiring a feature flag, behavior changes, MSRV raises. Items
  deprecated in 2.x (`Error::InvalidState`, `Error::Timeout`, the
  `async-std` feature) are removed no earlier than 3.0.

`Signal`, `ShutdownReason`, `Error`, `HookStatus`, `HookPhase`,
`Backend`, `Statistics`, `RunReport` and `HookOutcome` are
`#[non_exhaustive]`.

## 9. Dependency policy

Zero mandatory dependencies on Windows; `libc` (constants only) on Unix.
Everything else is optional and feature-gated:

| Dependency    | Feature(s)              | Role                                                     |
| ------------- | ----------------------- | -------------------------------------------------------- |
| `libc`        | always, Unix only       | per-target signal numbers                                |
| `tokio`       | `tokio`                 | listener tasks, runtime context for hooks (`rt`, `signal`) |
| `signal-hook` | `tokio`, `agnostic` (Unix) | listener iterator; conditional default restoration    |
| `windows-sys` | `agnostic` (Windows)    | `SetConsoleCtrlHandler`                                  |
| `tracing`     | `tracing`               | diagnostics                                              |
| `tokio-util`  | `tokio-util`            | `CancellationToken` interop                              |

`cargo audit` and `cargo deny check` (policy in `deny.toml`) run in CI.
The discontinued `async-std` crate is banned from the graph.

## 10. Testing requirements

- Unit tests in every module; integration tests in `tests/` for the
  public surface, hooks and deadlines, installation, edge cases.
- Property tests (`proptest`) for `SignalSet` algebra, signal numbers,
  trigger idempotence, hook ordering and run-once.
- Real-thread stress tests for every concurrency claim; loom models of
  the state machine (`RUSTFLAGS="--cfg loom"`).
- End-to-end real-signal tests in child processes on Linux, macOS and
  Windows (`tests/real_signals.rs`).
- Every code block in `README.md`, `docs/API.md` and `docs/MIGRATION.md`
  is compiled by `cargo test --doc`.
- CI covers the default, all-features and no-default-features builds and
  every feature on its own, on Linux (x86_64, ARM64), macOS and Windows.

## 11. Documentation requirements

Every public item carries rustdoc with `# Errors`, `# Panics` (none of
the public API panics) and `# Examples` where applicable.
`#![deny(missing_docs)]` enforces coverage. `docs/API.md` is the
cross-cutting guide; `docs/MIGRATION.md` covers every breaking change.

## 12. Out of scope

- Process supervision (spawning and managing child processes). That is
  `proc-daemon`'s job; it can build on this crate.
- Service-manager integration (`sd_notify`, the Windows Service Control
  Manager). Belongs in a separate adapter crate.
- Per-thread signal masks. The contract is process-global.
- Async hook bodies (`async fn run`). Hooks are synchronous and can
  bridge to async cleanup through `run_hooks_async`; an async trait
  would force a runtime choice into the trait.

## 13. Intended consumers

1. **`proc-daemon`** - async daemon framework that wants a
   runtime-agnostic substrate for signals and shutdown instead of using
   `ctrlc`, `signal-hook` and `signal-hook-async-std` directly. (As of
   this release it does not depend on `signal-mod` yet.)
2. **Long-running services and CLI tools** - want sane defaults, a
   bounded shutdown, and a one-line integration with their HTTP server.
3. **Embedded worker processes** - want the trigger half without the
   install half when a parent owns signal delivery; the
   `ShutdownTrigger` / `ShutdownToken` split and `simulate_signal`
   cover this.
