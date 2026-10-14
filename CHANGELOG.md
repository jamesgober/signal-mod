# Changelog

All notable changes to this project are documented here. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Fixed

### Security

## [2.0.0] - 2026-10-14

Major release driven by the 2026-10-13 audit of 1.0.0 (findings SM-001
to SM-023) and the re-audit of the 2.0.0 candidate (SM2-001 to
SM2-015). Upgrade notes: `docs/MIGRATION.md`.

### Added

- Enforced hook deadlines. Hooks run on a worker thread; a hook still
  running at its own timeout or at the end of its phase is reported
  `TimedOut` and abandoned, and the run continues. A run returns within
  `force_timeout` whatever the hooks do (SM-002).
- Force phase: hooks not started when `graceful_timeout` runs out start
  with `ShutdownReason::Forced` until `force_timeout` (total budget)
  runs out; the rest are `Skipped`. Both budgets are measured from the
  moment the first hook starts, so worker start-up never eats into them
  and the first hook always starts in the graceful phase (SM2-002).
  `force_timeout` was previously stored but unused (SM-002, D1, D2).
- `HookContext::current()` inside a hook: `deadline()`, `remaining()`,
  `is_abandoned()`, so long hooks can cooperate with the deadline
  (SM2-009).
- `ShutdownHook::timeout` (default `None`) and `FnHook::with_timeout`.
- `Coordinator::run_hooks_with_report` returning `RunReport`, with
  `HookOutcome`, `HookStatus`, `HookPhase`: per-hook status, phase,
  duration and panic message (SM-017).
- `Coordinator::run_hooks_async` / `RunHooksFuture`: runs the hooks on a
  supervisor thread without blocking the executor, on any runtime. With
  the `tokio` feature hooks run with the caller's runtime entered, so
  `Handle::current().block_on(..)` works inside a hook (SM-003).
- `CoordinatorBuilder::force_exit_on_second_signal`: a second shutdown
  signal ends the process. On Unix the signal is re-raised with its
  default action, so the parent sees a death by that signal; on Windows
  the process exits with 128 + number (SM-002, SM2-013).
- Non-shutdown signals: `CoordinatorBuilder::notify_signals`,
  `Coordinator::subscribe`, `SignalSubscription` (`recv`, `try_recv`,
  `recv_blocking`, `recv_timeout`, `is_closed`) and `Recv` (SM-013).
- `Coordinator::uninstall`, `is_installed` after uninstall,
  `backend()` / `Backend`, `installed_signals()`. On Unix, when the last
  coordinator covering a shutdown signal is uninstalled or dropped, the
  signal's default action comes back, but only if it was the default
  before signal-mod first installed it: a signal ignored or handled by
  other code is left to that code, and notify-only signals are never
  restored (SM-011, SM2-004).
- `Coordinator::simulate_signal` for tests and signal forwarding.
- Runtime-agnostic waiting: `ShutdownToken::wait` returns
  `ShutdownFuture` (no polling, any executor, `'static`), plus
  `ShutdownToken::cancelled`, `IntoFuture` for `ShutdownToken` and
  `&ShutdownToken` (SM-008).
- Child scopes: `ShutdownToken::child`.
- Subsystem tracking: `ShutdownToken::subsystem_guard`, `SubsystemGuard`,
  `active_subsystems`, `subsystems_finished` / `SubsystemsFinished`,
  `wait_subsystems_blocking`.
- `ShutdownTrigger::token`.
- Feature `agnostic` (alias `smol`): runtime-agnostic back-end, a
  signal-hook listener thread on Unix and a console control handler on
  Windows that tells all five console events apart. The Tokio back-end
  uses the same handler on Windows, so coordinators installed through
  either back-end all receive every event (SM2-005).
- Feature `tracing`: events for deliveries, installs, hooks and
  escalation.
- Feature `tokio-util`: `ShutdownToken::cancellation_token` and
  `From<&ShutdownToken> for CancellationToken`.
- `shutdown_signal()` / `ShutdownSignal` for `with_graceful_shutdown`.
- `Signal::from_unix_number`, `Signal::exit_code`,
  `ShutdownReason::exit_code`, `ShutdownReason::signal`.
