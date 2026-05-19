//! Cross-platform [`Signal`] enum and bit-packed [`SignalSet`].

use core::fmt;

/// A platform-neutral signal identifier.
///
/// Variants map to their nearest platform equivalent. On Unix the
/// mapping is direct (SIGTERM, SIGINT, etc.). On Windows the mapping
/// is to Windows console control events:
///
/// | Variant      | Unix     | Windows             |
/// | ------------ | -------- | ------------------- |
/// | `Terminate`  | SIGTERM  | `CTRL_CLOSE_EVENT`  |
/// | `Interrupt`  | SIGINT   | `CTRL_C_EVENT`      |
/// | `Quit`       | SIGQUIT  | `CTRL_BREAK_EVENT`  |
/// | `Hangup`     | SIGHUP   | `CTRL_SHUTDOWN_EVENT` |
/// | `Pipe`       | SIGPIPE  | inert               |
/// | `User1`      | SIGUSR1  | inert               |
/// | `User2`      | SIGUSR2  | inert               |
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Signal {
    /// SIGTERM on Unix, `CTRL_CLOSE_EVENT` on Windows.
    Terminate,
    /// SIGINT (Ctrl+C) on Unix, `CTRL_C_EVENT` on Windows.
    Interrupt,
    /// SIGQUIT on Unix, `CTRL_BREAK_EVENT` on Windows.
    Quit,
    /// SIGHUP on Unix, `CTRL_SHUTDOWN_EVENT` on Windows.
    Hangup,
    /// SIGPIPE (Unix only; inert on Windows).
    Pipe,
    /// SIGUSR1 (Unix only; inert on Windows).
    User1,
    /// SIGUSR2 (Unix only; inert on Windows).
    User2,
}

impl Signal {
    /// All defined variants, in canonical order.
    pub const ALL: [Self; 7] = [
        Self::Terminate,
        Self::Interrupt,
        Self::Quit,
        Self::Hangup,
        Self::Pipe,
        Self::User1,
        Self::User2,
    ];

    /// Human-readable description used by `Display` and logging.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Terminate => "Terminate (SIGTERM / CTRL_CLOSE_EVENT)",
            Self::Interrupt => "Interrupt (SIGINT / CTRL_C_EVENT)",
            Self::Quit => "Quit (SIGQUIT / CTRL_BREAK_EVENT)",
            Self::Hangup => "Hangup (SIGHUP / CTRL_SHUTDOWN_EVENT)",
            Self::Pipe => "Pipe (SIGPIPE, Unix only)",
            Self::User1 => "User1 (SIGUSR1, Unix only)",
            Self::User2 => "User2 (SIGUSR2, Unix only)",
        }
    }

    /// Unix signal number for this variant. Returns `None` for
    /// variants that have no canonical Unix number (none currently,
    /// but the API reserves the right to add Windows-only variants).
    #[must_use]
    pub const fn unix_number(self) -> Option<i32> {
        match self {
            Self::Hangup => Some(1),
            Self::Interrupt => Some(2),
            Self::Quit => Some(3),
            Self::User1 => Some(10),
            Self::User2 => Some(12),
            Self::Pipe => Some(13),
            Self::Terminate => Some(15),
        }
    }

    /// Returns `true` for variants that have no Windows analog.
    #[must_use]
    pub const fn is_unix_only(self) -> bool {
        matches!(self, Self::Pipe | Self::User1 | Self::User2)
    }

    /// Returns `true` if installing a handler for this signal is
    /// expected to succeed on the platform this binary is running on.
    #[must_use]
    pub const fn available_on_current_platform(self) -> bool {
        if cfg!(unix) {
            true
        } else {
            !self.is_unix_only()
        }
    }

    pub(crate) const fn bit(self) -> u16 {
        match self {
            Self::Terminate => 1 << 0,
            Self::Interrupt => 1 << 1,
            Self::Quit => 1 << 2,
            Self::Hangup => 1 << 3,
            Self::Pipe => 1 << 4,
            Self::User1 => 1 << 5,
            Self::User2 => 1 << 6,
        }
    }
}

impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.description())
    }
}

/// A bit-packed set of [`Signal`] values.
///
/// `SignalSet` is `Copy` and const-constructible. The recommended
/// constructors are [`SignalSet::graceful`] (the default for
/// long-running services) and [`SignalSet::standard`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalSet {
    bits: u16,
}

impl SignalSet {
    /// Bit mask covering every defined variant.
    const ALL_BITS: u16 = 0b0111_1111;

    /// Empty set.
    #[must_use]
    pub const fn empty() -> Self {
        Self { bits: 0 }
    }

