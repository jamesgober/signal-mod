//! The Win32 calls behind [`run`](super::run): the dispatcher, the
//! `ServiceMain` entry point, the control handler, and status reports.
//!
//! The SCM's callbacks take no context the program chooses before the
//! dispatcher starts, so the service body and the live status are handed
//! over through process-globals, touched only under their mutexes. One
//! service runs per process (`SERVICE_WIN32_OWN_PROCESS`).

// The FFI calls below; each `unsafe` block carries a `SAFETY:`
// justification. Everything else in the crate stays safe.
#![allow(unsafe_code)]

use std::any::Any;
use std::ffi::c_void;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_TABLE_ENTRYW,
    SetServiceStatus, StartServiceCtrlDispatcherW,
};
use windows_sys::core::PWSTR;

use super::{
    Action, Body, ERROR_CALL_NOT_IMPLEMENTED, Machine, NO_ERROR, Report, ServiceStatus, action,
};
use crate::signal::Signal;

/// How long the SCM waits after a stop control before the program's
/// first own `stopping` report.
const STOP_WAIT_HINT: Duration = Duration::from_secs(30);

/// A `SERVICE_STATUS_HANDLE`.
pub(super) struct StatusHandle(SERVICE_STATUS_HANDLE);

// SAFETY: a service status handle is an opaque value the SCM gives out
// once per service; `SetServiceStatus` may be called with it from any
// thread, it is never freed (Microsoft: "The service status handle does
// not have to be closed"), and this type never dereferences it.
unsafe impl Send for StatusHandle {}
// SAFETY: as above; sharing the value between threads only shares the
// right to call `SetServiceStatus`, which is thread-safe.
unsafe impl Sync for StatusHandle {}

impl StatusHandle {
    pub(super) fn set(&self, report: &Report) -> io::Result<()> {
        let status = SERVICE_STATUS {
            dwServiceType: report.service_type,
            dwCurrentState: report.current_state,
            dwControlsAccepted: report.controls_accepted,
            dwWin32ExitCode: report.win32_exit_code,
            dwServiceSpecificExitCode: report.service_specific_exit_code,
            dwCheckPoint: report.check_point,
            dwWaitHint: report.wait_hint,
        };
        // SAFETY: `self.0` is the handle `RegisterServiceCtrlHandlerExW`
        // returned (checked non-null before a `StatusHandle` is built),
        // and `status` is a fully initialised `SERVICE_STATUS` that lives
        // across the call; the function only reads it.
        let ok = unsafe { SetServiceStatus(self.0, &raw const status) };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// The service waiting for `ServiceMain`.
struct Pending {
    name: Vec<u16>,
    body: Body,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);
/// The running service's status, for the control handler.
static ACTIVE: Mutex<Option<Arc<ServiceStatus>>> = Mutex::new(None);
/// A panic from the service body, resumed on the dispatcher's thread.
static PANIC: Mutex<Option<Box<dyn Any + Send>>> = Mutex::new(None);
/// The OS error code when `ServiceMain` could not register its handler.
static REGISTER_ERROR: Mutex<Option<i32>> = Mutex::new(None);
/// Whether a service runs in this process.
static RUNNING: AtomicBool = AtomicBool::new(false);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub(super) fn run(name: &str, body: Body) -> io::Result<()> {
    if name.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a service name cannot contain a NUL character",
        ));
    }
    if RUNNING.swap(true, Ordering::AcqRel) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "a service is already running in this process",
        ));
    }
    let mut wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    *lock(&PENDING) = Some(Pending {
        name: wide.clone(),
        body,
    });
    *lock(&REGISTER_ERROR) = None;
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: wide.as_mut_ptr(),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: std::ptr::null_mut(),
            lpServiceProc: None,
        },
    ];
    // SAFETY: `table` is a valid, null-terminated array of
    // `SERVICE_TABLE_ENTRYW` whose name points into `wide`, a
    // NUL-terminated UTF-16 string; both outlive the call, which blocks
    // until every service in the table has stopped. `service_main` has
    // exactly the `LPSERVICE_MAIN_FUNCTIONW` signature and never unwinds.
    let ok = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    let dispatched = if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    };
    // The body never ran if the dispatcher failed; drop it here.
    drop(lock(&PENDING).take());
    RUNNING.store(false, Ordering::Release);
    dispatched?;
    if let Some(code) = lock(&REGISTER_ERROR).take() {
        return Err(io::Error::from_raw_os_error(code));
    }
    if let Some(payload) = lock(&PANIC).take() {
        panic::resume_unwind(payload);
    }
    Ok(())
}

/// `ServiceMain`: runs on a thread the SCM creates.
unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    // A panic must not unwind across the FFI boundary; the body's own
    // panic is caught inside and resumed by `run`.
    let _ran = panic::catch_unwind(serve);
}

fn serve() {
    let Some(Pending { name, body }) = lock(&PENDING).take() else {
        return;
    };
    // SAFETY: `name` is a NUL-terminated UTF-16 string that outlives the
    // call; `control_handler` has exactly the `LPHANDLER_FUNCTION_EX`
    // signature, never unwinds, and ignores its context pointer, which is
    // null.
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control_handler), std::ptr::null())
    };
    if handle.is_null() {
        *lock(&REGISTER_ERROR) = io::Error::last_os_error().raw_os_error();
        return;
    }
    let status = Arc::new(ServiceStatus {
        machine: Mutex::new(Machine::new()),
        handle: StatusHandle(handle),
    });
    // The first report: START_PENDING. A failure here cannot be reported
    // anywhere but the SCM, which will time the start out.
    let _reported = status.handle.set(&status.current());
    *lock(&ACTIVE) = Some(Arc::clone(&status));
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| body(&status)));
    let exit_code = match outcome {
        Ok(code) => code,
        Err(payload) => {
            *lock(&PANIC) = Some(payload);
            1
        }
    };
    drop(lock(&ACTIVE).take());
    // After STOPPED the SCM may end the process at any time; nothing
    // follows this report but returning to the dispatcher.
    let _reported = status.stopped(exit_code);
}

/// `HandlerEx`: runs on the dispatcher's thread for each control code.
unsafe extern "system" fn control_handler(
    control: u32,
    _event_type: u32,
    _event_data: *mut c_void,
    _context: *mut c_void,
) -> u32 {
    panic::catch_unwind(|| handle(control)).unwrap_or(ERROR_CALL_NOT_IMPLEMENTED)
}

fn handle(control: u32) -> u32 {
    let active = lock(&ACTIVE).clone();
    match action(control) {
        Action::Stop => {
            if let Some(status) = &active {
                let _reported = status.stopping(STOP_WAIT_HINT);
            }
            let _delivered = super::registry::dispatch(Signal::Terminate);
            NO_ERROR
        }
        Action::Notify(signal) => {
            let _delivered = super::registry::dispatch(signal);
            NO_ERROR
        }
        Action::Interrogate => {
            if let Some(status) = &active {
                let _reported = status.handle.set(&status.current());
            }
            NO_ERROR
        }
        Action::NotImplemented => ERROR_CALL_NOT_IMPLEMENTED,
    }
}