- `SignalSet`: `union`, `intersection`, `difference`,
  `available_on_current_platform`, operators `|` `&` `-` (and assign
  forms), `From<Signal>`, `FromIterator`, `Extend`, `Hash`,
  `IntoIterator for &SignalSet` (SM-017, D7). `SignalSetIter` is
  `ExactSizeIterator` and `FusedIterator`.
- `Statistics` fields `hooks_panicked`, `hooks_timed_out`,
  `hooks_skipped`, `hooks` (live per-hook status and timings),
  `installed`, `active_subsystems`; `PartialEq`/`Eq` (SM-017).
- `Hash` for `ShutdownReason`.
- `Error::OutsideTokioRuntime` and `Error::ListenerThread`.
- Examples `reload_on_sighup`, `axum_server`, `smol_runtime`.
- Tests: real-signal end-to-end tests in child processes on Unix and
  Windows, hook deadline / run-once / re-entrancy tests, installation
  tests per back-end, runtime-agnostic waiting tests, loom models of the
  state machine, more property and stress tests (SM-010, SM-020).
- `deny.toml`, `rust-toolchain.toml`, CI feature matrix, cross-target
  lint, MSRV, loom, AddressSanitizer, audit, deny and semver jobs
  (SM-010, SM-018, SM-022).

### Changed

- MSRV raised from 1.75 to 1.85; edition 2024.
- Hooks run at most once per coordinator. Every `run_hooks*` call after
  (or during) the first returns that run's report (SM-007, D4).
- Hooks run on a worker thread instead of the caller's thread.
- A panicking hook is reported `Panicked` and no longer counted as
  completed (SM-003).
- Hooks are sorted once at `build()` (stable) instead of on every run
  (D15).
- No lock is held while hooks run: `statistics()` and `Debug` work from
  inside a hook and never wait for a running hook (SM-005).
- A coordinator with hooks parks one standby worker thread at build
  time, so `run_hooks` hands off to it instead of spawning a thread
  (SM2-010).
- `install()` picks the back-end at call time (Tokio inside a Tokio
  runtime, otherwise the agnostic back-end) and is all-or-nothing:
  every stream is registered before any listener starts (SM-014).
- Dropping a `Coordinator` uninstalls its listeners.
- `Signal::Hangup` also covers `CTRL_LOGOFF_EVENT` on Windows (SM-015).
- `ctrlc-fallback` and `async-std` are aliases of `agnostic`: they now
  cover every signal in the set and report Windows events accurately
  (SM-015).
- `ShutdownToken::is_initiated` uses `Acquire` ordering (SM-019).
- The `tokio` dependency enables only `rt` and `signal` (was also
  `macros`, `time`, `sync`) (SM-016).
- `SignalSet`'s `Debug` output lists the signals.
- `Statistics` is `#[non_exhaustive]`.
- The crate root lint set follows REPS (`deny(warnings)`,
  `unused_results`, `clippy::unwrap_used`, ... and `clippy::pedantic`).
  `deny(warnings)` is lifted only for docs.rs builds, so a lint added to
  a future nightly cannot leave a release without documentation
  (SM2-012). `forbid(unsafe_code)` became `deny(unsafe_code)` with two
  documented exceptions: the Windows console handler registration and a
  read-only `sigaction` query on Unix (SM-018, SM2-004).
- License: `Apache-2.0 OR MIT` (was `Apache-2.0`); the Apache text moved
  to `LICENSE-APACHE` and `LICENSE-MIT` was added.
- `parking_lot` replaced by `std::sync` (SM-023): no mandatory
  dependency on Windows, only `libc` constants on Unix.
- Criterion 0.5 to 0.8; benches use `std::hint::black_box`.
- Package contents restricted with `include` (no CI files, release
  notes or lint configs).
- CI runs on pull requests and on pushes to `main`, `feature/**` and
  `fix/**`, with the lint toolchain pinned (1.99.0, bumped deliberately
  together with `rust-toolchain.toml`) (SM2-001, SM2-011).

### Deprecated

- Feature `async-std` (async-std is discontinued, RUSTSEC-2025-0052):
  use `agnostic` or `smol`. Removal no earlier than 3.0.
- `Error::InvalidState`, `Error::Timeout`: never produced. Removal no
  earlier than 3.0.

### Removed

