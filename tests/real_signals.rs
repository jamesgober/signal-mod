//! Real OS signal delivery, end to end, in child processes.
//!
//! This test binary has `harness = false`. Run normally it is the
//! parent: it re-executes itself once per scenario with
//! `SIGNAL_MOD_CHILD=<scenario>`, delivers real signals to (or inside)
//! the child, and checks the child's report and exit status. Isolating
//! each scenario in its own process keeps real handlers, escalation
//! exits and restored default dispositions away from other tests.
//!
//! - Unix: the child raises SIGUSR1 / SIGUSR2 / SIGHUP / SIGTERM / SIGINT
//!   / SIGPIPE at itself through each enabled back-end. Covers per-target
//!   signal numbers (audit SM-001: on macOS, 1.0.0 registered SIGBUS for
//!   `User1`), notify-only subscriptions, second-signal escalation, and
//!   default restoration after uninstall.
//! - Windows: the parent sends `CTRL_BREAK_EVENT` to a child started in
//!   its own process group (`GenerateConsoleCtrlEvent`). Scenarios are
//!   reported as skipped when the test runs without a console.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::print_stderr
)] // test binary
// With no back-end feature there are no scenarios and most helpers idle.
#![cfg_attr(
    not(any(feature = "tokio", feature = "agnostic")),
    allow(dead_code, unused_imports, unused_mut, unused_variables)
)]

#[cfg(loom)]
fn main() {}

#[cfg(not(loom))]
fn main() {
    if let Ok(scenario) = std::env::var(parent::CHILD_ENV) {
        child::run(&scenario);
        return;
    }
    parent::run_all(&scenarios());
}

