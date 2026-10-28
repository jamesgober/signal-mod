//! The service status as a state machine, and what each control code
//! does: plain Rust, unit-tested on every platform. Only the Windows FFI
//! uses it outside tests.
#![cfg_attr(not(windows), allow(dead_code))]

use std::time::Duration;

use super::CONTROL_DUMP;
use crate::signal::Signal;

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
