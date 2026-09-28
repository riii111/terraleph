use std::{
    io,
    mem::ManuallyDrop,
    sync::mpsc::{self, Receiver, RecvTimeoutError},
    thread,
    time::Duration,
};

use crossterm::event::{self, Event};
use ratatui::DefaultTerminal;

// ratatui::run and Terminal's drop report restore failures with eprintln!, which panics once the
// terminal is closed; the panic hook then panics again and aborts before saved plans are removed.
pub(super) fn run<R>(session: impl FnOnce(&mut DefaultTerminal) -> io::Result<R>) -> io::Result<R> {
    let mut active = ActiveTerminal {
        terminal: ManuallyDrop::new(ratatui::init()),
    };
    session(&mut active.terminal)
}

struct ActiveTerminal {
    terminal: ManuallyDrop<DefaultTerminal>,
}

impl Drop for ActiveTerminal {
    fn drop(&mut self) {
        match ratatui::try_restore() {
            // SAFETY: the terminal is not used after the session ends and is dropped only here.
            Ok(()) => unsafe { ManuallyDrop::drop(&mut self.terminal) },
            Err(error) => {
                super::report_error(&format!("failed to restore the terminal: {error}"));
            }
        }
    }
}

// crossterm keeps reading a hung-up terminal that only returns EOF, so a closed window would
// trap any thread that polls it. Reading on a detached thread keeps the event loop free to
// notice termination signals, stop the workers, and remove saved plans. The thread is never
// joined; it ends with the process.
pub(super) struct TerminalInput {
    events: Receiver<io::Result<Event>>,
}

impl TerminalInput {
    pub(super) fn spawn() -> io::Result<Self> {
        let (sender, events) = mpsc::channel();
        thread::Builder::new()
            .name("terraleph-input".to_owned())
            .spawn(move || {
                loop {
                    let event = event::read();
                    let failed = event.is_err();
                    if sender.send(event).is_err() || failed {
                        break;
                    }
                }
            })?;
        Ok(Self { events })
    }

    pub(super) fn next(&self, timeout: Duration) -> io::Result<Option<Event>> {
        match self.events.recv_timeout(timeout) {
            Ok(event) => event.map(Some),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::other("terminal input stopped")),
        }
    }
}