#[cfg(not(loom))]
type Scenario = (&'static str, fn(&str) -> parent::Outcome);

#[cfg(all(not(loom), unix))]
#[allow(clippy::vec_init_then_push)] // each entry is feature-gated
fn scenarios() -> Vec<Scenario> {
    let mut v: Vec<Scenario> = Vec::new();
    #[cfg(feature = "tokio")]
    v.push(("unix_tokio_delivers_every_signal", unix::expect_ok));
    #[cfg(feature = "agnostic")]
    v.push(("unix_agnostic_delivers_every_signal", unix::expect_ok));
    #[cfg(any(feature = "tokio", feature = "agnostic"))]
    {
        v.push((
            "unix_second_sigint_reraises_sigint",
            unix::expect_killed_by_sigint,
        ));
        v.push((
            "unix_uninstall_restores_default_sigterm",
            unix::expect_killed_by_sigterm,
        ));
        v.push(("unix_pipe_stays_ignored_after_uninstall", unix::expect_ok));
        // SM2-004: only a disposition signal-mod replaced is restored.
        v.push((
            "unix_foreign_handler_keeps_signal_after_uninstall",
            unix::expect_ok,
        ));
        v.push((
            "unix_ignored_signal_stays_ignored_after_uninstall",
            unix::expect_ok,
        ));
        v.push(("unix_notify_signal_is_not_restored", unix::expect_ok));
        // SM2-016: a process forked after `build()` still runs its hooks
        // within the budget.
        v.push(("unix_fork_after_build_runs_hooks", unix::expect_ok));
    }
    v
}

#[cfg(all(not(loom), windows))]
#[allow(clippy::vec_init_then_push)] // each entry is feature-gated
fn scenarios() -> Vec<Scenario> {
    let mut v: Vec<Scenario> = Vec::new();
    #[cfg(feature = "tokio")]
    v.push(("windows_tokio_ctrl_break_is_quit", windows::ctrl_break_once));
    #[cfg(feature = "agnostic")]
    v.push((
        "windows_agnostic_ctrl_break_is_quit",
        windows::ctrl_break_once,
    ));
    #[cfg(any(feature = "tokio", feature = "agnostic"))]
    v.push((
        "windows_second_ctrl_break_exits_131",
        windows::ctrl_break_twice,
    ));
    // SM2-005: a Tokio-installed and an agnostic-installed coordinator
    // both receive the event.
    #[cfg(all(feature = "tokio", feature = "agnostic"))]
    v.push((
        "windows_mixed_backends_both_receive",
        windows::ctrl_break_once,
    ));
    // SM2-017: the application's own earlier `tokio::signal` listener
    // still receives the event next to a coordinator.
    #[cfg(feature = "tokio")]
    v.push((
        "windows_app_tokio_listener_still_receives",
        windows::ctrl_break_once,
    ));
    v
}

#[cfg(all(not(loom), not(any(unix, windows))))]
fn scenarios() -> Vec<Scenario> {
    Vec::new()
}

#[cfg(not(loom))]
mod parent {
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, ExitStatus, Stdio};
    use std::sync::mpsc::{self, Receiver};
    use std::time::{Duration, Instant};

    pub(crate) const CHILD_ENV: &str = "SIGNAL_MOD_CHILD";
    const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

    pub(crate) enum Outcome {
        Pass,
        #[cfg_attr(not(windows), allow(dead_code))] // only Windows can skip
        Skip(String),
        Fail(String),
    }

    pub(crate) fn run_all(scenarios: &[super::Scenario]) {
        let mut failed = 0;
        for (name, check) in scenarios {
            match check(name) {
                Outcome::Pass => println!("test {name} ... ok"),
                Outcome::Skip(why) => println!("test {name} ... skipped ({why})"),
                Outcome::Fail(why) => {
                    failed += 1;
                    println!("test {name} ... FAILED\n    {why}");
                }
            }
        }
        println!(
            "\nreal_signals: {} scenario(s), {failed} failed",
            scenarios.len()
        );
        if failed > 0 {
            std::process::exit(1);
        }
    }

    /// A running child and the lines it has printed so far.
    pub(crate) struct Run {
        pub(crate) child: Child,
        lines: Receiver<String>,
        seen: Vec<String>,
    }

    pub(crate) fn spawn(scenario: &str, configure: impl FnOnce(&mut Command)) -> Run {
        let exe = std::env::current_exe().expect("test binary path");
        let mut cmd = Command::new(exe);
        let _env = cmd
            .env(CHILD_ENV, scenario)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        configure(&mut cmd);
        let mut child = cmd.spawn().expect("spawn child");
        let stdout = child.stdout.take().expect("piped stdout");
        let (tx, rx) = mpsc::channel();
        let _reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Run {
            child,
            lines: rx,
            seen: Vec::new(),
        }
    }

    impl Run {
        /// Wait until the child prints `marker`.
        #[cfg_attr(not(windows), allow(dead_code))] // used by the Windows scenarios
        pub(crate) fn wait_for(&mut self, marker: &str) -> Result<(), String> {
            let deadline = Instant::now() + CHILD_TIMEOUT;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let Ok(line) = self.lines.recv_timeout(left) else {
                    let _killed = self.child.kill();
                    return Err(format!("no {marker:?} from child; output: {:?}", self.seen));
                };
                let hit = line == marker;
                self.seen.push(line);
                if hit {
                    return Ok(());
                }
            }
        }

        /// Wait for exit (killing a hung child) and return the status and
        /// everything it printed.
        pub(crate) fn finish(mut self) -> Result<(ExitStatus, String), String> {
            let start = Instant::now();
            let status = loop {
                if let Some(status) = self.child.try_wait().map_err(|e| e.to_string())? {
                    break status;
                }
                if start.elapsed() > CHILD_TIMEOUT {
                    let _killed = self.child.kill();
                    return Err(format!("child timed out; output: {:?}", self.seen));
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            // The reader thread ends at EOF; collect what is left.
            while let Ok(line) = self.lines.recv_timeout(Duration::from_secs(5)) {
                self.seen.push(line);
            }
            Ok((status, self.seen.join("\n")))
        }
    }
}

#[cfg(all(not(loom), unix))]
mod unix {
    use super::parent::{Outcome, spawn};
    use std::os::unix::process::ExitStatusExt;

    fn run(name: &str) -> Result<(std::process::ExitStatus, String), String> {
        spawn(name, |_| {}).finish()
    }

    pub(crate) fn expect_ok(name: &str) -> Outcome {
        match run(name) {
            Ok((status, out)) if status.success() && out.contains("CHILD-OK") => Outcome::Pass,
            Ok((status, out)) => Outcome::Fail(format!("status {status:?}, output: {out}")),
            Err(e) => Outcome::Fail(e),
        }
    }

    /// Escalation re-raises the signal, so the child dies by SIGINT
    /// rather than exiting with 130.
    pub(crate) fn expect_killed_by_sigint(name: &str) -> Outcome {
        let sigint = signal_mod::Signal::Interrupt.unix_number();
        match run(name) {
            Ok((status, out)) if status.signal() == sigint && out.contains("INITIATED") => {
                Outcome::Pass
            }
            Ok((status, out)) => Outcome::Fail(format!("status {status:?}, output: {out}")),
            Err(e) => Outcome::Fail(e),
        }
    }

    pub(crate) fn expect_killed_by_sigterm(name: &str) -> Outcome {
        let sigterm = signal_mod::Signal::Terminate.unix_number();
        match run(name) {
            Ok((status, out)) if status.signal() == sigterm && out.contains("UNINSTALLED") => {
                Outcome::Pass
            }
            Ok((status, out)) => Outcome::Fail(format!("status {status:?}, output: {out}")),
            Err(e) => Outcome::Fail(e),
        }
    }
}

#[cfg(all(not(loom), windows))]
mod windows {
    use super::parent::{Outcome, Run, spawn};
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, GenerateConsoleCtrlEvent};
    use windows_sys::Win32::System::Threading::CREATE_NEW_PROCESS_GROUP;

    fn send_ctrl_break(run: &Run) -> bool {
        // SAFETY: `GenerateConsoleCtrlEvent` takes two integers and has no
        // memory-safety preconditions. The target is our own child's
        // process group: it was created with `CREATE_NEW_PROCESS_GROUP`,
        // so its pid is the group id.
        unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, run.child.id()) != 0 }
    }

    fn scenario(name: &str, breaks: usize, expect_code: i32) -> Outcome {
        let mut run = spawn(name, |cmd| {
            let _flags = cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        });
        if let Err(e) = run.wait_for("READY") {
            return Outcome::Fail(e);
        }
        if !send_ctrl_break(&run) {
            let _killed = run.child.kill();
            return Outcome::Skip("no console to deliver CTRL_BREAK_EVENT".into());
        }
        if breaks > 1 {
            if let Err(e) = run.wait_for("INITIATED") {
                return Outcome::Fail(e);
            }
            let _sent = send_ctrl_break(&run);
        }
        match run.finish() {
            Ok((status, _)) if status.code() == Some(expect_code) => Outcome::Pass,
            Ok((status, out)) => Outcome::Fail(format!("status {status:?}, output: {out}")),
            Err(e) => Outcome::Fail(e),
        }
    }

    /// The child exits 0 only after observing `Signal::Quit`.
    pub(crate) fn ctrl_break_once(name: &str) -> Outcome {
        scenario(name, 1, 0)
    }

    pub(crate) fn ctrl_break_twice(name: &str) -> Outcome {
        scenario(name, 2, signal_mod::Signal::Quit.exit_code())
    }
}

