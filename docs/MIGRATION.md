# signal-mod - Migration Guide

This guide covers the upgrade path **to** `signal-mod 1.0.0` and
the policy that governs upgrades **within** the `1.x` line. For the
authoritative API reference see [`API.md`](API.md); for the design
contract see [`../REPS.md`](../REPS.md).

## Table of contents

- [Adopting `signal-mod 1.0.0`](#adopting-signal-mod-100)
- [Upgrading within the `1.x` line](#upgrading-within-the-1x-line)
- [Coexisting with `ctrlc` and `signal-hook`](#coexisting-with-ctrlc-and-signal-hook)
- [Coexisting with daemon frameworks](#coexisting-with-daemon-frameworks)
- [The `2.0` plan](#the-20-plan)

## Adopting `signal-mod 1.0.0`

Add the dependency:

```toml
[dependencies]
signal-mod = "1.0"
```

Default features (`std + tokio`) cover the most common path. To
swap runtimes:

```toml
# async-std
signal-mod = { version = "1.0", default-features = false, features = ["std", "async-std"] }

# Synchronous Ctrl+C only, no async runtime
signal-mod = { version = "1.0", default-features = false, features = ["std", "ctrlc-fallback"] }
```

Then follow the Quick Start in the [crate root rustdoc][docs-root]
or [`examples/graceful_shutdown.rs`](../examples/graceful_shutdown.rs).

[docs-root]: https://docs.rs/signal-mod/latest/signal_mod/

### From a custom `ctrlc` + `signal-hook` setup

Replace the custom dispatcher with a `Coordinator`. The typical
mechanical changes:

| Before                                                  | After                                                                |
| ------------------------------------------------------- | -------------------------------------------------------------------- |
| `ctrlc::set_handler(\|\| { /* set flag */ })`           | `let coord = Coordinator::builder().build(); coord.install()?;`      |
| Polling a custom shutdown flag                          | `coord.token().wait().await` / `wait_blocking()`                     |
| A `Vec<Box<dyn Fn()>>` cleanup list iterated by hand    | `coord.run_hooks(reason)` with `ShutdownHook` impls                  |
| Hand-rolled priority sort                               | `ShutdownHook::priority` (descending; ties preserve insertion order) |
| Catching panics around cleanup callbacks                | Built in: `run_hooks` is panic-safe per hook                         |

### From `tokio::signal::ctrl_c().await`

`tokio::signal::ctrl_c` only covers SIGINT / Ctrl+C. `signal-mod`'s
default `SignalSet::graceful()` additionally covers SIGTERM and
SIGHUP (and the Windows `CTRL_CLOSE_EVENT` / `CTRL_SHUTDOWN_EVENT`),
which is what most services actually want.

Replacement:

```rust,no_run
use signal_mod::{Coordinator, ShutdownReason};

# #[cfg(feature = "tokio")]
# #[tokio::main]
# async fn main() -> signal_mod::Result<()> {
let coord = Coordinator::builder().build();
coord.install()?;

let token = coord.token();
token.wait().await;

let _reason = token.reason().unwrap_or(ShutdownReason::Requested);
# Ok(())
# }
```

## Upgrading within the `1.x` line

The `1.x` line follows [Semantic Versioning]. `cargo update -p signal-mod`
is sufficient for any patch (`1.x.y`) or minor (`1.x.0`) upgrade.

[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

The contract is documented in [`../REPS.md`](../REPS.md) section 8;
in summary:

- **Patch (`1.x.y`)** - bug fixes, doc improvements, dep bumps in
  semver-compatible ranges, CI changes. No public surface change.
- **Minor (`1.x.0`)** - additions to the public surface, new opt-in
  features, internal perf work. MSRV bumps allowed.
- **Major (`2.0.0`)** - anything that removes, renames, or changes
  the signature of a public symbol, retires a feature flag, or adds
  a non-opt-in runtime dependency.

The `Error` enum is `#[non_exhaustive]`. New variants may land in
`1.x.0`; downstream `match` arms must include a wildcard.

## Coexisting with `ctrlc` and `signal-hook`

`signal-mod` sits at the layer above `ctrlc` and `signal-hook`. If
your application already depends on one of them, you can:

- **Keep an existing `ctrlc` handler** and wire its callback to
  call `ShutdownTrigger::trigger`. Do not also call
  `Coordinator::install` in this configuration; the two would
  contend for the same OS slot.
- **Keep an existing `signal-hook::iterator::Signals`** and forward
  each signal to `ShutdownTrigger::trigger(ShutdownReason::Signal(...))`.
  Same caveat: do not double-install.

The cleanest path is to retire the direct dependency once you
migrate fully to `signal-mod`. `signal-hook` is pulled in
transitively through the `async-std` feature, so you do not save a
dep by keeping your own copy.

## Coexisting with daemon frameworks

If you embed `signal-mod` inside a larger daemon framework, pick
exactly one signal-handler owner:

- **Framework owns signals** - configure the framework as usual.
  Build the `signal-mod` `Coordinator` with `SignalSet::empty()`
  and forward signals from the framework's own handler by calling
  `ShutdownTrigger::trigger(ShutdownReason::Signal(...))`.
- **`signal-mod` owns signals** - configure the framework to skip
  its own signal install (most frameworks expose a flag for this),
  then call `coord.install()` on the `signal-mod` coordinator.

Process-global signal slots are first-come-first-served. Two
handlers in the same process do not compose.

## The `2.0` plan

There is no `2.0` plan as of the `1.0.0` tag. Breaking-change ideas
that surface in the wild will be collected and considered for a
future `2.0`. Until then, `1.x` is the stable line.

Items that are explicitly out of scope for the `1.x` line and
deferred to a hypothetical `2.0`:

- An `async fn` `ShutdownHook::run` (would force a runtime choice
  into the trait).
- Per-thread signal masks (the current contract is process-global).
- `no_std` core (the implementation needs `Mutex`, `Condvar`,
  `Instant`, and the signal back-ends).

If you have a concrete need for any of these, file an issue.
