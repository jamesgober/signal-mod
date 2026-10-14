//! `install` / `uninstall` for every back-end and feature combination.
//!
//! These tests register real OS handlers in the test process but never
//! deliver a signal; delivery is covered by `real_signals.rs`, which
//! uses child processes.

#![cfg(not(loom))]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test assertions

#[allow(unused_imports)] // not every feature combination uses every item
use signal_mod::{Backend, Coordinator, Error, Signal, SignalSet};

#[cfg(not(any(feature = "tokio", feature = "agnostic")))]
#[test]
fn test_install_without_backend_feature_returns_no_runtime() {
    let coord = Coordinator::builder().build();
    assert!(matches!(coord.install(), Err(Error::NoRuntime)));
    assert!(!coord.is_installed());
    assert_eq!(coord.backend(), None);
}

// SM-006: 1.0.0 panicked outside a runtime and left `installed` set.
#[cfg(all(feature = "tokio", not(feature = "agnostic")))]
#[test]
fn test_install_outside_runtime_with_only_tokio_errors_and_stays_clean() {
    let coord = Coordinator::builder().build();
    let err = coord.install().unwrap_err();
    assert!(matches!(err, Error::OutsideTokioRuntime), "{err}");
    assert!(!coord.is_installed());
    // Retrying inside a runtime works: nothing was left half-installed.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        coord.install().unwrap();
        assert_eq!(coord.backend(), Some(Backend::Tokio));
        assert!(coord.uninstall());
    });
}

#[cfg(all(feature = "tokio", unix))]
#[test]
fn test_install_on_runtime_without_io_driver_errors_instead_of_panicking() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let coord = Coordinator::builder().build();
        match coord.install() {
            Err(Error::SignalRegistration { signal, source }) => {
                assert_eq!(signal, Signal::Terminate);
                assert!(source.to_string().contains("enable_io"), "{source}");
            }
            other => panic!("expected SignalRegistration, got {other:?}"),
        }
        assert!(!coord.is_installed());
    });
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_tokio_install_twice_returns_already_installed() {
    let coord = Coordinator::builder().build();
    coord.install().unwrap();
    assert_eq!(coord.backend(), Some(Backend::Tokio));
    assert!(matches!(coord.install(), Err(Error::AlreadyInstalled)));
    assert!(coord.is_installed());
    assert!(coord.statistics().installed);
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_tokio_uninstall_then_reinstall_succeeds() {
    let coord = Coordinator::builder().build();
    coord.install().unwrap();
    assert!(coord.uninstall());
    assert!(!coord.is_installed());
    assert!(!coord.uninstall());
    coord.install().unwrap();
    assert!(coord.is_installed());
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_tokio_installed_signals_cover_shutdown_and_notify_sets() {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .notify_signals(SignalSet::from(Signal::User1))
        .build();
    coord.install().unwrap();
    let expected = (SignalSet::graceful() | Signal::User1).available_on_current_platform();
    assert_eq!(coord.installed_signals(), expected);
}

// Audit P8: several coordinators in one process compose.
#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_two_coordinators_install_side_by_side() {
    let a = Coordinator::builder().build();
    let b = Coordinator::builder().build();
    a.install().unwrap();
    b.install().unwrap();
    drop(a);
    assert!(b.is_installed());
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_tokio_empty_set_installs_as_no_op() {
    let coord = Coordinator::builder().signals(SignalSet::empty()).build();
    coord.install().unwrap();
    assert!(coord.is_installed());
    assert_eq!(coord.installed_signals(), SignalSet::empty());
}

#[cfg(feature = "agnostic")]
#[test]
fn test_agnostic_install_outside_runtime_uses_agnostic_backend() {
    let coord = Coordinator::builder()
        .signals(SignalSet::graceful())
        .notify_signals(SignalSet::all() - SignalSet::graceful() - Signal::Pipe)
        .build();
    coord.install().unwrap();
    assert_eq!(coord.backend(), Some(Backend::Agnostic));
    let expected = (SignalSet::all() - Signal::Pipe).available_on_current_platform();
    assert_eq!(coord.installed_signals(), expected);
    assert!(matches!(coord.install(), Err(Error::AlreadyInstalled)));
    assert!(coord.uninstall());
    coord.install().unwrap();
    assert!(coord.uninstall());
}

#[cfg(all(feature = "agnostic", feature = "tokio"))]
#[tokio::test]
async fn test_inside_runtime_tokio_backend_wins_over_agnostic() {
    let coord = Coordinator::builder().build();
    coord.install().unwrap();
    assert_eq!(coord.backend(), Some(Backend::Tokio));
}

#[cfg(any(feature = "agnostic", feature = "tokio"))]
#[test]
fn test_shutdown_signal_installs_or_explains_why_not() {
    match signal_mod::shutdown_signal() {
        Ok(sig) => {
            assert_eq!(sig.reason(), None);
            assert!(!sig.token().is_initiated());
        }
        Err(e) => assert!(matches!(e, Error::OutsideTokioRuntime), "{e}"),
    }
}

#[cfg(feature = "tokio")]
#[tokio::test]
async fn test_shutdown_signal_inside_runtime_installs() {
    let sig = signal_mod::shutdown_signal().unwrap();
    assert!(format!("{sig:?}").contains("ShutdownSignal"));
    let token = sig.token();
    // Not resolved: no signal has arrived.
    let pending = tokio::time::timeout(std::time::Duration::from_millis(20), sig).await;
    assert!(pending.is_err());
    assert!(!token.is_initiated());
}
