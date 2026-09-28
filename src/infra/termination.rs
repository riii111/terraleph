use std::{
    io,
    sync::atomic::{AtomicI32, Ordering},
};
#[cfg(unix)]
use std::{
    io::IsTerminal,
    os::fd::{AsRawFd, RawFd},
    sync::atomic::AtomicBool,
};

// Zero means no request. Only the first signal is kept so the exit status names the cause that
// started the shutdown, not a repeat delivered while workers were still stopping.
static RECEIVED: AtomicI32 = AtomicI32::new(0);

// A hung-up terminal that was never followed by SIGHUP (nohup, a shell that does not forward it,
// a wrapper that catches it). Negative so it cannot collide with a signal number; it exits like
// SIGHUP.
const TERMINAL_CLOSED: i32 = -1;

// Set only when standard input was a terminal at install time, so a pipe or file that reaches
// end of input is never mistaken for a closed terminal.
#[cfg(unix)]
static WATCH_TERMINAL: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminationSignal(i32);

impl TerminationSignal {
    #[must_use]
    pub(crate) fn exit_code(self) -> u8 {
        u8::try_from(128 + self.0.abs()).unwrap_or(u8::MAX)
    }

    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self.0 {
            1 => "SIGHUP",
            2 => "SIGINT",
            15 => "SIGTERM",
            TERMINAL_CLOSED => "a closed terminal",
            _ => "a termination signal",
        }
    }
}

// Terminal hangup, `kill`, and a SIGINT that arrives outside raw mode would otherwise end the
// process without unwinding, leaving saved plans behind. The handler only records the signal;
// the event loops observe it and shut down through the same path as a cancellation.
#[cfg(unix)]
pub(crate) fn install() -> io::Result<()> {
    WATCH_TERMINAL.store(io::stdin().is_terminal(), Ordering::Relaxed);
    for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        install_recorder(signal)?;
    }
    Ok(())
}

// Console close events on Windows are not handled yet, and `requested` does not check the
// console either; this keeps the callers portable.
#[cfg(windows)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Unix implementation can fail and callers share one signature"
)]
pub(crate) const fn install() -> io::Result<()> {
    Ok(())
}

#[must_use]
pub(crate) fn received() -> Option<TerminationSignal> {
    match RECEIVED.load(Ordering::Relaxed) {
        0 => None,
        signal => Some(TerminationSignal(signal)),
    }
}

#[must_use]
pub(crate) fn requested() -> Option<TerminationSignal> {
    #[cfg(unix)]
    if RECEIVED.load(Ordering::Relaxed) == 0
        && WATCH_TERMINAL.load(Ordering::Relaxed)
        && hung_up(io::stdin().as_raw_fd())
    {
        let _ = RECEIVED.compare_exchange(0, TERMINAL_CLOSED, Ordering::Relaxed, Ordering::Relaxed);
    }
    received()
}

#[cfg(unix)]
fn hung_up(descriptor: RawFd) -> bool {
    // macOS reports a hung-up pty only when an event is requested. Pending input alone sets
    // POLLIN, which is not a hangup.
    let mut entry = libc::pollfd {
        fd: descriptor,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `entry` is one valid pollfd and a zero timeout never blocks.
    let ready = unsafe { libc::poll(&raw mut entry, 1, 0) };
    ready > 0 && entry.revents & (libc::POLLHUP | libc::POLLERR) != 0
}

#[cfg(unix)]
fn install_recorder(signal: libc::c_int) -> io::Result<()> {
    extern "C" fn record(signal: libc::c_int) {
        let _ = RECEIVED.compare_exchange(0, signal, Ordering::Relaxed, Ordering::Relaxed);
    }

    // SAFETY: a zeroed sigaction is a valid output buffer for querying the current disposition.
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    // SAFETY: a null new action only reads the current disposition into `previous`.
    if unsafe { libc::sigaction(signal, std::ptr::null(), &raw mut previous) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // A signal ignored by the parent (for example under nohup) stays ignored.
    if previous.sa_sigaction == libc::SIG_IGN {
        return Ok(());
    }
    // SAFETY: a zeroed sigaction has no flags; the mask is cleared below before use.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = record as *const () as libc::sighandler_t;
    action.sa_flags = libc::SA_RESTART;
    // SAFETY: the mask belongs to the local action and the handler only performs an atomic store.
    let result = unsafe {
        libc::sigemptyset(&raw mut action.sa_mask);
        libc::sigaction(signal, &raw const action, std::ptr::null_mut())
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn hangup_is_reported_only_after_the_terminal_closes() {
        let mut controller = -1;
        let mut device = -1;
        // SAFETY: both outputs are valid, and the optional name, termios, and size are null.
        let opened = unsafe {
            libc::openpty(
                &raw mut controller,
                &raw mut device,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(opened, 0, "{}", io::Error::last_os_error());
        assert!(!hung_up(device));

        // SAFETY: the controller was opened above and is closed exactly once.
        unsafe { libc::close(controller) };
        let closed = hung_up(device);
        // SAFETY: the device was opened above and is closed exactly once.
        unsafe { libc::close(device) };

        assert!(closed);
    }
}
