//! Windows service control (feature `service`).
//!
//! A Windows service never receives a console event when it is asked to
//! stop: the service control manager (SCM) sends control codes to a
//! handler the service registers. [`run`] connects the process to the
//! SCM, runs the service's work on the thread the SCM provides, reports
//! the service's status, and turns control codes into the same
//! [`Signal`]s every installed [`Coordinator`](crate::Coordinator)
//! already receives:
//!
//! | Control code | Delivered as | Typical meaning |
//! | --- | --- | --- |
//! | `SERVICE_CONTROL_STOP` | [`Signal::Terminate`] | `sc stop`, the Services console |
//! | `SERVICE_CONTROL_SHUTDOWN` | [`Signal::Terminate`] | the machine is shutting down |
//! | `SERVICE_CONTROL_PRESHUTDOWN` | [`Signal::Terminate`] | as above, earlier |
//! | `SERVICE_CONTROL_PARAMCHANGE` | [`Signal::User2`] | reload the configuration |
//! | user code `128` | [`Signal::User1`] | write a state dump |
//! | `SERVICE_CONTROL_INTERROGATE` | (answered with the current status) | |
//!
//! On Windows nothing else produces `User1` or `User2`, so a program can
//! add them to its notify set ([`CoordinatorBuilder::notify_signals`])
//! to receive the reload and dump requests.
//!
//! On other platforms [`run`] returns [`io::ErrorKind::Unsupported`], so
//! portable code can call it unconditionally and fall back to running in
//! the foreground.
//!
//! [`CoordinatorBuilder::notify_signals`]: crate::CoordinatorBuilder::notify_signals
//!
//! # Example
//!
//! ```no_run
//! use std::time::Duration;
//! use signal_mod::{Coordinator, Signal, SignalSet, service};
//!
//! let coordinator = Coordinator::builder()
//!     .signals(SignalSet::empty().with(Signal::Terminate))
//!     .notify_signals(SignalSet::empty().with(Signal::User1).with(Signal::User2))
//!     .build();
//! coordinator.install()?;
//! let mut events = coordinator.subscribe();
//!
//! let ran = service::run("my-service", move |status| {
//!     let _ = status.running();
//!     while let Some(signal) = events.recv_blocking() {
//!         if signal == Signal::Terminate {
//!             let _ = status.stopping(Duration::from_secs(10));
//!             break;
//!         }
//!     }
//!     0 // the exit code reported to the SCM
//! });
//! if let Err(error) = ran {
//!     // Not started by the SCM: run in the foreground instead.
//!     eprintln!("not a service: {error}");
//! }
//! # Ok::<(), signal_mod::Error>(())
//! ```

use std::io;
use std::sync::Mutex;
use std::time::Duration;

use crate::signal::Signal;

#[cfg(windows)]
mod ffi;
#[cfg(windows)]
pub(crate) mod registry;

// Values of the Win32 service constants this module uses. Declared here
// (not taken from windows-sys) so the status logic below is plain Rust
// and is unit-tested on every platform.
pub(crate) const SERVICE_WIN32_OWN_PROCESS: u32 = 0x0000_0010;
pub(crate) const SERVICE_STOPPED: u32 = 1;
pub(crate) const SERVICE_START_PENDING: u32 = 2;
pub(crate) const SERVICE_STOP_PENDING: u32 = 3;
pub(crate) const SERVICE_RUNNING: u32 = 4;
pub(crate) const SERVICE_ACCEPT_STOP: u32 = 0x0000_0001;
pub(crate) const SERVICE_ACCEPT_SHUTDOWN: u32 = 0x0000_0004;
pub(crate) const SERVICE_ACCEPT_PARAMCHANGE: u32 = 0x0000_0008;
pub(crate) const SERVICE_CONTROL_STOP: u32 = 0x0000_0001;
pub(crate) const SERVICE_CONTROL_INTERROGATE: u32 = 0x0000_0004;
pub(crate) const SERVICE_CONTROL_SHUTDOWN: u32 = 0x0000_0005;
pub(crate) const SERVICE_CONTROL_PARAMCHANGE: u32 = 0x0000_0006;
pub(crate) const SERVICE_CONTROL_PRESHUTDOWN: u32 = 0x0000_000F;
/// The user-defined control code that asks for a state dump.
pub const CONTROL_DUMP: u32 = 128;
pub(crate) const NO_ERROR: u32 = 0;
pub(crate) const ERROR_CALL_NOT_IMPLEMENTED: u32 = 120;
pub(crate) const ERROR_SERVICE_SPECIFIC_ERROR: u32 = 1066;

