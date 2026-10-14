//! Hook execution: the graceful / force deadline ladder.
//!
//! # Model
//!
//! Hooks are sorted once, at build time, by descending priority (stable,
//! so registration order breaks ties). A run walks them in that order:
//!
//! 1. **Graceful phase.** Hooks start with the caller's reason until
//!    `graceful_timeout` (measured from the start of the run) elapses.
//! 2. **Force phase.** Hooks that had not started by then start with
//!    [`ShutdownReason::Forced`] until `force_timeout` (also measured from
//!    the start of the run) elapses. With `force_timeout <=
//!    graceful_timeout` there is no force phase.
//! 3. Hooks still not started when the force budget is gone are
//!    `Skipped`.
//!
//! # Execution
//!
//! A worker thread runs the hooks back to back and publishes which hook
//! it is running, and that hook's deadline (the earlier of the hook's own
//! timeout and the end of the phase it started in), in a small
//! mutex-protected [`Progress`] record. The supervisor (the caller of
//! `run_hooks`, or a dedicated thread for `run_hooks_async`) sleeps until
//! that deadline or until the worker finishes. Fast hooks therefore cost
//! no context switch each; the supervisor only wakes when a deadline
//! passes, when a hook with an earlier deadline starts, or at the end.
//!
//! When a deadline passes, the supervisor marks the hook `TimedOut`,
//! retires the worker (bumping the generation, so the worker stops as
//! soon as its hook returns and never touches another slot) and starts a
//! fresh worker at the next hook. Abandoning the thread is the only way
//! to bound arbitrary user code without `unsafe` thread cancellation.
//!
//! A hook slot moves `Pending -> Running -> (finishing) -> terminal`. The
//! worker and the supervisor race to finish a running slot with a
//! compare-and-swap into the internal `FINISHING` state; only the winner
//! writes the duration and final status, so a hook that returns at the
//! instant its deadline passes is reported exactly once.
//!
//! # Run-once
//!
//! The first caller runs the ladder; every other caller (concurrent or
//! later, sync or async) waits for that run and receives the same
//! [`RunReport`]. So every hook's `run` executes at most once.
//!
//! Nothing here holds a lock while user code runs, which is what lets
//! `Coordinator::statistics` read live progress from inside a hook or
//! from a monitoring thread without blocking.

use core::fmt;
use core::future::Future;
use core::pin::Pin;
use core::task::{Context, Poll};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::hook::ShutdownHook;
use crate::reason::ShutdownReason;
use crate::report::{HookOutcome, HookPhase, HookStatus, RunReport};
use crate::waitlist::{WaitKey, WaitList, wake_all};

const PHASE_NONE: u8 = 0;
const PHASE_GRACEFUL: u8 = 1;
const PHASE_FORCE: u8 = 2;
const NO_DURATION: u64 = u64::MAX;
/// Internal slot state between `Running` and a terminal state, owned by
/// whichever of worker / supervisor won the compare-and-swap. Reported
/// as `Running`.
const FINISHING: u8 = 0xFF;

/// Recover a guard from a poisoned std mutex. No user code runs under
/// the runner's locks, so the data is consistent.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait<'a, T>(cv: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    match cv.wait(guard) {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_until<'a, T>(
    cv: &Condvar,
    guard: MutexGuard<'a, T>,
    deadline: Instant,
) -> MutexGuard<'a, T> {
    let left = deadline.saturating_duration_since(Instant::now());
    match cv.wait_timeout(guard, left) {
        Ok((guard, _)) => guard,
        Err(poisoned) => poisoned.into_inner().0,
    }
}

fn earliest(a: Option<Instant>, b: Option<Instant>) -> Option<Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

/// The caller's async-runtime context, re-entered on hook threads.
#[derive(Clone, Default)]
pub(crate) struct RuntimeContext {
    #[cfg(feature = "tokio")]
    handle: Option<tokio::runtime::Handle>,
}

impl RuntimeContext {
    /// Capture the runtime the current thread is running in, if any.
    pub(crate) fn capture() -> Self {
        Self {
            #[cfg(feature = "tokio")]
            handle: tokio::runtime::Handle::try_current().ok(),
        }
    }

