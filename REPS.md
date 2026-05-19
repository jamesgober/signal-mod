# signal-mod - Project Specification (REPS)

> Authoritative specification for the public API surface and design
> contract of `signal-mod`.

## 1. Identity

- **Crate name:** `signal-mod`
- **Author:** James Gober <me@jamesgober.com>
- **Repository:** https://github.com/jamesgober/signal-mod
- **License:** Apache-2.0
- **MSRV:** 1.75

## 2. Mission

Unified OS signal handling for Rust. SIGTERM / SIGINT / SIGHUP / SIGPIPE
and Windows equivalents through one API. Graceful shutdown
orchestration with priority ordering, timeout enforcement, and hook
chaining. Replaces the patchwork of `ctrlc` + `signal-hook` with a
clean runtime-agnostic substrate.

## 3. Scope

`signal-mod` provides:

1. A cross-platform `Signal` enum that abstracts away the differences
   between Unix signals and Windows console control events.
2. A `Coordinator` that owns a shutdown state machine, fans signals
   out to observers, and runs priority-ordered shutdown hooks under a
   configurable timeout ladder.
3. Cloneable `ShutdownToken` (observer) and `ShutdownTrigger`
   (initiator) handles that downstream code can hold without owning
   the coordinator.
4. Optional runtime adapters for `tokio` and `async-std` that expose
   async wait points (`token.wait().await`) backed by the runtime's
   own signal stream, broadcast, or polling primitives.

Out of scope items are enumerated in section 12.

## 4. Public API

The public surface of `signal-mod 1.x.y` is the following set of
items, all re-exported from the crate root unless otherwise noted.
Every item below is frozen by the `1.0.0` semver contract; see
section 8 for the stability commitment.

### 4.1 Signal taxonomy

```rust
/// Cross-platform unified signal identifier.
///
/// Variants map to their nearest platform equivalent. On Unix the
/// mapping is direct (SIGTERM, SIGINT, etc.). On Windows the mapping
/// is to Windows console control events: `Terminate` -> CTRL_CLOSE,
/// `Interrupt` -> CTRL_C, `Quit` -> CTRL_BREAK, `Hangup` ->
/// CTRL_SHUTDOWN. Unix-only variants (`Pipe`, `User1`, `User2`) are
/// inert on Windows.
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

impl Signal {
    pub const fn description(self) -> &'static str;
    pub const fn unix_number(self) -> Option<i32>;
    pub const fn is_unix_only(self) -> bool;
    pub fn available_on_current_platform(self) -> bool;
}
```

### 4.2 Signal sets

```rust
/// Bit-packed set of signals the coordinator will install handlers
/// for. Const-constructible so default sets are zero-cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalSet { /* private bits */ }

impl SignalSet {
    pub const fn empty() -> Self;
    pub const fn all() -> Self;
    /// `Terminate | Interrupt | Hangup`. Recommended default for
    /// long-running services.
    pub const fn graceful() -> Self;
    /// `Terminate | Interrupt | Quit | Hangup`. Maximum graceful
    /// coverage.
    pub const fn standard() -> Self;

    pub const fn with(self, sig: Signal) -> Self;
    pub const fn without(self, sig: Signal) -> Self;
    pub const fn contains(self, sig: Signal) -> bool;
    pub const fn is_empty(self) -> bool;
    pub const fn len(self) -> usize;
    pub fn iter(&self) -> SignalSetIter;
}

pub struct SignalSetIter { /* ... */ }
impl Iterator for SignalSetIter { type Item = Signal; /* ... */ }
```

### 4.3 Shutdown reason

```rust
/// Reason a shutdown was initiated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    Signal(Signal),
    Requested,
    Forced,
    Timeout,
    Error,
}

impl ShutdownReason {
    pub const fn description(self) -> &'static str;
    pub const fn is_signal(self) -> bool;
}

impl core::fmt::Display for ShutdownReason { /* ... */ }
```

### 4.4 Observer and initiator handles