/// What the control handler does with a control code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Action {
    /// Report `STOP_PENDING`, then deliver [`Signal::Terminate`].
    Stop,
    /// Deliver a notify-only signal.
    Notify(Signal),
    /// Report the current status again.
    Interrogate,
    /// Not a control this service handles.
    NotImplemented,
}

/// Maps a control code to what the handler does.
pub(crate) const fn action(control: u32) -> Action {
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN | SERVICE_CONTROL_PRESHUTDOWN => {
            Action::Stop
        }
        SERVICE_CONTROL_PARAMCHANGE => Action::Notify(Signal::User2),
        CONTROL_DUMP => Action::Notify(Signal::User1),
        SERVICE_CONTROL_INTERROGATE => Action::Interrogate,
        _ => Action::NotImplemented,
    }
}

/// The fields of a Win32 `SERVICE_STATUS`, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Report {
    pub(crate) service_type: u32,
    pub(crate) current_state: u32,
    pub(crate) controls_accepted: u32,
    pub(crate) win32_exit_code: u32,
    pub(crate) service_specific_exit_code: u32,
    pub(crate) check_point: u32,
    pub(crate) wait_hint: u32,
}

/// The status a service reports, as a state machine: `START_PENDING`
/// (checkpoints rising), `RUNNING`, `STOP_PENDING` (checkpoints rising),
/// `STOPPED`. Controls are accepted only while running, as the SCM
/// expects. Once stopped, nothing changes it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Machine {
    report: Report,
}

impl Machine {
    pub(crate) const fn new() -> Machine {
        Machine {
            report: Report {
                service_type: SERVICE_WIN32_OWN_PROCESS,
                current_state: SERVICE_START_PENDING,
                controls_accepted: 0,
                win32_exit_code: NO_ERROR,
                service_specific_exit_code: 0,
                check_point: 1,
                wait_hint: 30_000,
            },
        }
    }

    pub(crate) const fn report(&self) -> Report {
        self.report
    }

    pub(crate) const fn state(&self) -> u32 {
        self.report.current_state
    }

    /// Still starting; the SCM waits `wait_hint` for the next report.
    pub(crate) fn starting(&mut self, wait_hint: Duration) -> Option<Report> {
        if self.report.current_state != SERVICE_START_PENDING {
            return None;
        }
        self.report.check_point = self.report.check_point.saturating_add(1);
        self.report.wait_hint = millis(wait_hint);
        Some(self.report)
    }

    /// Started: accepts stop, shutdown, and parameter-change controls.
    pub(crate) fn running(&mut self) -> Option<Report> {
        if self.report.current_state != SERVICE_START_PENDING {
            return None;
        }
        self.report.current_state = SERVICE_RUNNING;
        self.report.controls_accepted =
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_PARAMCHANGE;
        self.report.check_point = 0;
        self.report.wait_hint = 0;
        Some(self.report)
    }

    /// Stopping; each call is a new checkpoint, so the SCM sees progress.
    pub(crate) fn stopping(&mut self, wait_hint: Duration) -> Option<Report> {
        if self.report.current_state == SERVICE_STOPPED {
            return None;
        }
        if self.report.current_state == SERVICE_STOP_PENDING {
            self.report.check_point = self.report.check_point.saturating_add(1);
        } else {
            self.report.current_state = SERVICE_STOP_PENDING;
            self.report.check_point = 1;
        }
        self.report.controls_accepted = 0;
        self.report.wait_hint = millis(wait_hint);
        Some(self.report)
    }

    /// Stopped, with the service's exit code (`0` for success).
    pub(crate) fn stopped(&mut self, exit_code: u32) -> Option<Report> {
        if self.report.current_state == SERVICE_STOPPED {
            return None;
        }
        self.report.current_state = SERVICE_STOPPED;
        self.report.controls_accepted = 0;
        self.report.check_point = 0;
        self.report.wait_hint = 0;
        if exit_code == 0 {
            self.report.win32_exit_code = NO_ERROR;
            self.report.service_specific_exit_code = 0;
        } else {
            self.report.win32_exit_code = ERROR_SERVICE_SPECIFIC_ERROR;
            self.report.service_specific_exit_code = exit_code;
        }
        Some(self.report)
    }
}