- Dependencies `parking_lot`, `async-std`, `futures`, `ctrlc`,
  `signal-hook-async-std` (and through them `async-io` 1, `rustix` 0.37,
  `instant`).

### Fixed

- `Signal::unix_number` returned Linux numbers on every target, so on
  macOS and the BSDs `User1` / `User2` registered SIGBUS / SIGSYS under
  the async-std back-end, and on MIPS Linux the wrong signals. Numbers
  now come from `libc` (SM-001, D8, D9).
- `reason()` could return `None` after `is_initiated()` returned `true`:
  the reason is now published before the flag (SM-004, D6).
- `install()` outside a Tokio runtime panicked and left the coordinator
  marked as installed; it now returns `Error::OutsideTokioRuntime` (or
  uses the agnostic back-end) and leaves no state behind. A Tokio
  runtime without an I/O driver gives `Error::SignalRegistration`
  instead of a panic escaping `install` (Tokio still panics internally:
  the panic hook runs, and `panic = "abort"` binaries abort; documented
  under `# Panics`) (SM-006, D22, SM2-007).
- The async-std `wait()` slept 1-50 ms between polls (median wake
  latency 27 ms); waiting is now notification-driven everywhere
  (SM-008).
- A partially failed install left earlier listeners running; a failure
  under async-std named the wrong signal (SM-014).
- After a coordinator was dropped, SIGINT / SIGTERM / Ctrl+C were
  silently swallowed for the rest of the process (SM-011).
- The `ctrlc-fallback` and async-std back-ends reported Ctrl+Break as
  `Interrupt` and ignored SIGTERM / SIGHUP and console close (SM-015).
- Documentation that did not match the code: the "timeout ladder",
  "hooks have at most this long", "at most once", "consistent
  snapshot", `&SignalSet: IntoIterator`, the `Handle::block_on` bridge,
  "do not install from two coordinators", Windows event coverage,
  "early scaffolding", examples coverage, `.dev/DESIGN.md` references
  (D1 to D29; D18 and D26 corrected in REPS.md and below).
- Vacuous tests (`ran <= 2`, an empty install test) replaced with real
  assertions (SM-020).

### Security

- Removed the discontinued async-std (RUSTSEC-2025-0052) and the
  unmaintained `instant` (RUSTSEC-2024-0384) from every feature's
  dependency graph; `deny.toml` bans async-std.
- A hung hook or main loop can no longer make the process ignore further
  termination signals: deadlines are enforced and second-signal
  escalation is available (SM-002).

## [1.0.0] - 2026-05-19

### Added

- Stable 1.0 public API. Every item re-exported from the crate root
  is now covered by semantic versioning. See `REPS.md` section 4
  for the full surface and `docs/API.md` for the per-item reference.
- Modular installer back-ends under `src/install/`:
  `install::tokio_rt`, `install::async_std_rt`, `install::ctrlc_sync`.
  Each is feature-gated and the previous monolithic `coord.rs`
  install body is replaced with a thin dispatch.
- Panic-safety in `Coordinator::run_hooks`: each hook body runs
  inside `std::panic::catch_unwind`, so a panicking hook is
  swallowed and the remaining hooks still execute. The hook is
  counted as completed.
- Six new runnable examples:
  - `examples/programmatic_shutdown.rs` (trigger without OS signals).
  - `examples/multi_subsystem.rs` (fan-out to many observer tasks).
  - `examples/sync_blocking.rs` (no async runtime; `ctrlc-fallback`).
  - `examples/custom_signal_set.rs` (compile-time + runtime sets).
  - `examples/priority_hooks.rs` (priority ordering and budget).
  - `examples/custom_hook_type.rs` (implementing `ShutdownHook` directly).
- `tests/edge_cases.rs` (20 tests) covering boundary inputs:
  empty signal sets, double-install, zero / very-large hook counts,
  panicking hooks, extreme priorities, immediate-return waits,
  install-without-back-end.
- `tests/stress.rs` (6 tests) covering concurrency: many
  triggers racing to win the CAS, many observers waking on a
  single trigger, 10 000 token clones, rapid trigger / observe
  cycles, concurrent `run_hooks` calls.
- `docs/API.md` now ships a full per-item reference with multiple
  code examples per use case. `README.md` adds Install, Features,
  Examples, API at a glance, Performance, Platform support, and
  Testing sections.
