# Changelog

All notable changes to this project are documented here. The format is
based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and
the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/jamesgober/mod-signal/compare/v0.9.0...HEAD
[0.9.0]: https://github.com/jamesgober/mod-signal/releases/tag/v0.9.0
[0.4.0]: https://github.com/jamesgober/mod-signal/releases/tag/v0.4.0
[0.3.0]: https://github.com/jamesgober/mod-signal/releases/tag/v0.3.0
[0.2.0]: https://github.com/jamesgober/mod-signal/releases/tag/v0.2.0
[0.1.0]: https://github.com/jamesgober/mod-signal/releases/tag/v0.1.0