    #[cfg(feature = "tokio")]
    fn enter(&self) -> Option<tokio::runtime::EnterGuard<'_>> {
        self.handle.as_ref().map(tokio::runtime::Handle::enter)
    }

    #[cfg(not(feature = "tokio"))]
    #[allow(clippy::unused_self)] // mirrors the tokio variant's signature
    fn enter(&self) -> Option<()> {
        None
    }
}

/// One registered hook plus its live progress.
pub(crate) struct HookSlot {
    hook: Arc<dyn ShutdownHook>,
    name: Arc<str>,
    priority: i32,
    timeout: Option<Duration>,
    status: AtomicU8,
    phase: AtomicU8,
    duration_nanos: AtomicU64,
    panic_message: Mutex<Option<String>>,
}

impl HookSlot {
    fn new(hook: Box<dyn ShutdownHook>) -> Self {
        let hook: Arc<dyn ShutdownHook> = Arc::from(hook);
        Self {
            name: Arc::from(hook.name()),
            priority: hook.priority(),
            timeout: hook.timeout(),
            hook,
            status: AtomicU8::new(HookStatus::Pending.to_u8()),
            phase: AtomicU8::new(PHASE_NONE),
            duration_nanos: AtomicU64::new(NO_DURATION),
            panic_message: Mutex::new(None),
        }
    }

    fn raw_status(&self) -> u8 {
        self.status.load(Ordering::Acquire)
    }

    fn mark_running(&self, phase: HookPhase) {
        let phase = match phase {
            HookPhase::Graceful => PHASE_GRACEFUL,
            HookPhase::Force => PHASE_FORCE,
        };
        self.phase.store(phase, Ordering::Relaxed);
        self.status
            .store(HookStatus::Running.to_u8(), Ordering::Release);
    }

    fn mark_skipped(&self) {
        self.status
            .store(HookStatus::Skipped.to_u8(), Ordering::Release);
    }

    /// Try to finish a running hook. Returns `false` if the other party
    /// (worker or supervisor) already finished it.
    fn try_finish(&self, status: HookStatus, duration: Duration, panic: Option<String>) -> bool {
        if self
            .status
            .compare_exchange(
                HookStatus::Running.to_u8(),
                FINISHING,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return false;
        }
        if panic.is_some() {
            *lock(&self.panic_message) = panic;
        }
        let nanos = u64::try_from(duration.as_nanos()).unwrap_or(NO_DURATION - 1);
        self.duration_nanos.store(nanos, Ordering::Relaxed);
        self.status.store(status.to_u8(), Ordering::Release);
        true
    }

    pub(crate) fn outcome(&self) -> HookOutcome {
        let raw = self.raw_status();
        let status = if raw == FINISHING {
            HookStatus::Running
        } else {
            HookStatus::from_u8(raw)
        };
        let phase = match self.phase.load(Ordering::Relaxed) {
            PHASE_GRACEFUL => Some(HookPhase::Graceful),
            PHASE_FORCE => Some(HookPhase::Force),
            _ => None,
        };
        let nanos = self.duration_nanos.load(Ordering::Relaxed);
        let finished = status.is_finished();
        HookOutcome {
            name: Arc::clone(&self.name),
            priority: self.priority,
            status,
            phase,
            duration: (finished && nanos != NO_DURATION).then(|| Duration::from_nanos(nanos)),
            panic_message: lock(&self.panic_message).clone(),
        }
    }
}

/// Run one hook body, converting a panic into a reportable result.
fn execute(hook: &dyn ShutdownHook, reason: ShutdownReason) -> (HookStatus, Option<String>) {
    match catch_unwind(AssertUnwindSafe(|| hook.run(reason))) {
        Ok(()) => (HookStatus::Completed, None),
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned());
            (HookStatus::Panicked, message)
        }
    }
}

/// Deadlines of one run. The clock starts when the first worker is
/// about to start the first hook, not when `run_hooks` is called, so
/// thread start-up and scheduling delay before the first hook never eat
/// into the graceful budget; the first hook always starts in the graceful
/// phase (unless that budget is zero).
#[derive(Clone, Copy)]
struct Plan {
    reason: ShutdownReason,
    graceful_deadline: Option<Instant>,
    force_deadline: Option<Instant>,
}