```rust
/// Cheap-to-clone observer handle. Hand one of these to every
/// subsystem that needs to react to shutdown.
#[derive(Debug, Clone)]
pub struct ShutdownToken { /* Arc<Inner> */ }

impl ShutdownToken {
    pub fn is_initiated(&self) -> bool;
    pub fn reason(&self) -> Option<ShutdownReason>;
    pub fn elapsed(&self) -> Option<Duration>;

    /// Block the current thread until shutdown is initiated.
    pub fn wait_blocking(&self);

    /// Block the current thread for at most `timeout`. Returns true
    /// if shutdown was observed.
    pub fn wait_blocking_timeout(&self, timeout: Duration) -> bool;

    /// Async wait. Tokio variant; gated on the `tokio` feature.
    #[cfg(feature = "tokio")]
    pub async fn wait(&self);

    /// Async wait. async-std variant; gated on the `async-std`
    /// feature and only compiled when `tokio` is not enabled.
    #[cfg(all(feature = "async-std", not(feature = "tokio")))]
    pub async fn wait(&self);
}

/// Cheap-to-clone initiator handle. Hand one of these to any code
/// path that may need to ask for shutdown (HTTP `/shutdown` route,
/// supervisory parent, fatal-error site).
#[derive(Debug, Clone)]
pub struct ShutdownTrigger { /* Arc<Inner> */ }

impl ShutdownTrigger {
    /// Initiate shutdown with the given reason. Returns true if this
    /// call performed the transition; false if it was already
    /// initiated.
    pub fn trigger(&self, reason: ShutdownReason) -> bool;
    pub fn is_initiated(&self) -> bool;
}
```

### 4.5 Shutdown hooks

```rust
/// A unit of cleanup work to run during shutdown.
pub trait ShutdownHook: Send + Sync + 'static {
    fn name(&self) -> &str;

    /// Higher priority hooks run first. Defaults to 0.
    fn priority(&self) -> i32 { 0 }

    /// Cleanup body. Called once, on the coordinator's draining
    /// thread (sync). Hooks needing async work should bridge via
    /// the runtime's `block_on` or schedule onto a runtime they
    /// hold separately.
    fn run(&self, reason: ShutdownReason);
}

/// Convenience: convert any closure into a hook.
pub fn hook_from_fn<F>(name: impl Into<String>, priority: i32, f: F) -> impl ShutdownHook
where
    F: Fn(ShutdownReason) + Send + Sync + 'static;
```

### 4.6 Coordinator

```rust
pub struct Coordinator { /* ... */ }

impl Coordinator {
    pub fn builder() -> CoordinatorBuilder;

    pub fn token(&self) -> ShutdownToken;
    pub fn trigger(&self) -> ShutdownTrigger;

    /// Install OS-level signal handlers for the configured set.
    /// Idempotent within a single coordinator; the same physical
    /// process must not have two coordinators install handlers for
    /// the same signal concurrently.
    ///
    /// # Errors
    ///
    /// Returns `Error::AlreadyInstalled` if this coordinator has
    /// already installed its handlers, and
    /// `Error::SignalRegistration` if the platform rejects the
    /// registration.
    pub fn install(&self) -> Result<()>;

    /// Run all registered hooks in priority order. Honors the
    /// graceful timeout. Returns the number of hooks that completed
    /// in time.
    pub fn run_hooks(&self, reason: ShutdownReason) -> usize;

    pub fn statistics(&self) -> Statistics;
}

pub struct CoordinatorBuilder { /* ... */ }

impl CoordinatorBuilder {
    pub fn signals(self, set: SignalSet) -> Self;
    pub fn graceful_timeout(self, d: Duration) -> Self;
    pub fn force_timeout(self, d: Duration) -> Self;
    pub fn hook<H: ShutdownHook>(self, h: H) -> Self;
    pub fn build(self) -> Coordinator;
}

#[derive(Debug, Clone)]
pub struct Statistics {
    pub initiated: bool,
    pub reason: Option<ShutdownReason>,
    pub hooks_registered: usize,
    pub hooks_completed: usize,
    pub elapsed: Option<Duration>,
}
```

### 4.7 Error type

```rust
#[non_exhaustive]
#[derive(Debug)]
pub enum Error {
    AlreadyInstalled,
    SignalRegistration { signal: Signal, source: std::io::Error },
    InvalidState(&'static str),
    Timeout(&'static str),
    NoRuntime,
}

impl core::fmt::Display for Error { /* ... */ }
impl std::error::Error for Error { /* ... */ }

pub type Result<T> = core::result::Result<T, Error>;
```

### 4.8 Crate-level constants and helpers

```rust
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
```

## 5. Safety contract

