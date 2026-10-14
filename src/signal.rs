//! Cross-platform [`Signal`] enum and bit-packed [`SignalSet`].

use core::fmt;
use core::iter::FusedIterator;
use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Sub, SubAssign};

/// A platform-neutral signal identifier.
///
/// Variants map to their nearest platform equivalent. On Unix the
/// mapping is direct. On Windows the mapping is to console control
/// events:
///
/// | Variant      | Unix     | Windows                                      |
/// | ------------ | -------- | -------------------------------------------- |
/// | `Terminate`  | SIGTERM  | `CTRL_CLOSE_EVENT`                           |
/// | `Interrupt`  | SIGINT   | `CTRL_C_EVENT`                               |
/// | `Quit`       | SIGQUIT  | `CTRL_BREAK_EVENT`                           |
/// | `Hangup`     | SIGHUP   | `CTRL_LOGOFF_EVENT`, `CTRL_SHUTDOWN_EVENT`   |
/// | `Pipe`       | SIGPIPE  | never delivered                              |
/// | `User1`      | SIGUSR1  | never delivered                              |
/// | `User2`      | SIGUSR2  | never delivered                              |
///
/// Windows caveats, from the `HandlerRoutine` documentation:
///
/// - `CTRL_LOGOFF_EVENT` and `CTRL_SHUTDOWN_EVENT` reach services
///   only. An interactive console program is terminated at logoff or
///   shutdown without seeing either event.
/// - After `CTRL_CLOSE_EVENT` (console window closed) the operating
///   system terminates the process about five seconds later, whatever
///   the program is doing. Keep the hook budget below that when console
///   close matters (see
///   [`CoordinatorBuilder::graceful_timeout`](crate::CoordinatorBuilder::graceful_timeout)).
///
/// # Examples
///
/// ```
/// use signal_mod::Signal;
///
/// assert!(Signal::User1.is_unix_only());
/// assert_eq!(Signal::Interrupt.exit_code(), 130);
/// assert_eq!(Signal::Terminate.unix_number(), Some(15));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Signal {
    /// SIGTERM on Unix, `CTRL_CLOSE_EVENT` on Windows. The polite
    /// "please stop" sent by `kill`, systemd, Docker and Kubernetes.
    Terminate,
    /// SIGINT (Ctrl+C) on Unix, `CTRL_C_EVENT` on Windows.
    Interrupt,
    /// SIGQUIT (Ctrl+\\) on Unix, `CTRL_BREAK_EVENT` on Windows.
    Quit,
    /// SIGHUP on Unix; `CTRL_LOGOFF_EVENT` and `CTRL_SHUTDOWN_EVENT`
    /// on Windows.
    ///
    /// Daemons conventionally treat SIGHUP as "reload configuration"
    /// rather than "stop". [`SignalSet::graceful`] includes it as a
    /// shutdown signal; to use it for reload instead, remove it from
    /// the shutdown set and add it to
    /// [`CoordinatorBuilder::notify_signals`](crate::CoordinatorBuilder::notify_signals).
    Hangup,
    /// SIGPIPE (Unix only; never delivered on Windows).
    ///
    /// The Rust runtime ignores SIGPIPE by default, so writing to a
    /// closed pipe or socket returns `EPIPE` instead of killing the
    /// process. Installing a handler for `Pipe` replaces that: with
    /// `Pipe` in the shutdown set, any write to a closed pipe (for
    /// example `prog | head`, or a disconnected child's stdin) starts a
    /// graceful shutdown of the whole process. Only add it when that is
    /// what you want.
    Pipe,
    /// SIGUSR1 (Unix only; never delivered on Windows). Conventionally
    /// "reopen log files"; usually belongs in
    /// [`CoordinatorBuilder::notify_signals`](crate::CoordinatorBuilder::notify_signals).
    User1,
    /// SIGUSR2 (Unix only; never delivered on Windows).
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
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::Signal;
    ///
    /// assert_eq!(Signal::Pipe.description(), "Pipe (SIGPIPE, Unix only)");
    /// ```
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Terminate => "Terminate (SIGTERM / CTRL_CLOSE_EVENT)",
            Self::Interrupt => "Interrupt (SIGINT / CTRL_C_EVENT)",
            Self::Quit => "Quit (SIGQUIT / CTRL_BREAK_EVENT)",
            Self::Hangup => "Hangup (SIGHUP / CTRL_LOGOFF_EVENT / CTRL_SHUTDOWN_EVENT)",
            Self::Pipe => "Pipe (SIGPIPE, Unix only)",
            Self::User1 => "User1 (SIGUSR1, Unix only)",
            Self::User2 => "User2 (SIGUSR2, Unix only)",
        }
    }

    /// The Unix signal number of this variant **on the target the
    /// binary was compiled for**.
    ///
    /// Signal numbers are not portable: `SIGUSR1` is 10 on Linux x86 and
    /// ARM, 16 on MIPS Linux, and 30 on macOS and the BSDs. On Unix
    /// targets the value comes from the platform's `libc` constants.
    ///
    /// Non-Unix targets have no native number. There the function
    /// returns the numbers POSIX fixes for every system (HUP 1, INT 2,
    /// QUIT 3, TERM 15) plus PIPE 13, which every Unix shares, and
    /// `None` for `User1` / `User2`, whose numbers differ between
    /// systems.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::Signal;
    ///
    /// assert_eq!(Signal::Interrupt.unix_number(), Some(2));
    /// for sig in Signal::ALL {
    ///     if let Some(n) = sig.unix_number() {
    ///         assert_eq!(Signal::from_unix_number(n), Some(sig));
    ///     }
    /// }
    /// ```
    #[must_use]
    pub const fn unix_number(self) -> Option<i32> {
        #[cfg(unix)]
        {
            Some(match self {
                Self::Hangup => libc::SIGHUP,
                Self::Interrupt => libc::SIGINT,
                Self::Quit => libc::SIGQUIT,
                Self::User1 => libc::SIGUSR1,
                Self::User2 => libc::SIGUSR2,
                Self::Pipe => libc::SIGPIPE,
                Self::Terminate => libc::SIGTERM,
            })
        }
        #[cfg(not(unix))]
        {
            match self {
                Self::Hangup => Some(1),
                Self::Interrupt => Some(2),
                Self::Quit => Some(3),
                Self::Pipe => Some(13),
                Self::Terminate => Some(15),
                Self::User1 | Self::User2 => None,
            }
        }
    }

    /// Map a signal number (as returned by
    /// [`unix_number`](Self::unix_number) on this target) back to its
    /// variant. Returns `None` for numbers no variant represents.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::Signal;
    ///
    /// assert_eq!(Signal::from_unix_number(15), Some(Signal::Terminate));
    /// assert_eq!(Signal::from_unix_number(-1), None);
    /// ```
    #[must_use]
    pub const fn from_unix_number(number: i32) -> Option<Self> {
        let mut i = 0;
        while i < Self::ALL.len() {
            let sig = Self::ALL[i];
            if let Some(n) = sig.unix_number() {
                if n == number {
                    return Some(sig);
                }
            }
            i += 1;
        }
        None
    }

    /// Conventional exit status for a program stopped by this signal:
    /// `128 + signal number` (130 for SIGINT, 143 for SIGTERM), the
    /// value shells, CI systems and container runtimes expect. Non-Unix
    /// targets use the POSIX numbers listed under
    /// [`unix_number`](Self::unix_number), so Ctrl+C maps to 130
    /// everywhere; `User1` / `User2` there have no number and map to 1.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::Signal;
    ///
    /// assert_eq!(Signal::Interrupt.exit_code(), 130);
    /// assert_eq!(Signal::Terminate.exit_code(), 143);
    /// ```
    #[must_use]
    pub const fn exit_code(self) -> i32 {
        match self.unix_number() {
            Some(n) => 128 + n,
            None => 1,
        }
    }

    /// Returns `true` for variants that have no Windows analog.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::Signal;
    ///
    /// assert!(Signal::Pipe.is_unix_only());
    /// assert!(!Signal::Terminate.is_unix_only());
    /// ```
    #[must_use]
    pub const fn is_unix_only(self) -> bool {
        matches!(self, Self::Pipe | Self::User1 | Self::User2)
    }

    /// Returns `true` if the platform this binary was compiled for can
    /// deliver this signal: every variant on Unix, all but the
    /// Unix-only ones on Windows.
    ///
    /// # Examples
    ///
    /// ```
    /// use signal_mod::Signal;
    ///
    /// assert!(Signal::Interrupt.available_on_current_platform());
    /// assert_eq!(Signal::User1.available_on_current_platform(), cfg!(unix));
    /// ```
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
/// `SignalSet` is `Copy` and const-constructible, and supports the set
/// operators `|` (union), `&` (intersection) and `-` (difference), with
/// either another set or a single [`Signal`] on the right. The usual
/// constructors are [`SignalSet::graceful`] (also the `Default`) and
/// [`SignalSet::standard`].
///
/// # Examples
///
/// ```
/// use signal_mod::{Signal, SignalSet};
///
/// const RELOAD: SignalSet = SignalSet::empty().with(Signal::Hangup).with(Signal::User1);
/// let shutdown = SignalSet::graceful() - RELOAD;
///
/// assert!(shutdown.contains(Signal::Terminate));
/// assert!(!shutdown.contains(Signal::Hangup));
/// assert_eq!((shutdown | RELOAD).len(), 4);
///
/// let collected: SignalSet = [Signal::Quit, Signal::Pipe].into_iter().collect();
/// assert_eq!(collected.len(), 2);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
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

    /// Every defined variant, including [`Signal::Pipe`] (read its
    /// documentation before installing it) and the Unix-only variants,
    /// which are skipped on Windows.
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

    /// Signals in `self`, `other`, or both.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self {
            bits: self.bits | other.bits,
        }
    }

    /// Signals in both `self` and `other`.
    #[must_use]
    pub const fn intersection(self, other: Self) -> Self {
        Self {
            bits: self.bits & other.bits,
        }
    }

    /// Signals in `self` but not in `other`.
    #[must_use]
    pub const fn difference(self, other: Self) -> Self {
        Self {
            bits: self.bits & !other.bits,
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

    /// The subset of `self` the current platform can deliver (see
    /// [`Signal::available_on_current_platform`]).
    #[must_use]
    pub const fn available_on_current_platform(self) -> Self {
        if cfg!(unix) {
            self
        } else {
            self.without(Signal::Pipe)
                .without(Signal::User1)
                .without(Signal::User2)
        }
    }

    /// Iterate the enabled signals in canonical [`Signal::ALL`] order.
    #[must_use]
    pub const fn iter(&self) -> SignalSetIter {
        SignalSetIter {
            bits: self.bits,
            index: 0,
        }
    }
}

impl Default for SignalSet {
    /// [`SignalSet::graceful`].
    fn default() -> Self {
        Self::graceful()
    }
}

impl fmt::Debug for SignalSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SignalSet ")?;
        f.debug_set().entries(self.iter()).finish()
    }
}