fn millis(duration: Duration) -> u32 {
    u32::try_from(duration.as_millis()).unwrap_or(u32::MAX)
}

/// The service's status, as reported to the SCM. Handed to the closure
/// given to [`run`]; `Send + Sync`, so a shutdown path on another thread
/// can report progress too.
///
/// The service starts in `START_PENDING`. Call [`running`](Self::running)
/// once it serves, [`stopping`](Self::stopping) while it shuts down (each
/// call is a new checkpoint), and return the exit code from the closure:
/// [`run`] reports `STOPPED` with it. A stop control reports
/// `STOP_PENDING` by itself before delivering [`Signal::Terminate`].
pub struct ServiceStatus {
    machine: Mutex<Machine>,
    #[cfg(windows)]
    handle: ffi::StatusHandle,
}

impl std::fmt::Debug for ServiceStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceStatus")
            .field("state", &self.lock().state())
            .finish_non_exhaustive()
    }
}

impl ServiceStatus {
    fn lock(&self) -> std::sync::MutexGuard<'_, Machine> {
        match self.machine.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Still starting: the SCM waits up to `wait_hint` for the next
    /// report. Does nothing once the service is running or stopping.
    ///
    /// # Errors
    ///
    /// The OS error from `SetServiceStatus`.
    pub fn starting(&self, wait_hint: Duration) -> io::Result<()> {
        let report = self.lock().starting(wait_hint);
        self.send(report)
    }

    /// The service is serving: it now accepts stop, shutdown, and
    /// parameter-change controls.
    ///
    /// # Errors
    ///
    /// The OS error from `SetServiceStatus`.
    pub fn running(&self) -> io::Result<()> {
        let report = self.lock().running();
        self.send(report)
    }

    /// The service is stopping: the SCM waits up to `wait_hint` for the
    /// next report. Each call is a new checkpoint.
    ///
    /// # Errors
    ///
    /// The OS error from `SetServiceStatus`.
    pub fn stopping(&self, wait_hint: Duration) -> io::Result<()> {
        let report = self.lock().stopping(wait_hint);
        self.send(report)
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn stopped(&self, exit_code: u32) -> io::Result<()> {
        let report = self.lock().stopped(exit_code);
        self.send(report)
    }

    #[cfg_attr(not(windows), allow(dead_code))]
    fn current(&self) -> Report {
        self.lock().report()
    }

    #[cfg(windows)]
    fn send(&self, report: Option<Report>) -> io::Result<()> {
        match report {
            Some(report) => self.handle.set(&report),
            None => Ok(()),
        }
    }

    #[cfg(not(windows))]
    #[allow(clippy::unused_self, clippy::needless_pass_by_value)]
    fn send(&self, _report: Option<Report>) -> io::Result<()> {
        Ok(())
    }
}

/// Runs the calling process as the Windows service `name`, and returns
/// when the service has stopped.
///
/// Call it from the main thread, after the program has installed its
/// [`Coordinator`](crate::Coordinator)s: it connects to the SCM, which
/// calls `service` on a thread of its own with the service's
/// [`ServiceStatus`] (starting in `START_PENDING`). The value `service`
/// returns is reported as the exit code (`0` for success) with
/// `STOPPED`. A panic in `service` is reported as exit code `1` and
/// then resumed on the calling thread.
///
/// # Errors
///
/// - [`io::ErrorKind::Unsupported`] on platforms other than Windows.
/// - On Windows, the OS error from `StartServiceCtrlDispatcherW`: most
///   often `ERROR_FAILED_SERVICE_CONTROLLER_CONNECT` (1063), which means
///   the process was not started by the SCM (run in the foreground
///   instead). `InvalidInput` for a name with a NUL character, and
///   `AlreadyExists` when a service is already running in this process.
pub fn run<F>(name: &str, service: F) -> io::Result<()>
where
    F: FnOnce(&ServiceStatus) -> u32 + Send + 'static,
{
    #[cfg(windows)]
    {
        ffi::run(name, Box::new(service))
    }
    #[cfg(not(windows))]
    {
        let _unused = (name, service);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows service control is only available on Windows",
        ))
    }
}