/// Child-side scenarios. Each prints markers to stdout and exits with a
/// status the parent checks.
#[cfg(not(loom))]
#[allow(dead_code, unused_imports)] // the scenario set depends on platform and features
mod child {
    use signal_mod::{Coordinator, CoordinatorBuilder, ShutdownReason, Signal, SignalSet};
    use std::io::Write;
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(15);

    fn say(line: &str) {
        let mut out = std::io::stdout().lock();
        let _ignored = writeln!(out, "{line}").and_then(|()| out.flush());
    }

    fn fail(why: &str) -> ! {
        say(&format!("CHILD-FAIL {why}"));
        std::process::exit(2);
    }

    pub(crate) fn run(scenario: &str) {
        match scenario {
            #[cfg(all(unix, feature = "tokio"))]
            "unix_tokio_delivers_every_signal" => {
                with_tokio(|| unix_delivers_every_signal(signal_mod::Backend::Tokio));
            }
            #[cfg(all(unix, feature = "agnostic"))]
            "unix_agnostic_delivers_every_signal" => {
                unix_delivers_every_signal(signal_mod::Backend::Agnostic);
            }
            #[cfg(unix)]
            "unix_second_sigint_reraises_sigint" => in_any_backend(unix_second_sigint),
            #[cfg(unix)]
            "unix_uninstall_restores_default_sigterm" => in_any_backend(unix_restore_default),
            #[cfg(unix)]
            "unix_pipe_stays_ignored_after_uninstall" => in_any_backend(unix_pipe_ignored),
            #[cfg(unix)]
            "unix_foreign_handler_keeps_signal_after_uninstall" => {
                in_any_backend(unix_foreign_handler_kept);
            }
            #[cfg(unix)]
            "unix_ignored_signal_stays_ignored_after_uninstall" => {
                in_any_backend(unix_ignored_signal_kept);
            }
            #[cfg(unix)]
            "unix_notify_signal_is_not_restored" => in_any_backend(unix_notify_not_restored),
            #[cfg(unix)]
            "unix_fork_after_build_runs_hooks" => unix_fork_after_build(),
            #[cfg(all(windows, feature = "tokio", feature = "agnostic"))]
            "windows_mixed_backends_both_receive" => windows_mixed_backends(),
            #[cfg(all(windows, feature = "tokio"))]
            "windows_app_tokio_listener_still_receives" => windows_app_listener_kept(),
            #[cfg(all(windows, feature = "tokio"))]
            "windows_tokio_ctrl_break_is_quit" => with_tokio(|| windows_wait_for_quit(false)),
            #[cfg(all(windows, feature = "agnostic"))]
            "windows_agnostic_ctrl_break_is_quit" => windows_wait_for_quit(false),
            #[cfg(windows)]
            "windows_second_ctrl_break_exits_131" => {
                in_any_backend(|| windows_wait_for_quit(true));
            }
            other => fail(&format!("unknown scenario {other}")),
        }
    }