impl From<Signal> for SignalSet {
    fn from(sig: Signal) -> Self {
        Self::empty().with(sig)
    }
}

impl BitOr for SignalSet {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOr<Signal> for SignalSet {
    type Output = Self;
    fn bitor(self, rhs: Signal) -> Self {
        self.with(rhs)
    }
}

impl BitOrAssign for SignalSet {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.union(rhs);
    }
}

impl BitOrAssign<Signal> for SignalSet {
    fn bitor_assign(&mut self, rhs: Signal) {
        *self = self.with(rhs);
    }
}

impl BitAnd for SignalSet {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        self.intersection(rhs)
    }
}

impl BitAndAssign for SignalSet {
    fn bitand_assign(&mut self, rhs: Self) {
        *self = self.intersection(rhs);
    }
}

impl Sub for SignalSet {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self {
        self.difference(rhs)
    }
}

impl Sub<Signal> for SignalSet {
    type Output = Self;
    fn sub(self, rhs: Signal) -> Self {
        self.without(rhs)
    }
}

impl SubAssign for SignalSet {
    fn sub_assign(&mut self, rhs: Self) {
        *self = self.difference(rhs);
    }
}

impl SubAssign<Signal> for SignalSet {
    fn sub_assign(&mut self, rhs: Signal) {
        *self = self.without(rhs);
    }
}

