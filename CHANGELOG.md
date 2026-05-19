# Changelog

All notable changes to this project are documented here. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

## [1.0.0-rc.1] - 2026-05-19

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

## [0.9.0] - 2026-05-19

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

## [0.4.0] - 2026-05-19

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

## [0.3.0] - 2026-05-19

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

## [0.2.0] - 2026-05-19

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

## [0.1.0] - 2026-05-12

### Added

- Initial repository scaffold.
- Apache-2.0 license, README, REPS specification stub, CI workflow,
  `.dev/` planning structure (DIRECTIVES, ROADMAP, PROMPTS).
- Crate name reserved on crates.io.

[Unreleased]: https://github.com/jamesgober/signal-mod/compare/v1.0.0...HEAD
[1.0.0]: https://github.com/jamesgober/signal-mod/releases/tag/v1.0.0
[1.0.0-rc.1]: https://github.com/jamesgober/signal-mod/releases/tag/v1.0.0-rc.1
[0.9.1]: https://github.com/jamesgober/signal-mod/releases/tag/v0.9.1
[0.9.0]: https://github.com/jamesgober/signal-mod/releases/tag/v0.9.0
[0.4.0]: https://github.com/jamesgober/signal-mod/releases/tag/v0.4.0
[0.3.0]: https://github.com/jamesgober/signal-mod/releases/tag/v0.3.0
[0.2.0]: https://github.com/jamesgober/signal-mod/releases/tag/v0.2.0
[0.1.0]: https://github.com/jamesgober/signal-mod/releases/tag/v0.1.0