    /// Run `f` inside a Tokio runtime context, so `install` picks the
    /// Tokio back-end.
    #[cfg(feature = "tokio")]
    fn with_tokio(f: impl FnOnce()) {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let _guard = rt.enter();
        f();
    }

    /// Run `f` wherever `install` succeeds: directly with the agnostic
    /// back-end, otherwise inside a Tokio runtime.
    fn in_any_backend(f: impl FnOnce()) {
        #[cfg(feature = "agnostic")]
        f();
        #[cfg(all(feature = "tokio", not(feature = "agnostic")))]
        with_tokio(f);
        #[cfg(not(any(feature = "tokio", feature = "agnostic")))]
        {
            drop(f);
            fail("no back-end");
        }
    }

    fn installed(builder: CoordinatorBuilder) -> Coordinator {
        let coord = builder.build();
        if let Err(e) = coord.install() {
            fail(&format!("install: {e}"));
        }
        coord
    }

    #[cfg(unix)]
    fn raise(sig: Signal) {
        let number = sig.unix_number().unwrap_or_else(|| fail("no number"));
        if let Err(e) = signal_hook::low_level::raise(number) {
            fail(&format!("raise({sig}): {e}"));
        }
    }

    #[cfg(unix)]
    fn unix_delivers_every_signal(expected: signal_mod::Backend) {
        let notify = SignalSet::empty()
            .with(Signal::User1)
            .with(Signal::User2)
            .with(Signal::Hangup);
        let coord = installed(
            Coordinator::builder()
                .signals(SignalSet::graceful() - Signal::Hangup)
                .notify_signals(notify),
        );
        if coord.backend() != Some(expected) {
            fail(&format!("backend {:?}", coord.backend()));
        }
        let mut sub = coord.subscribe();
        for sig in [Signal::User1, Signal::User2, Signal::Hangup] {
            raise(sig);
            match sub.recv_timeout(WAIT) {
                Some(got) if got == sig => {}
                other => fail(&format!("raised {sig}, received {other:?}")),
            }
            if coord.token().is_initiated() {
                fail(&format!("{sig} initiated shutdown"));
            }
        }
        raise(Signal::Terminate);
        if !coord.token().wait_blocking_timeout(WAIT) {
            fail("SIGTERM did not initiate shutdown");
        }
        if coord.token().reason() != Some(ShutdownReason::Signal(Signal::Terminate)) {
            fail(&format!("reason {:?}", coord.token().reason()));
        }
        say("CHILD-OK");
        std::process::exit(0);
    }