impl Plan {
    fn starting_now(reason: ShutdownReason, graceful: Duration, force: Duration) -> Self {
        let start = Instant::now();
        Self {
            reason,
            graceful_deadline: start.checked_add(graceful),
            force_deadline: start.checked_add(force.max(graceful)),
        }
    }
}

impl Plan {
    /// Phase, phase deadline and reason for a hook starting at `now`;
    /// `None` once the force budget is exhausted.
    fn at(&self, now: Instant) -> Option<(HookPhase, Option<Instant>, ShutdownReason)> {
        if self.graceful_deadline.is_none_or(|d| now < d) {
            Some((HookPhase::Graceful, self.graceful_deadline, self.reason))
        } else if self.force_deadline.is_none_or(|d| now < d) {
            Some((
                HookPhase::Force,
                self.force_deadline,
                ShutdownReason::Forced,
            ))
        } else {
            None
        }
    }
}

/// The hook a worker is running.
#[derive(Clone)]
struct Current {
    index: usize,
    started: Instant,
    control: Arc<HookControl>,
}

/// Per-hook state shared with the hook itself through
/// [`HookContext`](crate::HookContext).
#[derive(Debug)]
pub(crate) struct HookControl {
    pub(crate) deadline: Option<Instant>,
    pub(crate) abandoned: AtomicBool,
}

thread_local! {
    /// The hook running on this worker thread, for `HookContext::current`.
    static CURRENT_HOOK: core::cell::RefCell<Option<Arc<HookControl>>> =
        const { core::cell::RefCell::new(None) };
}

/// The control block of the hook running on the calling thread.
pub(crate) fn current_hook() -> Option<Arc<HookControl>> {
    CURRENT_HOOK.with(|cell| cell.borrow().clone())
}

/// Sets the thread's current hook for the lifetime of the guard.
struct CurrentHookGuard;

impl CurrentHookGuard {
    fn set(control: &Arc<HookControl>) -> Self {
        CURRENT_HOOK.with(|cell| *cell.borrow_mut() = Some(Arc::clone(control)));
        Self
    }
}

impl Drop for CurrentHookGuard {
    fn drop(&mut self) {
        CURRENT_HOOK.with(|cell| *cell.borrow_mut() = None);
    }
}

/// Shared between the supervisor and the current worker of one run.
struct Progress {
    /// Bumped whenever the supervisor retires a worker.
    generation: u64,
    current: Option<Current>,
    /// The current generation's worker reached the end of the list.
    finished: bool,
    /// What the supervisor sleeps until (`None`: no deadline).
    supervisor_wakes_at: Option<Instant>,
}

struct Run {
    reason: ShutdownReason,
    graceful: Duration,
    force: Duration,
    clock: OnceLock<Plan>,
    progress: Mutex<Progress>,
    changed: Condvar,
}

impl Run {
    /// The run's deadlines, starting the clock on first use.
    fn plan(&self) -> &Plan {
        self.clock
            .get_or_init(|| Plan::starting_now(self.reason, self.graceful, self.force))
    }
}

/// Work handed to the standby worker.
struct Assignment {
    runner: Arc<HookRunner>,
    run: Arc<Run>,
    generation: u64,
    from: usize,
    ctx: RuntimeContext,
}

enum RunState {
    Idle,
    Running(WaitList),
    Done(RunReport),
}

/// Owns the hooks and the run-once state of one coordinator.
pub(crate) struct HookRunner {
    slots: Box<[HookSlot]>,
    graceful: Duration,
    force: Duration,
    state: Mutex<RunState>,
    done: Condvar,
    /// A parked worker thread started at build time, so that running the
    /// hooks does not pay for a thread spawn. `None` without hooks, once
    /// used, or if it could not be spawned.
    standby: Mutex<Option<mpsc::Sender<Assignment>>>,
}

impl fmt::Debug for HookRunner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HookRunner")
            .field("hooks", &self.slots.len())
            .field("graceful", &self.graceful)
            .field("force", &self.force)
            .finish_non_exhaustive()
    }
}

