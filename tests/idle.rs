//! An installed coordinator costs nothing while nothing happens: its
//! threads (the signal listener, the parked hook worker) block in the
//! kernel and are never woken until a signal arrives or a run starts.
//!
//! Linux only, where `/proc` reports per-thread context switches. Its own
//! test binary, so no other test's threads are counted.

#![cfg(not(loom))]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions

#[cfg(all(target_os = "linux", feature = "agnostic"))]
#[test]
fn test_installed_coordinator_has_no_idle_wake_ups() {
    use std::time::Duration;

    use signal_mod::{Backend, Coordinator, Signal, SignalSet, hook_from_fn};

    /// Context switches (voluntary + involuntary) of every signal-mod
    /// thread, by thread id. `comm` is cut to 15 bytes.
    fn switches() -> Vec<(String, u64)> {
        let mut found = Vec::new();
        for task in std::fs::read_dir("/proc/self/task").unwrap() {
            let dir = task.unwrap().path();
            let Ok(comm) = std::fs::read_to_string(dir.join("comm")) else {
                continue;
            };
            if !comm.starts_with("signal-mod") {
                continue;
            }
            let Ok(status) = std::fs::read_to_string(dir.join("status")) else {
                continue;
            };
            let total: u64 = status
                .lines()
                .filter(|l| l.contains("ctxt_switches"))
                .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
                .sum();
            found.push((dir.display().to_string(), total));
        }
        found.sort();
        found
    }

    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .notify_signals(SignalSet::empty().with(Signal::User1))
        .hook(hook_from_fn("flush", 0, |_| {}))
        .build();
    coord.install().unwrap();
    assert_eq!(coord.backend(), Some(Backend::Agnostic));
    let _sub = coord.subscribe();

    // Let both threads start and block.
    std::thread::sleep(Duration::from_millis(200));
    let before = switches();
    assert_eq!(before.len(), 2, "listener and hook worker: {before:?}");
    std::thread::sleep(Duration::from_secs(1));
    let after = switches();
    assert_eq!(before, after, "an idle coordinator's threads were woken");
}