    #[cfg(unix)]
    fn unix_second_sigint() {
        let coord = installed(Coordinator::builder().force_exit_on_second_signal(true));
        raise(Signal::Interrupt);
        if !coord.token().wait_blocking_timeout(WAIT) {
            fail("first SIGINT did not initiate shutdown");
        }
        say("INITIATED");
        raise(Signal::Interrupt);
        std::thread::sleep(WAIT);
        fail("second SIGINT did not exit");
    }

    #[cfg(unix)]
    fn unix_restore_default() {
        let coord = installed(Coordinator::builder());
        drop(coord);
        say("UNINSTALLED");
        raise(Signal::Terminate);
        std::thread::sleep(WAIT);
        fail("SIGTERM after uninstall did not terminate");
    }

    #[cfg(unix)]
    fn unix_pipe_ignored() {
        let coord = installed(Coordinator::builder().notify_signals(SignalSet::from(Signal::Pipe)));
        let mut sub = coord.subscribe();
        raise(Signal::Pipe);
        if sub.recv_timeout(WAIT) != Some(Signal::Pipe) {
            fail("SIGPIPE not delivered while installed");
        }
        drop(coord);
        raise(Signal::Pipe);
        std::thread::sleep(Duration::from_millis(200));
        say("CHILD-OK");
        std::process::exit(0);
    }

    /// A handler registered by other code before signal-mod must keep
    /// working after the coordinator is gone (1.0 audit follow-up SM2-004:
    /// the process used to be killed).
    #[cfg(unix)]
    fn unix_foreign_handler_kept() {
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let number = Signal::User1
            .unix_number()
            .unwrap_or_else(|| fail("no number"));
        if let Err(e) = signal_hook::flag::register(number, std::sync::Arc::clone(&seen)) {
            fail(&format!("foreign register: {e}"));
        }
        let coord = installed(Coordinator::builder().signals(SignalSet::from(Signal::User1)));
        drop(coord);
        raise(Signal::User1);
        std::thread::sleep(Duration::from_millis(200));
        if !seen.load(std::sync::atomic::Ordering::SeqCst) {
            fail("foreign handler did not run");
        }
        say("CHILD-OK");
        std::process::exit(0);
    }

    /// A signal ignored before signal-mod (as under `nohup`) stays
    /// ignored after the coordinator is gone.
    #[cfg(unix)]
    fn unix_ignored_signal_kept() {
        let number = Signal::Hangup
            .unix_number()
            .unwrap_or_else(|| fail("no number"));
        // SAFETY: setting SIGHUP to SIG_IGN installs no handler code; the
        // child is single-purpose and nothing else touches SIGHUP yet.
        let previous = unsafe { libc::signal(number, libc::SIG_IGN) };
        if previous == libc::SIG_ERR {
            fail("could not ignore SIGHUP");
        }
        let coord = installed(Coordinator::builder().signals(SignalSet::from(Signal::Hangup)));
        drop(coord);
        raise(Signal::Hangup);
        std::thread::sleep(Duration::from_millis(200));
        say("CHILD-OK");
        std::process::exit(0);
    }

    /// Notify-only signals are never restored to their default action.
    #[cfg(unix)]
    fn unix_notify_not_restored() {
        let coord = installed(
            Coordinator::builder()
                .signals(SignalSet::empty())
                .notify_signals(SignalSet::from(Signal::User2)),
        );
        drop(coord);
        raise(Signal::User2);
        std::thread::sleep(Duration::from_millis(200));
        say("CHILD-OK");
        std::process::exit(0);
    }

    /// Name of the standby worker thread that `build()` parks.
    #[cfg(unix)]
    const STANDBY_THREAD: &str = "signal-mod-hook";