/// Start a parked worker that runs one assignment, or exits when the
/// runner (and with it the sender) is dropped first.
fn spawn_standby() -> Option<mpsc::Sender<Assignment>> {
    let (tx, rx) = mpsc::channel::<Assignment>();
    let spawned = thread::Builder::new()
        .name("signal-mod-hook".into())
        .spawn(move || {
            if let Ok(job) = rx.recv() {
                let _runtime = job.ctx.enter();
                job.runner.work(&job.run, job.generation, job.from);
            }
        });
    match spawned {
        Ok(_detached) => Some(tx),
        Err(error) => {
            sm_warn!(error = %error, "could not pre-spawn the hook worker; it will start on demand");
            drop(error);
            None
        }
    }
}

impl HookRunner {
    pub(crate) fn new(
        mut hooks: Vec<Box<dyn ShutdownHook>>,
        graceful: Duration,
        force: Duration,
    ) -> Self {
        hooks.sort_by_key(|h| core::cmp::Reverse(h.priority()));
        let standby = if hooks.is_empty() {
            None
        } else {
            spawn_standby()
        };
        Self {
            slots: hooks.into_iter().map(HookSlot::new).collect(),
            graceful,
            force,
            state: Mutex::new(RunState::Idle),
            done: Condvar::new(),
            standby: Mutex::new(standby),
        }
    }

    pub(crate) fn slots(&self) -> &[HookSlot] {
        &self.slots
    }

    /// Run the hooks with this thread as supervisor (or wait for the run
    /// another caller started) and return the report.
    pub(crate) fn run_blocking(
        self: &Arc<Self>,
        reason: ShutdownReason,
        ctx: &RuntimeContext,
    ) -> RunReport {
        {
            let mut state = lock(&self.state);
            loop {
                match &*state {
                    RunState::Idle => {
                        *state = RunState::Running(WaitList::default());
                        break;
                    }
                    RunState::Running(_) => state = wait(&self.done, state),
                    RunState::Done(report) => return report.clone(),
                }
            }
        }
        let report = self.supervise(reason, ctx);
        self.publish(report.clone());
        report
    }

    /// Store the finished report and wake every waiter.
    fn publish(&self, report: RunReport) {
        let wakers = {
            let mut state = lock(&self.state);
            match core::mem::replace(&mut *state, RunState::Done(report)) {
                RunState::Running(mut waiters) => waiters.take_all(),
                RunState::Idle | RunState::Done(_) => Vec::new(),
            }
        };
        self.done.notify_all();
        wake_all(wakers);
    }

    /// Execute the ladder. Called by exactly one caller per runner.
    fn supervise(self: &Arc<Self>, reason: ShutdownReason, ctx: &RuntimeContext) -> RunReport {
        let start = Instant::now();
        sm_debug!(hooks = self.slots.len(), %reason, "running shutdown hooks");
        if !self.slots.is_empty() {
            let run = Arc::new(Run {
                reason,
                graceful: self.graceful,
                force: self.force,
                clock: OnceLock::new(),
                progress: Mutex::new(Progress {
                    generation: 0,
                    current: None,
                    finished: false,
                    supervisor_wakes_at: None,
                }),
                changed: Condvar::new(),
            });
            self.drive(&run, ctx);
        }
        let outcomes = self.slots.iter().map(HookSlot::outcome).collect();
        let report = RunReport::new(reason, outcomes, start.elapsed());
        sm_debug!(%report, "shutdown hooks finished");
        report
    }

    /// The supervisor loop: start a worker, enforce deadlines, replace
    /// the worker after each timeout, until the list is done.
    fn drive(self: &Arc<Self>, run: &Arc<Run>, ctx: &RuntimeContext) {
        if !self.spawn_worker(run, 0, 0, ctx) {
            return;
        }
        let mut progress = lock(&run.progress);
        loop {
            if progress.finished {
                return;
            }
            let Some(current) = progress.current.clone() else {
                // The worker has not published its first hook yet; it
                // notifies when it does (we are not sleeping on a
                // deadline).
                progress.supervisor_wakes_at = None;
                progress = wait(&run.changed, progress);
                continue;
            };
            let Some(deadline) = current.control.deadline else {
                progress.supervisor_wakes_at = None;
                progress = wait(&run.changed, progress);
                continue;
            };
            if Instant::now() < deadline {
                progress.supervisor_wakes_at = Some(deadline);
                progress = wait_until(&run.changed, progress, deadline);
                continue;
            }
            // Deadline passed: time the hook out unless it just finished.
            let Some(slot) = self.slots.get(current.index) else {
                return;
            };
            let ran_for = deadline.saturating_duration_since(current.started);
            if slot.try_finish(HookStatus::TimedOut, ran_for, None) {
                current.control.abandoned.store(true, Ordering::Release);
                sm_warn!(hook = %slot.name, "shutdown hook timed out; abandoning it");
                progress.generation += 1;
                progress.current = None;
                let generation = progress.generation;
                drop(progress);
                if !self.spawn_worker(run, generation, current.index + 1, ctx) {
                    return;
                }
                progress = lock(&run.progress);
            } else {
                // The worker finished it in the meantime and moved on (or
                // is about to publish the next hook).
                progress.supervisor_wakes_at = None;
                if progress
                    .current
                    .as_ref()
                    .is_some_and(|c| c.index == current.index)
                {
                    progress = wait(&run.changed, progress);
                }
            }
        }
    }

