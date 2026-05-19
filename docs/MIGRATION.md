# mod-signal - Migration Guide

> Upgrade paths between `0.x.y` versions, and a forward-looking note
> on the `1.0` API freeze.

## Table of contents

- [From `0.2.0` or earlier (design-only)](#from-020-or-earlier-design-only)
- [From `0.3.0` to `0.4.0`](#from-030-to-040)
- [From `0.4.0` to `0.9.0`](#from-040-to-090)
- [Forward-looking: `0.9.x` -> `1.0.0`](#forward-looking-09x---100)
- [Coexisting with `ctrlc` and `signal-hook`](#coexisting-with-ctrlc-and-signal-hook)
- [Coexisting with `proc-daemon`](#coexisting-with-proc-daemon)

## From `0.2.0` or earlier (design-only)

`0.1.0` reserved the crate name; `0.2.0` published the design lock.
Neither shipped a functional `src/` body. There is nothing to
migrate from in your code base. Add the dep at the current minor:

```toml
[dependencies]
mod-signal = "0.9"
```

Start from the Quick Start in the crate root rustdoc, or
`examples/graceful_shutdown.rs`.

## From `0.3.0` to `0.4.0`

No public API change. `cargo update -p mod-signal` is sufficient.

Optional: rerun the bench suite on your hardware to populate
your own performance baseline:

```bash
cargo bench --bench shutdown_bench
```

## From `0.4.0` to `0.9.0`

No public API change. `cargo update -p mod-signal` is sufficient.

New in `0.9.0`:

- `tests/property_tests.rs` (proptest) is part of the in-tree test
  suite. Downstream consumers do not pay for `proptest` in their
  builds; it is dev-only.
- `docs/MIGRATION.md` (this file).
- `cargo-semver-checks` recommendation in the CI documentation;
  see `.github/workflows/ci.yml` for the actual wiring.

If you wrote your own custom property-style tests against
`SignalSet`'s algebraic identities, the bundled `tests/property_tests.rs`
now covers them. Yours can be retired or kept as a regression
backstop.

## Forward-looking: `0.9.x` -> `1.0.0`

The `0.9.x` line is the API freeze prep window. The `1.0.0` plan:

1. **`1.0.0-rc.1`** ships once `cargo-semver-checks` reports no
   breaking change versus the latest `0.9.x` release.
2. A 14-day soak period; bugfixes only.
3. **`1.0.0`** locks the surface. After `1.0.0`, any item exported
   from the crate root is covered by semver.

If you are pinning today, prefer `mod-signal = "0.9"` over
`"0.9.0"` so patch fixes flow in.

Things that may still change between `0.9.0` and `1.0.0-rc.1`:

- The `Error` enum is `#[non_exhaustive]`; new variants may be
  added in `0.9.x` patches (not in `1.0.x`).
- Internal modules (`state`) are private and may be reshaped.
- `docs.rs` metadata may add additional features as needs surface.

Things that will **not** change:

- The names, kinds, and signatures of the public items listed in
  [`docs/API.md`](API.md).
- The `parking_lot` runtime dependency (it stays mandatory).
- MSRV `1.75`. A bump requires a minor version increment and a
  `CHANGELOG` `### Changed` entry per `REPS.md` section 6.

## Coexisting with `ctrlc` and `signal-hook`

`mod-signal` sits at the layer above `ctrlc` and `signal-hook`. If
your application already depends on one of them, you can:

- **Keep your existing `ctrlc` handler** and wire its callback to
  call `ShutdownTrigger::trigger`. Do not also call
  `Coordinator::install` in this configuration; the two would
  contend for the same OS slot.
- **Keep your existing `signal-hook` `SignalsInfo`** and forward
  each signal to `ShutdownTrigger::trigger(ShutdownReason::Signal(...))`.
  Same caveat: don't double-install.

The cleanest path is to retire the direct dependency once you
migrate fully to `mod-signal`. The `signal-hook` Unix code is
already pulled in transitively through the `async-std` feature, so
you do not save a dep by keeping your own copy.

## Coexisting with `proc-daemon`

`proc-daemon` is being refactored (in its own v2 line) to consume
`mod-signal` as a substrate. Until that lands:

- A `proc-daemon` application can hold a `mod_signal::Coordinator`
  alongside its `proc_daemon::Daemon`. Wire `mod_signal::ShutdownTrigger`
  to `proc_daemon::ShutdownCoordinator::initiate_shutdown` if you want
  the two to share state.
- Do **not** let both crates install handlers for the same signal
  in the same process. Pick one: either `proc-daemon` owns signal
  installation (default), or `mod-signal` does (call
  `Coordinator::install` and disable `proc-daemon`'s signal
  handling via its `SignalConfig`).

Once `proc-daemon` v2 ships, it will install via `mod-signal`
internally and this section will be retired.