The core implementation is `#![deny(unsafe_code)]`. Signal
installation is delegated to runtime crates (`tokio`,
`signal-hook-async-std`) which carry their own audited `unsafe`. The
runtime-less fallback uses `ctrlc 3.x`, which similarly contains the
`unsafe` blocks behind a vetted boundary. `signal-mod` itself
contains no `unsafe` blocks.

If any `unsafe` is added in a future release, every block must carry
a `// SAFETY:` comment per `.dev/DIRECTIVES.md` section 3.

## 6. MSRV policy

Pinned at 1.75. Bumps require a minor version increment and a
CHANGELOG entry under `### Changed` with rationale.

## 7. Performance contract

The fast path of `ShutdownToken::is_initiated` is a single relaxed
atomic load. `ShutdownTrigger::trigger` is one compare-exchange plus
notification of waiters. Hook iteration is `O(n log n)` once at sort
time and `O(n)` thereafter. Concrete numbers ship in `0.4.0` once
benchmarks land.

## 8. Stability guarantees

`signal-mod 1.0.0` is production-stable. Every public item
re-exported from the crate root is covered by semantic versioning:

- **Patch (`1.x.y`)** - bug fixes, dep bumps inside
  semver-compatible ranges, doc improvements, CI fixes. No public
  surface change.
- **Minor (`1.x.0`)** - pure additions to the public surface,
  new opt-in features, internal performance work that does not
  change behavior. MSRV bumps allowed; see section 6.
- **Major (`2.0.0`)** - anything that removes, renames, or
  changes the signature of a public symbol, retires a feature
  flag, or adds a non-opt-in runtime dependency.

The `Error` enum is `#[non_exhaustive]`, so adding variants is a
minor-version change. Downstream `match` arms must include a
wildcard.

## 9. Dependency policy

Zero runtime dependencies preferred. The current dependency set is:

- `parking_lot` (runtime, default) - non-poisoning mutex for the
  internal state machine. Justified in `.dev/DESIGN.md`.
- `tokio` (optional, feature `tokio`) - async wait point and signal
  stream.
- `async-std`, `signal-hook-async-std`, `signal-hook`, `futures`
  (optional, feature `async-std`) - same role under async-std.
- `ctrlc` (optional, feature `ctrlc-fallback`) - no-runtime signal
  installation path.
- `libc` (optional, target_family = "unix", feature `async-std`) -
  pulled in transitively by the async-std signal path.

Any future addition requires a documented justification entry in
`.dev/DESIGN.md`.

## 10. Testing requirements

- Every public method has a `#[cfg(test)]` exercise once the API
  stabilizes at `0.3.0`.
- Integration tests in `tests/` cover the cross-platform scenarios
  (signal trigger, hook ordering, timeout behavior, double-install
  rejection).
- Property tests in `tests/` (via `proptest`) ship at `0.9.0`.
- Doc tests for every public entry point ship at `0.9.0`.

## 11. Documentation requirements

Every public item carries a rustdoc block. `# Errors`, `# Panics`,
and `# Examples` sections are present where applicable. `docs/API.md`
mirrors the rustdoc layout for offline readers.

## 12. Out of scope

The following are explicitly out of scope and will not be added in
the `1.0` line:

- Process supervision (spawning child processes, managing their
  lifecycle). That is the job of `proc-daemon`, which is a downstream
  consumer of this crate.
- Service-manager integration (`sd_notify`, Windows Service Control
  Manager). Belongs in a separate adapter crate.
- Logging or tracing facade. The crate emits zero log output by
  default; observability is the caller's responsibility.
- Per-thread signal masks. The current contract is process-global.

## 13. Real-world consumers

The design is validated against three concrete consumer profiles:

1. **`proc-daemon`** (in-tree sibling crate) - async daemon
   framework. Wants a runtime-agnostic substrate for signal handling
   so its own surface (Subsystem, ShutdownCoordinator) does not have
   to choose between `ctrlc` and `signal-hook`.
2. **CLI long-running tools** (e.g. local development servers, log
   tailers, file watchers) - want a one-liner that installs sane
   defaults, runs a single async closure on shutdown, and otherwise
   stays out of the way.
3. **Embedded supervisor processes** (e.g. workers spawned by a
   parent that uses `proc-daemon`) - want the trigger half without
   the install half, because the parent already owns signal
   delivery. The `ShutdownTrigger` / `ShutdownToken` split makes this
   trivial.
