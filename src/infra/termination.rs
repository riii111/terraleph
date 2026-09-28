use std::{
    io,
    sync::atomic::{AtomicI32, Ordering},
};

// Zero means no request. Only the first signal is kept so the exit status names the cause that
// started the shutdown, not a repeat delivered while workers were still stopping.
static RECEIVED: AtomicI32 = AtomicI32::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TerminationSignal(i32);

impl TerminationSignal {
    #[must_use]
    pub(crate) fn exit_code(self) -> u8 {
        u8::try_from(128 + self.0).unwrap_or(u8::MAX)
    }

    #[must_use]
    pub(crate) const fn name(self) -> &'static str {
        match self.0 {
            1 => "SIGHUP",
            2 => "SIGINT",
            15 => "SIGTERM",
            _ => "a termination signal",
        }
    }
}

// Terminal hangup, `kill`, and a SIGINT that arrives outside raw mode would otherwise end the
// process without unwinding, leaving saved plans behind. The handler only records the signal;
// the event loops observe it and shut down through the same path as a cancellation.
#[cfg(unix)]
pub(crate) fn install() -> io::Result<()> {
    for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
        install_recorder(signal)?;
    }
    Ok(())
}

// Console close events on Windows are not handled yet; this keeps the caller portable.
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