    /// Start a worker for `generation` at hook `from`. If no thread can
    /// be spawned, run the remaining hooks on this thread instead: an
    /// explicit degraded path without deadlines. Returns `true` if a
    /// worker was started.
    fn spawn_worker(
        self: &Arc<Self>,
        run: &Arc<Run>,
        generation: u64,
        from: usize,
        ctx: &RuntimeContext,
    ) -> bool {
        if from >= self.slots.len() {
            lock(&run.progress).finished = true;
            return false;
        }
        if let Some(standby) = lock(&self.standby).take() {
            let assignment = Assignment {
                runner: Arc::clone(self),
                run: Arc::clone(run),
                generation,
                from,
                ctx: ctx.clone(),
            };
            if standby.send(assignment).is_ok() {
                return true;
            }
        }
        let runner = Arc::clone(self);
        let worker_run = Arc::clone(run);
        let worker_ctx = ctx.clone();
        let spawned = thread::Builder::new()
            .name("signal-mod-hook".into())
            .spawn(move || {
                let _runtime = worker_ctx.enter();
                runner.work(&worker_run, generation, from);
            });
        match spawned {
            Ok(_detached) => true,
            Err(error) => {
                sm_warn!(error = %error,
                    "could not spawn a hook thread; running the remaining hooks inline without deadlines");
                drop(error);
                self.work(run, generation, from);
                false
            }
        }
    }

    /// Worker body: run hooks `from..` while `generation` is current.
    fn work(&self, run: &Run, generation: u64, from: usize) {
        for (index, slot) in self.slots.iter().enumerate().skip(from) {
            // Start the clock (first hook only) before reading the time,
            // so a zero budget really is zero.
            let plan = *run.plan();
            let started = Instant::now();
            let Some((phase, phase_deadline, reason)) = plan.at(started) else {
                self.skip_rest(run, generation, index);
                return;
            };
            let deadline = earliest(
                slot.timeout.and_then(|t| started.checked_add(t)),
                phase_deadline,
            );
            let control = Arc::new(HookControl {
                deadline,
                abandoned: AtomicBool::new(false),
            });
            {
                let mut progress = lock(&run.progress);
                if progress.generation != generation {
                    return;
                }
                slot.mark_running(phase);
                progress.current = Some(Current {
                    index,
                    started,
                    control: Arc::clone(&control),
                });
                // Wake the supervisor only if it would otherwise sleep
                // past this hook's deadline.
                let sooner = match (deadline, progress.supervisor_wakes_at) {
                    (Some(d), Some(wakes)) => d < wakes,
                    (Some(_), None) => true,
                    (None, _) => false,
                };
                if sooner {
                    run.changed.notify_all();
                }
            }
            sm_debug!(hook = %slot.name, ?phase, "shutdown hook started");
            let (status, panic) = {
                let _current = CurrentHookGuard::set(&control);
                execute(&*slot.hook, reason)
            };
            if !slot.try_finish(status, started.elapsed(), panic) {
                // Timed out by the supervisor: this worker is retired.
                return;
            }
            if status == HookStatus::Panicked {
                sm_error!(hook = %slot.name, "shutdown hook panicked");
            } else {
                sm_debug!(hook = %slot.name, "shutdown hook completed");
            }
        }
        let mut progress = lock(&run.progress);
        if progress.generation == generation {
            progress.finished = true;
            progress.current = None;
            run.changed.notify_all();
        }
    }

