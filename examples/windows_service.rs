//! A Windows service: install it with `sc create`, then start, signal, and
//! stop it with `sc`. Every control it receives is appended to the log
//! file named by its first argument (services have no console).
//!
//! ```text
//! cargo build --example windows_service --no-default-features --features std,agnostic,service
//! sc create signal-mod-example binPath= "C:\path\to\windows_service.exe C:\path\to\service.log"
//! sc start signal-mod-example
//! sc control signal-mod-example paramchange   (logged as User2: reload)
//! sc control signal-mod-example 128           (logged as User1: dump)
//! sc stop signal-mod-example                  (logged as Terminate)
//! sc delete signal-mod-example
//! ```
//!
//! Run from a console (not by the service manager), it says so and exits
//! with code 2.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use signal_mod::{Coordinator, Signal, SignalSet, service};

fn log(path: &PathBuf, line: &str) {
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: windows_service <log file>");
        return ExitCode::from(2);
    };
    let coordinator = Coordinator::builder()
        .signals(SignalSet::empty().with(Signal::Terminate))
        .notify_signals(SignalSet::empty().with(Signal::User1).with(Signal::User2))
        .build();
    if let Err(error) = coordinator.install() {
        log(&path, &format!("install failed: {error}"));
        return ExitCode::from(1);
    }
    let mut events = coordinator.subscribe();
    let service_log = path.clone();
    let ran = service::run("signal-mod-example", move |status| {
        let _ = status.starting(Duration::from_secs(5));
        let _ = status.running();
        log(&service_log, "running");
        while let Some(signal) = events.recv_blocking() {
            log(&service_log, &format!("{signal:?}"));
            if signal == Signal::Terminate {
                let _ = status.stopping(Duration::from_secs(5));
                break;
            }
        }
        log(&service_log, "stopped");
        0
    });
    match ran {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("not started as a service: {error}");
            log(&path, &format!("not a service: {error}"));
            ExitCode::from(2)
        }
    }
}