    /// All defined variants enabled. Inert variants on the current
    /// platform are still represented but ignored by `install`.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            bits: Self::ALL_BITS,
        }
    }

    /// The recommended default: `Terminate | Interrupt | Hangup`.
    #[must_use]
    pub const fn graceful() -> Self {
        Self::empty()
            .with(Signal::Terminate)
            .with(Signal::Interrupt)
            .with(Signal::Hangup)
    }

    /// Maximum graceful coverage: `Terminate | Interrupt | Quit | Hangup`.
    #[must_use]
    pub const fn standard() -> Self {
        Self::graceful().with(Signal::Quit)
    }

    /// Return a copy of `self` with `sig` enabled.
    #[must_use]
    pub const fn with(self, sig: Signal) -> Self {
        Self {
            bits: self.bits | sig.bit(),
        }
    }

    /// Return a copy of `self` with `sig` disabled.
    #[must_use]
    pub const fn without(self, sig: Signal) -> Self {
        Self {
            bits: self.bits & !sig.bit(),
        }
    }

    /// Check whether `sig` is enabled.
    #[must_use]
    pub const fn contains(self, sig: Signal) -> bool {
        (self.bits & sig.bit()) != 0
    }

    /// `true` if the set has no signals enabled.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.bits == 0
    }

    /// Number of signals enabled in the set.
    #[must_use]
    pub const fn len(self) -> usize {
        self.bits.count_ones() as usize
    }

    /// Iterate the enabled signals in canonical [`Signal::ALL`] order.
    #[must_use]
    pub const fn iter(&self) -> SignalSetIter {
        SignalSetIter {
            set: *self,
            index: 0,
        }
    }
}

impl Default for SignalSet {
    fn default() -> Self {
        Self::graceful()
    }
}

impl IntoIterator for SignalSet {
    type Item = Signal;
    type IntoIter = SignalSetIter;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the signals enabled in a [`SignalSet`].
#[derive(Debug, Clone)]
pub struct SignalSetIter {
    set: SignalSet,
    index: usize,
}

impl Iterator for SignalSetIter {
    type Item = Signal;

    fn next(&mut self) -> Option<Signal> {
        while self.index < Signal::ALL.len() {
            let sig = Signal::ALL[self.index];
            self.index += 1;
            if self.set.contains(sig) {
                return Some(sig);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_display_matches_description() {
        for s in Signal::ALL {
            assert_eq!(format!("{s}"), s.description());
        }
    }

    #[test]
    fn signal_unix_number_round_trip() {
        assert_eq!(Signal::Terminate.unix_number(), Some(15));
        assert_eq!(Signal::Interrupt.unix_number(), Some(2));
        assert_eq!(Signal::Hangup.unix_number(), Some(1));
        assert_eq!(Signal::Quit.unix_number(), Some(3));
        assert_eq!(Signal::Pipe.unix_number(), Some(13));
        assert_eq!(Signal::User1.unix_number(), Some(10));
        assert_eq!(Signal::User2.unix_number(), Some(12));
    }

    #[test]
    fn is_unix_only_is_correct() {
        assert!(Signal::Pipe.is_unix_only());
        assert!(Signal::User1.is_unix_only());
        assert!(Signal::User2.is_unix_only());
        assert!(!Signal::Terminate.is_unix_only());
        assert!(!Signal::Interrupt.is_unix_only());
        assert!(!Signal::Quit.is_unix_only());
        assert!(!Signal::Hangup.is_unix_only());
    }

    #[test]
    fn set_empty_and_all() {
        assert!(SignalSet::empty().is_empty());
        assert_eq!(SignalSet::empty().len(), 0);
        assert_eq!(SignalSet::all().len(), 7);
    }

    #[test]
    fn set_graceful_contents() {
        let g = SignalSet::graceful();
        assert!(g.contains(Signal::Terminate));
        assert!(g.contains(Signal::Interrupt));
        assert!(g.contains(Signal::Hangup));
        assert!(!g.contains(Signal::Quit));
        assert!(!g.contains(Signal::Pipe));
        assert_eq!(g.len(), 3);
    }

    #[test]
    fn set_standard_contents() {
        let s = SignalSet::standard();
        assert!(s.contains(Signal::Quit));
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn with_without_idempotent() {
        let s = SignalSet::empty()
            .with(Signal::Terminate)
            .with(Signal::Terminate);
        assert_eq!(s.len(), 1);
        let s = s.without(Signal::Terminate).without(Signal::Terminate);
        assert!(s.is_empty());
    }

    #[test]
    fn iter_canonical_order() {
        let s = SignalSet::all();
        let v: Vec<Signal> = s.iter().collect();
        assert_eq!(v, Signal::ALL.to_vec());
    }

    #[test]
    fn default_is_graceful() {
        assert_eq!(SignalSet::default(), SignalSet::graceful());
    }
}