/// The service closure, boxed for the hand-over to the SCM's thread.
#[cfg(windows)]
type Body = Box<dyn FnOnce(&ServiceStatus) -> u32 + Send + 'static>;

#[cfg(all(test, not(loom)))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions

    use super::*;

    #[test]
    fn stop_shutdown_and_preshutdown_stop() {
        for control in [
            SERVICE_CONTROL_STOP,
            SERVICE_CONTROL_SHUTDOWN,
            SERVICE_CONTROL_PRESHUTDOWN,
        ] {
            assert_eq!(action(control), Action::Stop);
        }
    }

    #[test]
    fn paramchange_and_dump_notify() {
        assert_eq!(
            action(SERVICE_CONTROL_PARAMCHANGE),
            Action::Notify(Signal::User2)
        );
        assert_eq!(action(CONTROL_DUMP), Action::Notify(Signal::User1));
    }

    #[test]
    fn interrogate_and_unknown_codes() {
        assert_eq!(action(SERVICE_CONTROL_INTERROGATE), Action::Interrogate);
        for code in [0, 2, 3, 7, 129, 255, 0xFFFF_FFFF] {
            assert_eq!(action(code), Action::NotImplemented, "{code}");
        }
    }

    #[test]
    fn starts_pending_with_rising_checkpoints_and_accepts_nothing() {
        let mut machine = Machine::new();
        let first = machine.report();
        assert_eq!(first.current_state, SERVICE_START_PENDING);
        assert_eq!(first.controls_accepted, 0);
        assert_eq!(first.service_type, SERVICE_WIN32_OWN_PROCESS);
        let next = machine.starting(Duration::from_secs(5)).unwrap();
        assert_eq!(next.check_point, first.check_point + 1);
        assert_eq!(next.wait_hint, 5_000);
        assert_eq!(next.controls_accepted, 0);
    }

    #[test]
    fn running_accepts_stop_shutdown_paramchange() {
        let mut machine = Machine::new();
        let report = machine.running().unwrap();
        assert_eq!(report.current_state, SERVICE_RUNNING);
        assert_eq!(
            report.controls_accepted,
            SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_PARAMCHANGE
        );
        assert_eq!((report.check_point, report.wait_hint), (0, 0));
        // Starting again, or running twice, changes nothing.
        assert_eq!(machine.starting(Duration::from_secs(1)), None);
        assert_eq!(machine.running(), None);
    }

    #[test]
    fn stopping_counts_checkpoints_and_accepts_nothing() {
        let mut machine = Machine::new();
        let _running = machine.running();
        let first = machine.stopping(Duration::from_secs(10)).unwrap();
        assert_eq!(first.current_state, SERVICE_STOP_PENDING);
        assert_eq!(first.check_point, 1);
        assert_eq!(first.controls_accepted, 0);
        let second = machine.stopping(Duration::from_secs(10)).unwrap();
        assert_eq!(second.check_point, 2);
        assert_eq!(
            machine.running(),
            None,
            "a stopping service cannot run again"
        );
    }

    #[test]
    fn stopped_reports_the_exit_code_once() {
        let mut ok = Machine::new();
        let report = ok.stopped(0).unwrap();
        assert_eq!(report.current_state, SERVICE_STOPPED);
        assert_eq!(report.win32_exit_code, NO_ERROR);
        assert_eq!(ok.stopped(3), None);
        assert_eq!(ok.stopping(Duration::from_secs(1)), None);

        let mut failed = Machine::new();
        let _running = failed.running();
        let report = failed.stopped(79).unwrap();
        assert_eq!(report.win32_exit_code, ERROR_SERVICE_SPECIFIC_ERROR);
        assert_eq!(report.service_specific_exit_code, 79);
    }

    #[test]
    fn a_huge_wait_hint_saturates() {
        let mut machine = Machine::new();
        let report = machine.starting(Duration::from_secs(u64::MAX)).unwrap();
        assert_eq!(report.wait_hint, u32::MAX);
    }

    #[cfg(not(windows))]
    #[test]
    fn run_is_unsupported_off_windows() {
        let error = run("test", |_| 0).err().map(|e| e.kind());
        assert_eq!(error, Some(io::ErrorKind::Unsupported));
    }
}