impl FromIterator<Signal> for SignalSet {
    fn from_iter<I: IntoIterator<Item = Signal>>(iter: I) -> Self {
        iter.into_iter().fold(Self::empty(), Self::with)
    }
}

impl Extend<Signal> for SignalSet {
    fn extend<I: IntoIterator<Item = Signal>>(&mut self, iter: I) {
        for sig in iter {
            *self = self.with(sig);
        }
    }
}

impl IntoIterator for SignalSet {
    type Item = Signal;
    type IntoIter = SignalSetIter;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl IntoIterator for &SignalSet {
    type Item = Signal;
    type IntoIter = SignalSetIter;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Iterator over the signals enabled in a [`SignalSet`], in canonical
/// [`Signal::ALL`] order.
///
/// # Examples
///
/// ```
/// use signal_mod::{Signal, SignalSet};
///
/// let mut it = SignalSet::graceful().iter();
/// assert_eq!(it.len(), 3);
/// assert_eq!(it.next(), Some(Signal::Terminate));
/// assert_eq!(it.next(), Some(Signal::Interrupt));
/// assert_eq!(it.next(), Some(Signal::Hangup));
/// assert_eq!(it.next(), None);
/// ```
#[derive(Debug, Clone)]
pub struct SignalSetIter {
    bits: u16,
    index: usize,
}

impl Iterator for SignalSetIter {
    type Item = Signal;

    #[inline]
    fn next(&mut self) -> Option<Signal> {
        while self.index < Signal::ALL.len() {
            let sig = Signal::ALL[self.index];
            self.index += 1;
            if self.bits & sig.bit() != 0 {
                return Some(sig);
            }
        }
        None
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self
            .bits
            .checked_shr(u32::try_from(self.index).unwrap_or(u32::MAX));
        let n = remaining.unwrap_or(0).count_ones() as usize;
        (n, Some(n))
    }
}

impl ExactSizeIterator for SignalSetIter {}

impl FusedIterator for SignalSetIter {}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[test]
    fn test_signal_display_matches_description() {
        for s in Signal::ALL {
            assert_eq!(format!("{s}"), s.description());
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_signal_unix_number_matches_libc_on_this_target() {
        assert_eq!(Signal::Terminate.unix_number(), Some(libc::SIGTERM));
        assert_eq!(Signal::Interrupt.unix_number(), Some(libc::SIGINT));
        assert_eq!(Signal::Hangup.unix_number(), Some(libc::SIGHUP));
        assert_eq!(Signal::Quit.unix_number(), Some(libc::SIGQUIT));
        assert_eq!(Signal::Pipe.unix_number(), Some(libc::SIGPIPE));
        assert_eq!(Signal::User1.unix_number(), Some(libc::SIGUSR1));
        assert_eq!(Signal::User2.unix_number(), Some(libc::SIGUSR2));
    }

    // SM-001 regression: 1.0.0 returned 10 / 12 everywhere, which are
    // SIGBUS / SIGSYS on these systems.
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    #[test]
    fn test_signal_unix_number_on_apple_and_bsd_uses_30_and_31_for_user_signals() {
        assert_eq!(Signal::User1.unix_number(), Some(30));
        assert_eq!(Signal::User2.unix_number(), Some(31));
        assert_eq!(Signal::from_unix_number(10), None);
        assert_eq!(Signal::from_unix_number(12), None);
    }

    #[cfg(all(
        target_os = "linux",
        any(
            target_arch = "x86_64",
            target_arch = "x86",
            target_arch = "aarch64",
            target_arch = "arm",
            target_arch = "riscv64"
        )
    ))]
    #[test]
    fn test_signal_unix_number_on_common_linux_uses_10_and_12_for_user_signals() {
        assert_eq!(Signal::User1.unix_number(), Some(10));
        assert_eq!(Signal::User2.unix_number(), Some(12));
    }