    fn skip_rest(&self, run: &Run, generation: u64, from: usize) {
        let mut progress = lock(&run.progress);
        if progress.generation != generation {
            return;
        }
        for slot in self.slots.iter().skip(from) {
            sm_warn!(hook = %slot.name, "shutdown hook skipped: force budget exhausted");
            slot.mark_skipped();
        }
        progress.finished = true;
        progress.current = None;
        run.changed.notify_all();
    }

    /// Future that runs the hooks on a supervisor thread (or waits for
    /// the run another caller started) without blocking the executor.
    pub(crate) fn run_async(self: &Arc<Self>, reason: ShutdownReason) -> RunHooksFuture {
        RunHooksFuture {
            runner: Arc::clone(self),
            reason,
            key: None,
        }
    }

    fn poll_run(
        self: &Arc<Self>,
        reason: ShutdownReason,
        key: &mut Option<WaitKey>,
        cx: &Context<'_>,
    ) -> Poll<RunReport> {
        {
            let mut state = lock(&self.state);
            match &mut *state {
                RunState::Done(report) => return Poll::Ready(report.clone()),
                RunState::Running(waiters) => {
                    waiters.register(key, cx.waker());
                    return Poll::Pending;
                }
                RunState::Idle => {
                    let mut waiters = WaitList::default();
                    waiters.register(key, cx.waker());
                    *state = RunState::Running(waiters);
                }
            }
        }
        let ctx = RuntimeContext::capture();
        let runner = Arc::clone(self);
        let supervisor_ctx = ctx.clone();
        let spawned = thread::Builder::new()
            .name("signal-mod-shutdown".into())
            .spawn(move || {
                let report = runner.supervise(reason, &supervisor_ctx);
                runner.publish(report);
            });
        match spawned {
            Ok(_detached) => Poll::Pending,
            Err(error) => {
                sm_warn!(error = %error,
                    "could not spawn the shutdown supervisor thread; running hooks on the executor thread");
                drop(error);
                let report = self.supervise(reason, &ctx);
                self.publish(report.clone());
                *key = None;
                Poll::Ready(report)
            }
        }
    }

    fn cancel_wait(&self, key: WaitKey) {
        if let RunState::Running(waiters) = &mut *lock(&self.state) {
            waiters.remove(key);
        }
    }
}

/// Future returned by
/// [`Coordinator::run_hooks_async`](crate::Coordinator::run_hooks_async).
///
/// The first poll starts the hook run on a dedicated supervisor thread
/// (unless another caller already started it); the future then resolves
/// with the [`RunReport`] of that run. It never blocks the executor and
/// works on any runtime. Dropping it does not cancel a run that has
/// started.
///
/// # Examples
///
/// ```
/// use signal_mod::{Coordinator, RunHooksFuture, ShutdownReason, hook_from_fn};
///
/// let coord = Coordinator::builder().hook(hook_from_fn("flush", 0, |_| {})).build();
/// let run: RunHooksFuture = coord.run_hooks_async(ShutdownReason::Requested);
/// // Any executor drives it; here a minimal one.
/// let report = smol::block_on(run);
/// assert_eq!(report.completed(), 1);
/// ```
#[must_use = "futures do nothing unless awaited"]
pub struct RunHooksFuture {
    runner: Arc<HookRunner>,
    reason: ShutdownReason,
    key: Option<WaitKey>,
}

impl fmt::Debug for RunHooksFuture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunHooksFuture")
            .field("reason", &self.reason)
            .finish_non_exhaustive()
    }
}

impl Future for RunHooksFuture {
    type Output = RunReport;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<RunReport> {
        let this = self.get_mut();
        let polled = this.runner.poll_run(this.reason, &mut this.key, cx);
        if polled.is_ready() {
            this.key = None;
        }
        polled
    }
}

impl Drop for RunHooksFuture {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.runner.cancel_wait(key);
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::hook::hook_from_fn;

    fn runner(
        hooks: Vec<Box<dyn ShutdownHook>>,
        graceful_ms: u64,
        force_ms: u64,
    ) -> Arc<HookRunner> {
        Arc::new(HookRunner::new(
            hooks,
            Duration::from_millis(graceful_ms),
            Duration::from_millis(force_ms),
        ))
    }

