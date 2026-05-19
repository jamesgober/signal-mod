//! Internal shutdown state shared by [`ShutdownToken`] and
//! [`ShutdownTrigger`].
//!
//! Implementation detail; not part of the public API.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};

use crate::reason::ShutdownReason;

#[derive(Debug, Default)]
struct State {
    reason: Option<ShutdownReason>,
    time: Option<Instant>,
}

#[derive(Debug)]
pub(crate) struct Inner {
    initiated: AtomicBool,
    state: Mutex<State>,
    cv: Condvar,
    #[cfg(feature = "tokio")]
    pub(crate) tx: tokio::sync::broadcast::Sender<ShutdownReason>,
}

impl Inner {
    pub(crate) fn new() -> Arc<Self> {
        #[cfg(feature = "tokio")]
        let (tx, _) = tokio::sync::broadcast::channel(16);
        Arc::new(Self {
            initiated: AtomicBool::new(false),
            state: Mutex::new(State::default()),
            cv: Condvar::new(),
            #[cfg(feature = "tokio")]
            tx,
        })
    }

    pub(crate) fn is_initiated(&self) -> bool {
        self.initiated.load(Ordering::Relaxed)
    }

    pub(crate) fn reason(&self) -> Option<ShutdownReason> {
        if !self.is_initiated() {
            return None;
        }
        self.state.lock().reason
    }

    pub(crate) fn elapsed(&self) -> Option<Duration> {
        if !self.is_initiated() {
            return None;
        }
        self.state.lock().time.map(|t| t.elapsed())
    }

    /// Initiate shutdown. Returns `true` if this call performed the
    /// transition; `false` if the state was already initiated.
    pub(crate) fn trigger(&self, reason: ShutdownReason) -> bool {
        if self
            .initiated
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            {
                let mut state = self.state.lock();
                state.reason = Some(reason);
                state.time = Some(Instant::now());
            }
            self.cv.notify_all();
            #[cfg(feature = "tokio")]
            {
                let _ = self.tx.send(reason);
            }
            true
        } else {
            false
        }
    }

    pub(crate) fn wait_blocking(&self) {
        if self.is_initiated() {
            return;
        }
        let mut guard = self.state.lock();
        while !self.is_initiated() {
            self.cv.wait(&mut guard);
        }
    }

    pub(crate) fn wait_blocking_timeout(&self, timeout: Duration) -> bool {
        if self.is_initiated() {
            return true;
        }
        let mut guard = self.state.lock();
        let deadline = Instant::now() + timeout;
        loop {
            if self.is_initiated() {
                return true;
            }
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let remaining = deadline - now;
            let result = self.cv.wait_for(&mut guard, remaining);
            if self.is_initiated() {
                return true;
            }
            if result.timed_out() {
                return false;
            }
        }
    }
}