    #[cfg(not(unix))]
    #[test]
    fn test_signal_unix_number_off_unix_uses_posix_numbers_and_none_for_user_signals() {
        assert_eq!(Signal::Hangup.unix_number(), Some(1));
        assert_eq!(Signal::Interrupt.unix_number(), Some(2));
        assert_eq!(Signal::Quit.unix_number(), Some(3));
        assert_eq!(Signal::Pipe.unix_number(), Some(13));
        assert_eq!(Signal::Terminate.unix_number(), Some(15));
        assert_eq!(Signal::User1.unix_number(), None);
        assert_eq!(Signal::User2.unix_number(), None);
        assert_eq!(Signal::User1.exit_code(), 1);
    }

    #[test]
    fn test_signal_from_unix_number_round_trips_every_numbered_variant() {
        for sig in Signal::ALL {
            if let Some(n) = sig.unix_number() {
                assert_eq!(Signal::from_unix_number(n), Some(sig));
            }
        }
        assert_eq!(Signal::from_unix_number(0), None);
        assert_eq!(Signal::from_unix_number(i32::MAX), None);
    }

    #[test]
    fn test_signal_exit_code_is_128_plus_number() {
        assert_eq!(Signal::Interrupt.exit_code(), 130);
        assert_eq!(Signal::Terminate.exit_code(), 143);
        assert_eq!(Signal::Hangup.exit_code(), 129);
        assert_eq!(Signal::Quit.exit_code(), 131);
    }