    /// Wait (up to 10 s) until the thread named [`STANDBY_THREAD`] exists
    /// and the kernel reports it blocked, and fail the scenario otherwise.
    /// Linux and macOS observe the thread's scheduler state; on other
    /// Unix systems (not run in CI) a pause stands in for it.
    #[cfg(unix)]
    fn wait_until_standby_parked() {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            match standby_parked() {
                Some(true) => return,
                Some(false) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Some(false) => fail("the standby worker never parked"),
                None => {
                    std::thread::sleep(Duration::from_millis(200));
                    return;
                }
            }
        }
    }

    /// `Some(true)` when the standby thread exists and is sleeping
    /// (state `S` in `/proc/self/task/<tid>/stat`).
    #[cfg(target_os = "linux")]
    fn standby_parked() -> Option<bool> {
        let Ok(tasks) = std::fs::read_dir("/proc/self/task") else {
            return None;
        };
        for task in tasks.flatten() {
            let path = task.path();
            let name = std::fs::read_to_string(path.join("comm")).unwrap_or_default();
            if name.trim_end() != STANDBY_THREAD {
                continue;
            }
            let stat = std::fs::read_to_string(path.join("stat")).unwrap_or_default();
            // `pid (comm) state ...`: the state follows the last `)`.
            let state = stat.rsplit_once(')').map(|(_, rest)| rest.trim_start());
            return Some(state.is_some_and(|rest| rest.starts_with('S')));
        }
        Some(false)
    }

    /// `Some(true)` when the standby thread exists and is waiting
    /// (`TH_STATE_WAITING` through `proc_pidinfo`).
    #[cfg(target_os = "macos")]
    fn standby_parked() -> Option<bool> {
        // `PROC_PIDLISTTHREADS` from <sys/proc_info.h> (not in libc).
        const PROC_PIDLISTTHREADS: libc::c_int = 6;
        let pid = libc::c_int::try_from(std::process::id()).ok()?;
        let mut handles = [0u64; 256];
        let size = libc::c_int::try_from(core::mem::size_of_val(&handles)).ok()?;
        // SAFETY: the buffer is valid for `size` bytes for the call.
        let bytes = unsafe {
            libc::proc_pidinfo(
                pid,
                PROC_PIDLISTTHREADS,
                0,
                handles.as_mut_ptr().cast(),
                size,
            )
        };
        let count = usize::try_from(bytes).ok()? / core::mem::size_of::<u64>();
        for &handle in handles.iter().take(count) {
            let mut info = core::mem::MaybeUninit::<libc::proc_threadinfo>::zeroed();
            let info_size =
                libc::c_int::try_from(core::mem::size_of::<libc::proc_threadinfo>()).ok()?;
            // SAFETY: `info` is valid for `info_size` bytes for the call.
            let got = unsafe {
                libc::proc_pidinfo(
                    pid,
                    libc::PROC_PIDTHREADINFO,
                    handle,
                    info.as_mut_ptr().cast(),
                    info_size,
                )
            };
            if got != info_size {
                continue;
            }
            // SAFETY: the call filled the whole struct, and an all-zero
            // `proc_threadinfo` is valid anyway.
            let info = unsafe { info.assume_init() };
            // SAFETY: the kernel NUL-terminates `pth_name` within its
            // fixed size; the zeroed buffer is NUL-terminated otherwise.
            let name = unsafe { std::ffi::CStr::from_ptr(info.pth_name.as_ptr()) };
            if name.to_bytes() == STANDBY_THREAD.as_bytes() {
                return Some(info.pth_run_state == libc::TH_STATE_WAITING);
            }
        }
        Some(false)
    }

    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    fn standby_parked() -> Option<bool> {
        None
    }

    /// Daemon pattern: build the coordinator, then `fork`. The standby hook
    /// worker started by `build()` does not exist in the child; the run
    /// there must still execute the hooks and end within the budget.
    #[cfg(unix)]
    fn unix_fork_after_build() {
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&ran);
        let coord = Coordinator::builder()
            .graceful_timeout(Duration::from_secs(1))
            .force_timeout(Duration::from_secs(2))
            .hook(signal_mod::hook_from_fn("flush", 0, move |_| {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
            }))
            .build();
        // Fork only once the standby worker is blocked in its channel
        // receive: the worst case for the child, which must not touch
        // that channel.
        wait_until_standby_parked();
        // SAFETY: `fork` has no memory-safety preconditions. The child
        // only runs the hook sequence and leaves with `_exit`, never
        // returning into the test harness.
        let pid = unsafe { libc::fork() };
        if pid < 0 {
            fail("fork failed");
        }
        if pid == 0 {
            let start = std::time::Instant::now();
            let report = coord.run_hooks_with_report(ShutdownReason::Requested);
            let ok = report.completed() == 1
                && ran.load(std::sync::atomic::Ordering::SeqCst)
                && start.elapsed() < Duration::from_secs(2) + Duration::from_millis(500);
            // SAFETY: `_exit` ends the forked child immediately without
            // running destructors or the parent's atexit handlers.
            unsafe { libc::_exit(if ok { 0 } else { 3 }) };
        }
        let mut status = 0;
        // SAFETY: `pid` is our own child and `status` is a valid out
        // pointer for the duration of the call.
        let waited = unsafe { libc::waitpid(pid, &raw mut status, 0) };
        if waited != pid || !libc::WIFEXITED(status) || libc::WEXITSTATUS(status) != 0 {
            fail(&format!("forked child failed: status {status}"));
        }
        say("CHILD-OK");
        std::process::exit(0);
    }

    /// One coordinator installed through each back-end; both must see
    /// the `CTRL_BREAK`.
    #[cfg(all(windows, feature = "tokio", feature = "agnostic"))]
    fn windows_mixed_backends() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let tokio_coord = {
            let _guard = rt.enter();
            installed(Coordinator::builder().signals(SignalSet::standard()))
        };
        let agnostic_coord = installed(Coordinator::builder().signals(SignalSet::standard()));
        if tokio_coord.backend() != Some(signal_mod::Backend::Tokio)
            || agnostic_coord.backend() != Some(signal_mod::Backend::Agnostic)
        {
            fail("unexpected back-ends");
        }
        say("READY");
        for coord in [&tokio_coord, &agnostic_coord] {
            if !coord.token().wait_blocking_timeout(WAIT) {
                fail(&format!("{:?} coordinator saw nothing", coord.backend()));
            }
            if coord.token().reason() != Some(ShutdownReason::Signal(Signal::Quit)) {
                fail(&format!("reason {:?}", coord.token().reason()));
            }
        }
        std::process::exit(0);
    }

    /// An application listener registered through Tokio before the
    /// coordinator keeps receiving events.
    #[cfg(all(windows, feature = "tokio"))]
    fn windows_app_listener_kept() {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let _guard = rt.enter();
        let mut app = tokio::signal::windows::ctrl_break()
            .unwrap_or_else(|e| fail(&format!("app listener: {e}")));
        let coord = installed(Coordinator::builder().signals(SignalSet::standard()));
        say("READY");
        if !coord.token().wait_blocking_timeout(WAIT) {
            fail("coordinator saw nothing");
        }
        let app_saw = rt.block_on(async { tokio::time::timeout(WAIT, app.recv()).await });
        if !matches!(app_saw, Ok(Some(()))) {
            fail("the application's own listener did not receive CTRL_BREAK");
        }
        std::process::exit(0);
    }

    #[cfg(windows)]
    fn windows_wait_for_quit(escalate: bool) {
        let coord = installed(
            Coordinator::builder()
                .signals(SignalSet::standard())
                .force_exit_on_second_signal(escalate),
        );
        say("READY");
        if !coord.token().wait_blocking_timeout(WAIT) {
            fail("no console event arrived");
        }
        if coord.token().reason() != Some(ShutdownReason::Signal(Signal::Quit)) {
            fail(&format!("reason {:?}", coord.token().reason()));
        }
        say("INITIATED");
        if escalate {
            std::thread::sleep(WAIT);
            fail("second CTRL_BREAK did not exit");
        }
        std::process::exit(0);
    }
}