- `docs/MIGRATION.md` rewritten as a 1.0 adoption and `1.x`
  upgrade guide.

### Changed

- Crate renamed from `mod-signal` to `signal-mod` to match the
  repository at `github.com/jamesgober/signal-mod`. The library
  import is now `signal_mod` (snake_case). Pre-1.0 consumers
  who pinned `mod-signal` must migrate by changing the dependency
  line in `Cargo.toml` and the `use` statements in their code.
- Crate version bumped from `0.9.1` to `1.0.0`. The `1.0.0-rc.1`
  intermediate tag from earlier in the session is superseded.
- `Cargo.toml` metadata rewritten for crates.io discoverability:
  description expanded; keywords reordered for search intent
  (`signal`, `sigterm`, `shutdown`, `graceful`, `ctrlc`);
  categories rebuilt from the crates.io taxonomy
  (`os`, `os::unix-apis`, `os::windows-apis`, `asynchronous`,
  `concurrency`).
- `REPS.md` section 4 header updated from "0.x.y" to "1.x.y" and
  section 8 (Stability) rewritten as the production stability
  contract.
- `README.md`, `docs/API.md`, `docs/MIGRATION.md` polished for
  production: every status / pre-1.0 / RC notice removed, every
  version reference aligned to `1.0`.

### Internal

- `coord.rs` shrunk from ~600 lines to ~400 lines after the
  installer extraction. Hook execution now runs each hook through
  `std::panic::catch_unwind(AssertUnwindSafe(|| ...))`.
- `tests/property_tests.rs` strategies cover every variant of
  `Signal` and `ShutdownReason` at 256 cases per property; 10
  algebraic properties total.

## 1.0.0-rc.1 - 2026-05-19

### Changed

- Crate version bumped from `0.9.1` to `1.0.0-rc.1`. This opens
  the API freeze window: every public item re-exported from the
  crate root is contractual from this tag forward. Breaking
  changes between `rc.N` and the next `rc.N+1` require an
  explicit `### Removed` or `### Changed (breaking)` entry here.
- `README.md` status line updated to reflect RC status and the
  14-day minimum soak window before `1.0.0`.
- `REPS.md` section 8 (Stability guarantees) rewritten: the
  `0.x.y` "no API stability" clause is replaced with the active
  freeze contract.

### Internal

- No source under `src/`, `tests/`, `examples/`, `benches/` changed.
- The pre-1.0 verification set continues to apply: 40 tests
  pass across the feature matrix; clippy is clean across four
  feature combinations; doc build is clean.

## [0.9.1] - 2026-05-19

### Fixed

- `MSRV (Rust 1.75)` CI job failed on the `v0.9.0` push because
  `cargo build --all-features --verbose` pulled in
  `async-lock 3.4.2`, which raised its MSRV to 1.85 in early 2026.
  The job is reworked to follow the `proc-daemon` pattern: delete
  `Cargo.lock`, regenerate, pin `async-lock@3.4.2 -> 3.3.0`
  defensively, and validate three realistic feature sets
  (`--no-default-features`, default, `default + ctrlc-fallback`)
  via `cargo check --lib`. The `async-std` feature is excluded
  from MSRV coverage; consumers who need `async-std` should pin
  their own toolchain to a newer minor. See the comment block in
  `.github/workflows/ci.yml` for the full rationale.

### Changed

- Crate version bumped from `0.9.0` to `0.9.1`. No source under
  `src/` changed; this is a CI-only patch.

## 0.9.0 - 2026-05-19

### Added

- `tests/property_tests.rs` (proptest 1.x) with 10 properties
  covering `SignalSet` algebraic identities, trigger idempotence,
  hook-completion totals, Unix-number uniqueness, and platform
  availability consistency. 256 cases per property.
- `docs/MIGRATION.md` documenting upgrade paths between `0.x.y`
  releases and the forward-looking `1.0` plan, with notes on
  coexisting with `ctrlc`, `signal-hook`, and `proc-daemon`.
- `proptest 1` dev-dependency.

### Changed

- README rewritten to reflect functional status and ship the
  canonical Quick Start. Replaces the scaffold-only copy.
- `examples/graceful_shutdown.rs` imports gated on `feature = "tokio"`
  so `cargo clippy --all-targets --no-default-features` is clean.