    #[test]
    fn test_signal_is_unix_only_flags_pipe_and_user_signals() {
        assert!(Signal::Pipe.is_unix_only());
        assert!(Signal::User1.is_unix_only());
        assert!(Signal::User2.is_unix_only());
        assert!(!Signal::Terminate.is_unix_only());
        assert!(!Signal::Interrupt.is_unix_only());
        assert!(!Signal::Quit.is_unix_only());
        assert!(!Signal::Hangup.is_unix_only());
    }

    #[test]
    fn test_set_empty_and_all_have_expected_len() {
        assert!(SignalSet::empty().is_empty());
        assert_eq!(SignalSet::empty().len(), 0);
        assert_eq!(SignalSet::all().len(), 7);
    }

    #[test]
    fn test_set_graceful_contains_term_int_hup() {
        let g = SignalSet::graceful();
        assert!(g.contains(Signal::Terminate));
        assert!(g.contains(Signal::Interrupt));
        assert!(g.contains(Signal::Hangup));
        assert!(!g.contains(Signal::Quit));
        assert!(!g.contains(Signal::Pipe));
        assert_eq!(g.len(), 3);
    }

    #[test]
    fn test_set_standard_adds_quit() {
        let s = SignalSet::standard();
        assert!(s.contains(Signal::Quit));
        assert_eq!(s.len(), 4);
    }

    #[test]
    fn test_set_with_without_are_idempotent() {
        let s = SignalSet::empty()
            .with(Signal::Terminate)
            .with(Signal::Terminate);
        assert_eq!(s.len(), 1);
        let s = s.without(Signal::Terminate).without(Signal::Terminate);
        assert!(s.is_empty());
    }

    #[test]
    fn test_set_iter_yields_canonical_order_with_exact_size() {
        let s = SignalSet::all();
        let it = s.iter();
        assert_eq!(it.len(), 7);
        let v: Vec<Signal> = it.collect();
        assert_eq!(v, Signal::ALL.to_vec());
        let mut it = SignalSet::empty().iter();
        assert_eq!(it.next(), None);
        assert_eq!(it.next(), None);
    }

    #[test]
    fn test_set_default_is_graceful() {
        assert_eq!(SignalSet::default(), SignalSet::graceful());
    }

    #[test]
    fn test_set_operators_match_named_methods() {
        let a = SignalSet::graceful();
        let b = SignalSet::empty().with(Signal::Hangup).with(Signal::User1);
        assert_eq!(a | b, a.union(b));
        assert_eq!(a & b, a.intersection(b));
        assert_eq!(a - b, a.difference(b));
        assert_eq!(a | Signal::Quit, a.with(Signal::Quit));
        assert_eq!(a - Signal::Hangup, a.without(Signal::Hangup));

        let mut c = a;
        c |= b;
        assert_eq!(c, a | b);
        c &= b;
        assert_eq!(c, b);
        c -= Signal::User1;
        assert_eq!(c, SignalSet::from(Signal::Hangup));
        c |= Signal::Quit;
        c -= SignalSet::from(Signal::Quit);
        assert_eq!(c, SignalSet::from(Signal::Hangup));
    }

    #[test]
    fn test_set_from_iterator_and_extend_collect_signals() {
        let set: SignalSet = [Signal::Quit, Signal::Quit, Signal::User2]
            .into_iter()
            .collect();
        assert_eq!(set.len(), 2);
        let mut ext = SignalSet::empty();
        ext.extend(set);
        assert_eq!(ext, set);
        let by_ref: Vec<Signal> = (&set).into_iter().collect();
        assert_eq!(by_ref, vec![Signal::Quit, Signal::User2]);
    }

    #[test]
    fn test_set_debug_lists_signals() {
        let s = format!("{:?}", SignalSet::empty().with(Signal::Quit));
        assert_eq!(s, "SignalSet {Quit}");
    }

    #[test]
    fn test_set_available_on_current_platform_drops_unix_only_off_unix() {
        let avail = SignalSet::all().available_on_current_platform();
        if cfg!(unix) {
            assert_eq!(avail, SignalSet::all());
        } else {
            assert_eq!(avail, SignalSet::standard());
        }
    }
}