    #[test]
    fn test_earliest_prefers_the_sooner_deadline() {
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        assert_eq!(earliest(Some(now), Some(later)), Some(now));
        assert_eq!(earliest(None, Some(later)), Some(later));
        assert_eq!(earliest(Some(now), None), Some(now));
        assert_eq!(earliest(None, None), None);
    }

    #[test]
    fn test_runner_with_unbounded_budgets_waits_for_slow_hook() {
        let r = Arc::new(HookRunner::new(
            vec![Box::new(hook_from_fn("slow", 0, |_| {
                thread::sleep(Duration::from_millis(20));
            }))],
            Duration::MAX,
            Duration::MAX,
        ));
        let report = r.run_blocking(ShutdownReason::Requested, &RuntimeContext::default());
        assert_eq!(report.completed(), 1);
    }

    #[test]
    fn test_runner_force_not_above_graceful_has_no_force_phase() {
        let r = runner(
            vec![
                Box::new(hook_from_fn("hang", 1, |_| {
                    thread::sleep(Duration::from_secs(30));
                })),
                Box::new(hook_from_fn("late", 0, |_| {})),
            ],
            30,
            10,
        );
        let report = r.run_blocking(ShutdownReason::Requested, &RuntimeContext::default());
        assert_eq!(report.timed_out(), 1);
        assert_eq!(report.skipped(), 1);
        assert_eq!(report.forced(), 0);
    }

    #[test]
    fn test_short_hook_timeout_after_long_one_still_enforced() {
        // The second hook's deadline is earlier than the supervisor's
        // current sleep target; the worker must wake the supervisor.
        let r = Arc::new(HookRunner::new(
            vec![
                Box::new(hook_from_fn("fast", 1, |_| {})),
                Box::new(
                    hook_from_fn("hang", 0, |_| thread::sleep(Duration::from_secs(5)))
                        .with_timeout(Duration::from_millis(20)),
                ),
            ],
            Duration::from_secs(30),
            Duration::from_secs(30),
        ));
        let start = Instant::now();
        let report = r.run_blocking(ShutdownReason::Requested, &RuntimeContext::default());
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(report.completed(), 1);
        assert_eq!(report.timed_out(), 1);
    }

    #[test]
    fn test_runner_debug_lists_configuration() {
        let r = runner(Vec::new(), 1, 2);
        let text = format!("{r:?}");
        assert!(text.contains("hooks: 0"), "{text}");
    }

    #[test]
    fn test_execute_panic_with_non_string_payload_has_no_message() {
        let hook = hook_from_fn("odd", 0, |_| std::panic::panic_any(42_u32));
        assert_eq!(
            execute(&hook, ShutdownReason::Requested),
            (HookStatus::Panicked, None)
        );
    }

    #[test]
    fn test_execute_panic_with_string_payload_keeps_message() {
        let hook = hook_from_fn("fmt", 0, |_| panic!("code {}", 7));
        assert_eq!(
            execute(&hook, ShutdownReason::Requested),
            (HookStatus::Panicked, Some("code 7".to_owned()))
        );
    }

    #[test]
    fn test_try_finish_only_first_caller_wins() {
        let slot = HookSlot::new(Box::new(hook_from_fn("h", 0, |_| {})));
        slot.mark_running(HookPhase::Graceful);
        assert!(slot.try_finish(HookStatus::TimedOut, Duration::from_millis(3), None));
        assert!(!slot.try_finish(HookStatus::Completed, Duration::from_millis(9), None));
        let outcome = slot.outcome();
        assert_eq!(outcome.status, HookStatus::TimedOut);
        assert_eq!(outcome.duration, Some(Duration::from_millis(3)));
    }

    #[test]
    fn test_outcome_hides_duration_until_finished() {
        let slot = HookSlot::new(Box::new(hook_from_fn("h", 0, |_| {})));
        slot.mark_running(HookPhase::Force);
        let outcome = slot.outcome();
        assert_eq!(outcome.status, HookStatus::Running);
        assert_eq!(outcome.phase, Some(HookPhase::Force));
        assert_eq!(outcome.duration, None);
    }
}