- Crate version bumped from `0.4.0` to `0.9.0` to mark the
  pre-1.0 stabilization milestone.

## 0.4.0 - 2026-05-19

### Added

- `benches/shutdown_bench.rs` (criterion harness) covering the
  hot paths: `is_initiated`, `trigger` first / redundant,
  `ShutdownToken::clone`, `run_hooks` at 1 / 4 / 16 / 64 hooks,
  `SignalSet::iter`, `wait_blocking_timeout`.
- Performance section in `docs/API.md` with measured numbers on the
  reference platform and notes on the Windows timer-granularity
  effect on small `wait_blocking_timeout` budgets.
- `criterion 0.5` as a dev-dependency.
- `[[bench]]` and `[profile.bench]` sections in `Cargo.toml`.

### Changed

- Crate version bumped from `0.3.0` to `0.4.0`.

## 0.3.0 - 2026-05-19

### Added

- Source implementation of the public API specified at `0.2.0`:
  `Signal`, `SignalSet`, `SignalSetIter`, `ShutdownReason`,
  `ShutdownToken`, `ShutdownTrigger`, `ShutdownHook`, `FnHook`,
  `hook_from_fn`, `Coordinator`, `CoordinatorBuilder`, `Statistics`,
  `Error`, `Result`.
- `Coordinator::install` with feature-dispatched back-ends:
  `tokio` (default), `async-std`, `ctrlc-fallback`. Returns
  `Error::NoRuntime` if none are enabled.
- 21 unit tests, 7 integration tests, 2 doctests.
- `examples/graceful_shutdown.rs` showing the canonical
  install + wait + run_hooks pattern under `tokio`.
- `docs/API.md` populated with the offline API mirror.

### Changed

- `lib.rs` doc comment expanded with a Quick Start doctest.
- Cargo manifest: `parking_lot 0.12` is a mandatory runtime dep;
  `tokio 1.40`, `async-std 1.12`, `futures 0.3`, `signal-hook 0.3`,
  `signal-hook-async-std 0.2`, and `ctrlc 3.4` are optional and
  feature-gated. `[package.metadata.docs.rs]` configured to build
  with `tokio + ctrlc-fallback` (excluding `async-std` for the same
  nightly-rustix reason that affects `proc-daemon`).
- Crate version bumped from `0.2.0` to `0.3.0`.

### Internal

- `forbid(unsafe_code)` retained; all platform `unsafe` is delegated
  to `tokio`, `signal-hook`, and `ctrlc`.

## 0.2.0 - 2026-05-19

### Added

- REPS section 4 (Public API) populated with the full surface for the
  0.x line: `Signal`, `SignalSet`, `ShutdownReason`, `ShutdownToken`,
  `ShutdownTrigger`, `ShutdownHook`, `Coordinator`,
  `CoordinatorBuilder`, `Statistics`, `Error`, `Result`.
- `.dev/DESIGN.md` documenting design trade-offs: substrate vs
  framework split with `proc-daemon`, two-handle observer/initiator
  capability separation, sync hook trait, `parking_lot` choice,
  optional runtime features, sorted-vec hook ordering, install
  fallibility, Windows console-event mapping table, boundary with
  `signal-hook` / `ctrlc`, rejected alternatives.
- REPS section 13: three named real-world consumer profiles
  (`proc-daemon`, CLI long-running tools, embedded supervisor
  processes).

### Changed

- Crate version bumped to `0.2.0` to mark design-lock milestone. No
  source under `src/` changed; implementation lands in `0.3.0`.

## 0.1.0 - 2026-05-12

### Added

- Initial repository scaffold.
- Apache-2.0 license, README, REPS specification stub, CI workflow,
  `.dev/` planning structure (DIRECTIVES, ROADMAP, PROMPTS).
- Crate name reserved on crates.io.

[Unreleased]: https://github.com/jamesgober/signal-mod/compare/v2.0.0...HEAD
[2.0.0]: https://github.com/jamesgober/signal-mod/compare/v1.0.0...v2.0.0
[1.0.0]: https://github.com/jamesgober/signal-mod/compare/v0.9.1...v1.0.0
[0.9.1]: https://github.com/jamesgober/signal-mod/releases/tag/v0.9.1

<!-- Versions without a link were never tagged on GitHub. -->
