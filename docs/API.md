# signal-mod - API Guide

Reference for every public item of `signal-mod` 2.x, with the behavior
behind it. The rustdoc on [docs.rs](https://docs.rs/signal-mod) carries
the same contracts item by item; this guide adds the cross-cutting
explanations. Every Rust block below is compiled (and, unless marked
`no_run`, executed) by `cargo test --doc`.

For the design contract see [`REPS.md`](../REPS.md); for upgrading from
1.x see [`MIGRATION.md`](MIGRATION.md).

## Table of contents

- [Concepts](#concepts)
- [Back-ends and features](#back-ends-and-features)
- [`Signal`](#signal)
- [`SignalSet`](#signalset) and [`SignalSetIter`](#signalsetiter)
- [`ShutdownReason`](#shutdownreason)
- [`ShutdownToken`](#shutdowntoken), [`ShutdownFuture`](#shutdownfuture),
  [`SubsystemGuard`](#subsystemguard), [`SubsystemsFinished`](#subsystemsfinished)
- [`ShutdownTrigger`](#shutdowntrigger)
- [`ShutdownHook`](#shutdownhook), [`HookContext`](#hookcontext), [`FnHook`](#fnhook), [`hook_from_fn`](#hook_from_fn)
- [`Coordinator`](#coordinator)
- [`CoordinatorBuilder`](#coordinatorbuilder)
- [Running hooks: the deadline ladder](#running-hooks-the-deadline-ladder)
- [`RunReport`](#runreport), [`HookOutcome`](#hookoutcome), [`HookStatus`](#hookstatus), [`HookPhase`](#hookphase), [`RunHooksFuture`](#runhooksfuture)
- [`Statistics`](#statistics)
- [`SignalSubscription`](#signalsubscription) and [`Recv`](#recv)
- [`Backend`](#backend)
- [`shutdown_signal`](#shutdown_signal) and [`ShutdownSignal`](#shutdownsignal)
- [`Error`](#error) and [`Result`](#result)
- [`VERSION`](#version)
- [Platform behavior](#platform-behavior)
- [Performance](#performance)

---

## Concepts

A **coordinator** owns one shutdown. It installs OS signal listeners,
hands out two kinds of handle, and runs cleanup hooks:

- [`ShutdownToken`](#shutdowntoken) observes: `is_initiated`, `reason`,
  `wait().await`, `wait_blocking()`.
- [`ShutdownTrigger`](#shutdowntrigger) initiates: `trigger(reason)`.
  The first trigger wins; later ones return `false`.

Splitting the two keeps "may stop the process" a capability you hand out
on purpose. Both cost one `Arc` clone to copy.

Signals come in two sets: the **shutdown set** (initiates shutdown) and
the **notify set** (delivered to [`SignalSubscription`](#signalsubscription)s
only: SIGHUP for reload, SIGUSR1 for log rotation).

**Hooks** run once, in descending priority, under an enforced
graceful-then-force deadline; see
[the deadline ladder](#running-hooks-the-deadline-ladder).

```rust
use signal_mod::{Coordinator, ShutdownReason, Signal, SignalSet, hook_from_fn};

let coord = Coordinator::builder()
    .signals(SignalSet::graceful() - Signal::Hangup)
    .notify_signals(SignalSet::from(Signal::Hangup))
    .hook(hook_from_fn("flush", 0, |_| {}))
    .build();

let token = coord.token();
let trigger = coord.trigger();
assert!(trigger.trigger(ShutdownReason::Requested));
assert_eq!(token.reason(), Some(ShutdownReason::Requested));
assert_eq!(coord.run_hooks(ShutdownReason::Requested), 1);
```

---

## Back-ends and features

| Feature          | Default | Effect                                                                                     |
| ---------------- | ------- | ------------------------------------------------------------------------------------------ |
| `std`            | yes     | Reserved for a future `no_std` core.                                                        |
| `tokio`          | yes     | `install` inside a Tokio runtime spawns one `tokio::signal` listener task per signal.        |
| `agnostic`       | no      | `install` anywhere: a signal-hook listener thread (Unix) or a console control handler (Windows). |
| `smol`           | no      | Alias of `agnostic`.                                                                       |
| `ctrlc-fallback` | no      | 1.x name, alias of `agnostic`.                                                             |
| `async-std`      | no      | Deprecated alias of `agnostic`; no longer pulls in the discontinued async-std crate.       |
| `tracing`        | no      | `tracing` events (target `signal_mod`) for deliveries, installs, hooks and escalation.     |
| `tokio-util`     | no      | `ShutdownToken::cancellation_token` and `From<&ShutdownToken> for CancellationToken`.       |

[`Coordinator::install`](#coordinator) picks the back-end when it is
called: Tokio if the `tokio` feature is on and a Tokio runtime is
current, otherwise the agnostic back-end if enabled, otherwise an error
that says what is missing. Waiting, subscriptions and
`run_hooks_async` are runtime-agnostic in every configuration.

---

## `Signal`

```rust,ignore
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal { Terminate, Interrupt, Quit, Hangup, Pipe, User1, User2 }
```

| Variant     | Unix     | Windows                                    |
| ----------- | -------- | ------------------------------------------ |
| `Terminate` | SIGTERM  | `CTRL_CLOSE_EVENT`                         |
| `Interrupt` | SIGINT   | `CTRL_C_EVENT`                             |
| `Quit`      | SIGQUIT  | `CTRL_BREAK_EVENT`                         |
| `Hangup`    | SIGHUP   | `CTRL_LOGOFF_EVENT`, `CTRL_SHUTDOWN_EVENT` |
| `Pipe`      | SIGPIPE  | never delivered                            |
| `User1`     | SIGUSR1  | never delivered                            |
| `User2`     | SIGUSR2  | never delivered                            |

| Item                                    | Meaning                                                                 |
| --------------------------------------- | ----------------------------------------------------------------------- |
| `ALL: [Signal; 7]`                      | Every variant, canonical order.                                          |
| `description(self) -> &'static str`     | Label used by `Display`.                                                 |
| `unix_number(self) -> Option<i32>`      | Number **on the compiled target** (from `libc`); POSIX numbers off Unix, `None` for USR1/USR2 there. |
| `from_unix_number(i32) -> Option<Self>` | Inverse of `unix_number`.                                                |
| `exit_code(self) -> i32`                | `128 + number` (130 for SIGINT, 143 for SIGTERM).                        |
| `is_unix_only(self) -> bool`            | `Pipe`, `User1`, `User2`.                                                |
| `available_on_current_platform(self)`   | Whether this target can deliver it.                                      |

Signal numbers are per target: SIGUSR1 is 10 on Linux x86/ARM, 16 on
MIPS Linux, 30 on macOS and the BSDs. (1.0.0 hard-coded the Linux
values, so on macOS its async-std back-end registered SIGBUS for
`User1`.)

`Pipe`: Rust programs ignore SIGPIPE by default so writes to a closed
pipe return `EPIPE`. Putting `Pipe` in the shutdown set turns every such
write into a graceful shutdown. Only do that on purpose.

```rust
use signal_mod::Signal;

assert_eq!(Signal::Interrupt.exit_code(), 130);
for sig in Signal::ALL {
    if let Some(n) = sig.unix_number() {
        assert_eq!(Signal::from_unix_number(n), Some(sig));
    }
}
assert_eq!(Signal::User1.available_on_current_platform(), cfg!(unix));
```

---

## `SignalSet`

A `Copy`, const-constructible bit set of signals with `|`, `&`, `-`
(with a set or a single signal on the right), `FromIterator`, `Extend`,
`IntoIterator` (by value and by reference) and `Hash`. `Default` is
`graceful()`.

| Constructor / method                    | Result                                  |
| --------------------------------------- | --------------------------------------- |
| `empty()`                               | no signals                              |
| `graceful()`                            | `Terminate`, `Interrupt`, `Hangup`      |
| `standard()`                            | `graceful()` plus `Quit`                |
| `all()`                                 | every variant (including `Pipe`)        |
| `with(sig)` / `without(sig)`            | add / remove one signal                 |
| `union` / `intersection` / `difference` | const set algebra                       |
| `contains`, `is_empty`, `len`, `iter`   | queries                                 |
| `available_on_current_platform()`       | subset this target can deliver          |

```rust
use signal_mod::{Signal, SignalSet};

const RELOAD: SignalSet = SignalSet::empty().with(Signal::Hangup).with(Signal::User1);
const SHUTDOWN: SignalSet = SignalSet::standard().difference(RELOAD);

assert!(SHUTDOWN.contains(Signal::Quit));
assert!(!SHUTDOWN.contains(Signal::Hangup));
assert_eq!((SHUTDOWN | RELOAD).len(), 5);
assert_eq!(SignalSet::default(), SignalSet::graceful());

let collected: SignalSet = [Signal::Quit, Signal::Quit].into_iter().collect();
assert_eq!(collected.len(), 1);
for sig in &collected {
    assert_eq!(sig, Signal::Quit);
}
```

## `SignalSetIter`

Iterator over a set in canonical `Signal::ALL` order; implements
`ExactSizeIterator` and `FusedIterator`.

```rust
use signal_mod::{Signal, SignalSet};

let mut it = SignalSet::graceful().iter();
assert_eq!(it.len(), 3);
assert_eq!(it.next(), Some(Signal::Terminate));
assert_eq!(it.next(), Some(Signal::Interrupt));
assert_eq!(it.next(), Some(Signal::Hangup));
assert_eq!(it.next(), None);
assert_eq!(it.next(), None);
```

---

## `ShutdownReason`

```rust,ignore
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShutdownReason { Signal(Signal), Requested, Forced, Timeout, Error }
```

The crate produces `Signal(_)` (from a back-end) and `Forced` (hooks
started in the force phase). `Requested`, `Timeout` and `Error` are
labels for callers.

| Method          | Result                                                        |
| --------------- | ------------------------------------------------------------- |
| `description()` | `"signal"`, `"requested"`, `"forced"`, `"timeout"`, `"error"` |
| `is_signal()`   | `true` for `Signal(_)`                                        |
| `signal()`      | the signal, if any                                            |
| `exit_code()`   | `Signal(s)` gives `s.exit_code()`; `Requested` 0; others 1    |

```rust
use signal_mod::{ShutdownReason, Signal};

let reason = ShutdownReason::Signal(Signal::Terminate);
assert_eq!(reason.exit_code(), 143);
assert_eq!(reason.signal(), Some(Signal::Terminate));
assert_eq!(ShutdownReason::Requested.exit_code(), 0);
assert_eq!(reason.to_string(), "Signal(Terminate (SIGTERM / CTRL_CLOSE_EVENT))");
```

---

## `ShutdownToken`

Cloneable observer.

| Method                                         | Behavior                                                       |
| ---------------------------------------------- | -------------------------------------------------------------- |
| `is_initiated()`                               | `Acquire` load; `true` implies `reason().is_some()`            |
| `reason()` / `elapsed()`                       | lock-free; `Some` exactly when initiated                       |
| `wait()` / `cancelled()` / `.await`            | [`ShutdownFuture`](#shutdownfuture); any executor, no polling  |
| `wait_blocking()` / `wait_blocking_timeout(d)` | park the thread (condition variable)                           |
| `child()`                                      | `(ShutdownTrigger, ShutdownToken)` of a child scope            |
| `subsystem_guard()`                            | [`SubsystemGuard`](#subsystemguard) keeping the scope busy     |
| `active_subsystems()`                          | live guards in this scope and its children                     |
| `subsystems_finished()`                        | [`SubsystemsFinished`](#subsystemsfinished) future             |
| `wait_subsystems_blocking(d)`                  | block until no guard is alive, at most `d`                     |
| `cancellation_token()` (`tokio-util`)          | a `CancellationToken` cancelled on initiation                  |

A **child scope** is initiated when its parent is (same reason) or by its
own trigger, without affecting the parent: one connection, one job, one
tenant. Guards taken in a child scope also count in every ancestor, so
the root's `subsystems_finished()` waits for the whole tree.

```rust
# #[tokio::main(flavor = "current_thread")]
# async fn main() {
use std::time::Duration;
use signal_mod::{Coordinator, ShutdownReason};

let coord = Coordinator::builder().build();
let token = coord.token();

// A worker that registers itself and stops on shutdown.
let worker = tokio::spawn({
    let token = token.clone();
    let guard = token.subsystem_guard();
    async move {
        token.wait().await;
        drop(guard); // finished draining
    }
});

let (job_trigger, job) = token.child();
job_trigger.trigger(ShutdownReason::Requested); // stops the job only
assert!(job.is_initiated() && !token.is_initiated());

coord.trigger().trigger(ShutdownReason::Requested);
tokio::time::timeout(Duration::from_secs(5), token.subsystems_finished())
    .await
    .expect("worker drained");
worker.await.unwrap();
# }
```

### `ShutdownFuture`

Returned by `wait`, `cancelled` and `IntoFuture` for `ShutdownToken` and
`&ShutdownToken`. It owns a clone of the token, so it is `'static` and
`Send`: hand it to `tokio::spawn`, `select!`, or
`axum::serve(..).with_graceful_shutdown(..)`. The trigger wakes it
directly; dropping it unregisters its waker.

```rust
# #[tokio::main(flavor = "current_thread")]
# async fn main() {
use signal_mod::{Coordinator, ShutdownReason};

let coord = Coordinator::builder().build();
let token = coord.token();
let by_value = tokio::spawn(token.clone().into_future());
coord.trigger().trigger(ShutdownReason::Requested);
by_value.await.unwrap();
(&token).await; // already initiated: ready immediately
# }
```

### `SubsystemGuard`

Counts as one running subsystem in its scope (and its ancestors) until
dropped.

### `SubsystemsFinished`

Future resolving once no guard of the scope is alive. Runtime-agnostic.

---

## `ShutdownTrigger`

| Method                    | Behavior                                                                  |
| ------------------------- | ------------------------------------------------------------------------- |
| `trigger(reason) -> bool` | First call initiates (returns `true`), wakes every waiter, initiates every child scope. Later calls return `false`. |
| `is_initiated()`          | as on the token                                                           |
| `token()`                 | observer for the same scope                                               |

```rust
use signal_mod::{Coordinator, ShutdownReason, ShutdownTrigger};

fn on_fatal_error(trigger: &ShutdownTrigger, err: &dyn std::error::Error) {
    eprintln!("fatal: {err}");
    trigger.trigger(ShutdownReason::Error);
}

let coord = Coordinator::builder().build();
on_fatal_error(&coord.trigger(), &std::io::Error::other("disk gone"));
assert_eq!(coord.token().reason(), Some(ShutdownReason::Error));
```

---

## `ShutdownHook`

```rust,ignore
pub trait ShutdownHook: Send + Sync + 'static {
    fn name(&self) -> &str;
    fn priority(&self) -> i32 { 0 }
    fn timeout(&self) -> Option<Duration> { None }
    fn run(&self, reason: ShutdownReason);
}
```

Contract:

- `run` is called **at most once** per coordinator.
- It runs on a worker thread named `signal-mod-hook`, not the caller's
  thread. With the `tokio` feature, a Tokio runtime current on the
  calling thread is entered there, so async cleanup can use
  `tokio::runtime::Handle::current().block_on(..)`; call
  `run_hooks_async` from async code so the runtime stays free to drive
  it.
- `timeout()` bounds this hook; past it (or past the phase deadline) the
  hook is reported `TimedOut` and abandoned while the run continues. An
  abandoned hook keeps running on its thread until it returns, holding
  whatever locks it took: keep critical sections in hooks short, or
  check [`HookContext`](#hookcontext) between steps.
- A panic is caught and reported with its message.

```rust
use std::time::Duration;
use signal_mod::{Coordinator, ShutdownHook, ShutdownReason};

struct FlushCache;

impl ShutdownHook for FlushCache {
    fn name(&self) -> &str { "flush-cache" }
    fn priority(&self) -> i32 { 50 }
    fn timeout(&self) -> Option<Duration> { Some(Duration::from_secs(2)) }
    fn run(&self, reason: ShutdownReason) {
        println!("flushing ({reason})");
    }
}

let coord = Coordinator::builder().hook(FlushCache).build();
assert_eq!(coord.run_hooks(ShutdownReason::Requested), 1);
```

## `HookContext`

`HookContext::current()` returns the context of the hook running on the
calling thread (`None` outside a hook): `deadline()`, `remaining()` and
`is_abandoned()`. Hooks cannot be stopped from outside; this is how a
long hook cooperates with the deadline.

```rust
use std::time::Duration;
use signal_mod::{Coordinator, HookContext, ShutdownReason, hook_from_fn};

let coord = Coordinator::builder()
    .hook(
        hook_from_fn("flush-batches", 0, |_| {
            let ctx = HookContext::current().expect("inside a hook");
            for _batch in 0..3 {
                if ctx.is_abandoned() {
                    return;
                }
                // flush one batch ...
            }
        })
        .with_timeout(Duration::from_secs(5)),
    )
    .build();
assert_eq!(coord.run_hooks(ShutdownReason::Requested), 1);
```

## `FnHook`

Closure-backed hook: `FnHook::new(name, priority, f)`, plus
`.with_timeout(d)`.

## `hook_from_fn`

`hook_from_fn(name, priority, f) -> FnHook<F>`.

```rust
use std::time::Duration;
use signal_mod::{ShutdownHook, hook_from_fn};

let hook = hook_from_fn("close-db", 200, |_| {}).with_timeout(Duration::from_secs(1));
assert_eq!(hook.name(), "close-db");
assert_eq!(hook.priority(), 200);
assert_eq!(hook.timeout(), Some(Duration::from_secs(1)));
```

---

## `Coordinator`

| Method                                   | Behavior                                                             |
| ---------------------------------------- | -------------------------------------------------------------------- |
| `builder()`                              | [`CoordinatorBuilder`](#coordinatorbuilder)                           |
| `token()` / `trigger()`                  | handles for the root scope                                            |
| `install() -> Result<()>`                | install listeners for the shutdown and notify sets (see below)        |
| `uninstall() -> bool`                    | remove them; default actions return. Also done on drop.               |
| `is_installed()`, `backend()`, `installed_signals()` | installation state                                       |
| `subscribe()`                            | [`SignalSubscription`](#signalsubscription)                           |
| `simulate_signal(sig) -> bool`           | deliver as a back-end would (tests, forwarding)                       |
| `run_hooks(reason) -> usize`             | blocking run; number of hooks completed                               |
| `run_hooks_with_report(reason)`          | blocking run; [`RunReport`](#runreport)                               |
| `run_hooks_async(reason)`                | [`RunHooksFuture`](#runhooksfuture); never blocks the executor        |
| `statistics()`                           | [`Statistics`](#statistics); never blocks on a running hook           |
| `signals()`, `notify_signals()`, `graceful_timeout()`, `force_timeout()`, `force_exit_on_second_signal()` | configuration |

### `install`

1. `tokio` enabled and a Tokio runtime current: Tokio listener tasks.
2. Otherwise, `agnostic` enabled: listener thread or console handler.
3. Otherwise `Error::OutsideTokioRuntime` (only `tokio` enabled) or
   `Error::NoRuntime` (no back-end).

Installation is all-or-nothing. Several coordinators can be installed
at once, through either back-end, and every one receives its signals
(on Windows both back-ends share one console control handler for that
reason).

After the last coordinator covering a signal is uninstalled or dropped:

| Situation (Unix) | What happens the next time the signal arrives |
| ---------------- | --------------------------------------------- |
| shutdown signal that was at its default action when signal-mod first installed it | the default action: the process terminates (SIGQUIT dumps core). This also applies if other code registered its own handler for that signal after signal-mod did. |
| signal that was ignored or already handled before signal-mod (SIGHUP under `nohup`, another library's handler) | left to that earlier owner |
| notify-only signal, or SIGPIPE | absorbed (ignored), as with `tokio::signal` |

On Windows the console handler declines events nobody covers, and the
next handler (ultimately the OS default, terminate) runs.

`install` never panics with unwinding panics. One caveat: with the Tokio
back-end on Unix and a runtime built without its I/O driver, Tokio panics
inside `install`; signal-mod catches the unwind and returns
`Error::SignalRegistration`, but the panic hook still runs, and a
`panic = "abort"` binary aborts. `#[tokio::main]` and `Runtime::new`
enable the driver.

The coordinator is the installation guard: keep it alive (normally for
all of `main`).

```rust,no_run
use signal_mod::{Coordinator, ShutdownReason};

#[tokio::main]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder().force_exit_on_second_signal(true).build();
    coord.install()?;

    let token = coord.token();
    token.wait().await;

    let reason = token.reason().unwrap_or(ShutdownReason::Requested);
    let report = coord.run_hooks_async(reason).await;
    eprintln!("{report}");
    std::process::exit(reason.exit_code());
}
```

Without a runtime (feature `agnostic`):

```rust,no_run
use signal_mod::{Coordinator, ShutdownReason};

fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder().build();
    coord.install()?;
    coord.token().wait_blocking();
    let reason = coord.token().reason().unwrap_or(ShutdownReason::Requested);
    coord.run_hooks(reason);
    Ok(())
}
```

---

## `CoordinatorBuilder`

| Method                              | Default                 | Meaning                                               |
| ----------------------------------- | ----------------------- | ----------------------------------------------------- |
| `signals(set)`                      | `SignalSet::graceful()` | signals that initiate shutdown                        |
| `notify_signals(set)`               | empty                   | signals delivered to subscribers only                 |
| `graceful_timeout(d)`               | 5 s                     | graceful phase budget, from the start of the hook run |
| `force_timeout(d)`                  | 10 s                    | total budget for the run, including the force phase   |
| `force_exit_on_second_signal(bool)` | `false`                 | a second shutdown signal ends the process (re-raised on Unix, `exit_code()` on Windows) |
| `hook(h)`                           | none                    | register a hook                                       |
| `build()`                           |                         | sort hooks once, create the coordinator               |

```rust
use std::time::Duration;
use signal_mod::{Coordinator, Signal, SignalSet, hook_from_fn};

let coord = Coordinator::builder()
    .signals(SignalSet::standard() - Signal::Hangup)
    .notify_signals(SignalSet::from(Signal::Hangup))
    .graceful_timeout(Duration::from_secs(3))
    .force_timeout(Duration::from_secs(4))
    .force_exit_on_second_signal(true)
    .hook(hook_from_fn("close-db", 200, |_| {}))
    .build();
assert_eq!(coord.graceful_timeout(), Duration::from_secs(3));
```

---

## Running hooks: the deadline ladder

Hooks are sorted once at `build()` by descending priority (stable: ties
keep registration order). A run then goes through:

1. **Graceful phase.** Starts hooks with the caller's reason until
   `graceful_timeout` has elapsed since the first hook started.
2. **Force phase.** Starts the remaining hooks with
   `ShutdownReason::Forced` until `force_timeout` has elapsed since the
   first hook started. `force_timeout <= graceful_timeout` disables it.

The clock starts when the worker is about to start the first hook, so
the first hook always starts in the graceful phase (unless that budget
is zero), however long the worker thread took to be scheduled.
3. **Skipped.** Hooks not started when the force budget is gone.

Each hook runs on a worker thread; the run waits for it until the
earliest of its own `timeout()` and the current phase's deadline. On
expiry the hook is `TimedOut` and abandoned (its thread keeps running
it), and the next hook starts on a fresh worker. So a run returns within
`force_timeout` (plus scheduling noise) whatever the hooks do.
`Duration::MAX` budgets disable the deadlines.

**Run-once.** The first `run_hooks*` call runs the ladder. Every other
call, concurrent or later, sync or async, waits for that run and returns
its report. No hook body ever runs twice.

**Re-entrancy.** Nothing is locked while a hook runs: a hook may call
`statistics()`, format the coordinator, or trigger shutdown. A hook
should not call `run_hooks` (it would wait for the run that is waiting
for it, until the hook's deadline).

```rust
use std::time::Duration;
use signal_mod::{Coordinator, HookPhase, HookStatus, ShutdownReason, hook_from_fn};

let coord = Coordinator::builder()
    .graceful_timeout(Duration::from_millis(200))
    .force_timeout(Duration::from_secs(20))
    .hook(hook_from_fn("hangs", 10, |_| std::thread::sleep(Duration::from_secs(30))))
    .hook(hook_from_fn("flush", 0, |reason| assert_eq!(reason, ShutdownReason::Forced)))
    .build();

let report = coord.run_hooks_with_report(ShutdownReason::Requested);
assert!(report.elapsed() < Duration::from_secs(20));
assert_eq!(report.outcomes()[0].status, HookStatus::TimedOut);
assert_eq!(report.outcomes()[1].status, HookStatus::Completed);
assert_eq!(report.outcomes()[1].phase, Some(HookPhase::Force));
```

---

## `RunReport`

`#[non_exhaustive]`, `Clone`, `PartialEq`, `Display`.

| Method                                                              | Result                                 |
| ------------------------------------------------------------------- | -------------------------------------- |
| `reason()`                                                          | reason passed to the run               |
| `outcomes() -> &[HookOutcome]`                                      | one entry per hook, execution order    |
| `elapsed()`                                                         | run duration                           |
| `completed()`, `panicked()`, `timed_out()`, `skipped()`, `forced()` | counts                                 |
| `is_clean()`                                                        | every hook completed                   |

## `HookOutcome`

`#[non_exhaustive]` struct with public fields: `name: Arc<str>`,
`priority`, `status: HookStatus`, `phase: Option<HookPhase>`,
`duration: Option<Duration>`, `panic_message: Option<String>`.

## `HookStatus`

`Pending`, `Running`, `Completed`, `Panicked`, `TimedOut`, `Skipped`
(`#[non_exhaustive]`); `is_finished()` is `true` for the last four.

## `HookPhase`

`Graceful` or `Force` (`#[non_exhaustive]`).

## `RunHooksFuture`

Returned by `run_hooks_async`. The first poll starts the run on a
`signal-mod-shutdown` supervisor thread (or joins a run already in
progress); it resolves with the `RunReport`. `'static`, `Send`, any
executor. Dropping it does not cancel a started run.

```rust
# #[tokio::main]
# async fn main() {
use signal_mod::{Coordinator, ShutdownReason, hook_from_fn};

let coord = Coordinator::builder()
    .hook(hook_from_fn("boom", 0, |_| panic!("disk on fire")))
    .build();
let report = coord.run_hooks_async(ShutdownReason::Requested).await;
assert_eq!(report.panicked(), 1);
assert_eq!(report.outcomes()[0].panic_message.as_deref(), Some("disk on fire"));
println!("{report}");
# }
```

---

## `Statistics`

`#[non_exhaustive]` snapshot with public fields: `initiated`, `reason`,
`elapsed`, `hooks_registered`, `hooks_completed`, `hooks_panicked`,
`hooks_timed_out`, `hooks_skipped`, `hooks: Vec<HookOutcome>` (live
per-hook status and timings), `installed`, `active_subsystems`.

`initiated`, `reason` and `elapsed` come from one atomic publication
(`initiated == reason.is_some()` always holds). Per-hook entries are read
one at a time from atomics, so `statistics()` never waits for a running
hook.

```rust
use signal_mod::{Coordinator, HookStatus, ShutdownReason, hook_from_fn};

let coord = Coordinator::builder().hook(hook_from_fn("h", 0, |_| {})).build();
assert_eq!(coord.statistics().hooks[0].status, HookStatus::Pending);
coord.run_hooks(ShutdownReason::Requested);
let stats = coord.statistics();
assert_eq!(stats.hooks_completed, 1);
assert!(stats.hooks[0].duration.is_some());
```

---

## `SignalSubscription`

Created by `Coordinator::subscribe()`. Sees every delivered signal
(shutdown and notify sets) from the moment it is created. Like the OS,
it coalesces repeats of a signal that arrive before you receive it;
distinct pending signals come out in canonical order. Memory is one bit
per signal; nothing is dropped silently.

| Method            | Behavior                                               |
| ----------------- | ------------------------------------------------------ |
| `recv()`          | [`Recv`](#recv) future; `None` once closed and drained |
| `try_recv()`      | pending signal, if any                                 |
| `recv_blocking()` | block until a signal arrives (or closed)               |
| `recv_timeout(d)` | block at most `d`                                      |
| `is_closed()`     | the coordinator and its listeners are gone             |

```rust
use signal_mod::{Coordinator, Signal, SignalSet};

let coord = Coordinator::builder()
    .signals(SignalSet::graceful() - Signal::Hangup)
    .notify_signals(SignalSet::empty().with(Signal::Hangup).with(Signal::User1))
    .build();
let mut events = coord.subscribe();

coord.simulate_signal(Signal::User1);
coord.simulate_signal(Signal::Hangup);
coord.simulate_signal(Signal::Hangup); // coalesced
assert_eq!(events.try_recv(), Some(Signal::Hangup));
assert_eq!(events.try_recv(), Some(Signal::User1));
assert_eq!(events.try_recv(), None);
assert!(!coord.token().is_initiated());
```

With a runtime, reload and shutdown fit one `select!` loop:

```rust,no_run
use signal_mod::{Coordinator, Signal, SignalSet};

#[tokio::main]
async fn main() -> signal_mod::Result<()> {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful() - Signal::Hangup)
        .notify_signals(SignalSet::from(Signal::Hangup))
        .build();
    coord.install()?;
    let token = coord.token();
    let mut events = coord.subscribe();
    loop {
        tokio::select! {
            () = token.wait() => break,
            Some(Signal::Hangup) = events.recv() => println!("reload"),
        }
    }
    Ok(())
}
```

## `Recv`

Future returned by `SignalSubscription::recv`.

---

## `Backend`

`Tokio` or `Agnostic` (`#[non_exhaustive]`), as reported by
`Coordinator::backend()`.

```rust
use signal_mod::{Coordinator, SignalSet};

let coord = Coordinator::builder().signals(SignalSet::empty()).build();
assert_eq!(coord.backend(), None);
```

---

## `shutdown_signal`

`shutdown_signal() -> Result<ShutdownSignal>` installs handlers for
`SignalSet::graceful()` and returns a future that resolves on the first
one. It is made for `with_graceful_shutdown` APIs (axum, hyper, tonic).
The handlers live inside the returned value; once the server drops it,
the default action is back, so a second Ctrl+C terminates even if
draining hangs.

```rust,no_run
# async fn serve() -> std::io::Result<()> {
let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
axum::serve(listener, axum::Router::new())
    .with_graceful_shutdown(signal_mod::shutdown_signal().map_err(std::io::Error::other)?)
    .await
# }
```

## `ShutdownSignal`

The future returned by `shutdown_signal`; also offers `reason()` and
`token()`.

---

## `Error`

`#[non_exhaustive]`; implements `std::error::Error` (with `source()` for
the OS error) and `Display`.

| Variant                                 | Meaning and what to do                                                      |
| --------------------------------------- | --------------------------------------------------------------------------- |
| `AlreadyInstalled`                      | this coordinator is installed; `uninstall()` first to replace it             |
| `SignalRegistration { signal, source }` | the OS rejected `signal`, or the Tokio runtime has no I/O driver (`enable_all`) |
| `NoRuntime`                             | no back-end feature enabled; enable `tokio` or `agnostic`                    |
| `OutsideTokioRuntime`                   | only `tokio` enabled and called outside a runtime; call inside it or enable `agnostic` |
| `ListenerThread(io::Error)`             | the listener could not start (Unix thread or self-pipe, Windows console handler registration); retry |
| `InvalidState`, `Timeout`               | deprecated since 2.0.0, never produced, removed in 3.0                       |

```rust
use signal_mod::{Coordinator, Error};

let coord = Coordinator::builder().build();
match coord.install() {
    Ok(()) => assert!(coord.uninstall()),
    Err(Error::OutsideTokioRuntime | Error::NoRuntime) => {} // no back-end usable here
    Err(other) => panic!("unexpected: {other}"),
}
```

## `Result`

`pub type Result<T> = core::result::Result<T, Error>;`

## `VERSION`

```rust
assert_eq!(signal_mod::VERSION, env!("CARGO_PKG_VERSION"));
```

---

## Platform behavior

**Unix.** Numbers come from `libc` for the compiled target. The Tokio
back-end uses `tokio::signal::unix`; the agnostic back-end a signal-hook
iterator thread. Neither library ever removes an OS handler, so for
shutdown signals that were at their default action before signal-mod
installed them, signal-mod arms a conditional default action that takes
over once the last coordinator covering the signal is gone (see
[`install`](#install)). A second-signal escalation re-raises the signal
with its default action, so the parent sees a death by that signal.

**Windows.** Both back-ends use one shared console control handler.
Windows calls console handlers last-registered first and stops at the
first that claims the event, so a handler registered by other code after
signal-mod's sees events first (and can hide them from it).
`Interrupt` is Ctrl+C, `Quit` Ctrl+Break, `Terminate` the
console window closing, `Hangup` logoff or system shutdown (delivered to
services only). After `CTRL_CLOSE_EVENT` the OS terminates the process
about 5 seconds later; both back-ends hold the OS handler thread so that
window is usable, so keep `force_timeout` under it if console close
matters. `Pipe`, `User1` and `User2` are never delivered and are left out
of `installed_signals()`.

**Other targets.** `install` succeeds with an empty `installed_signals()`.

---

## Performance

`cargo bench --bench shutdown_bench`; numbers and conditions are in the
[README](../README.md#performance). The observer hot paths
(`is_initiated`, `reason`, polling a ready `wait()` future) are
lock-free loads. A coordinator with hooks parks one standby worker
thread when it is built, so a hook run does not pay for a thread spawn:
it costs one hand-off to that thread and one wake-up of the caller, once
per coordinator run. Hooks that finish in time add no context switch
each.

---

Copyright (C) 2026 James Gober. Licensed under Apache-2.0 OR MIT.
