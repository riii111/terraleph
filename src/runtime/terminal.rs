use std::{
    io,
    mem::ManuallyDrop,
    panic,
    sync::{
        Once,
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread,
    time::Duration,
};

use crossterm::{
    event::{self, Event},
    terminal::{EnterAlternateScreen, enable_raw_mode},
};
use ratatui::{DefaultTerminal, Terminal, backend::CrosstermBackend};

// ratatui's init, restore, panic hook, and Terminal drop report failures with eprintln!, which
// panics once the terminal is closed; the hook then panics again and aborts before saved plans
// are removed. Every restore here tolerates a closed terminal instead.
pub(super) fn run<R>(session: impl FnOnce(&mut DefaultTerminal) -> io::Result<R>) -> io::Result<R> {
    let terminal = match init() {
        Ok(terminal) => terminal,
        Err(error) => {
            let _ = ratatui::try_restore();
            return Err(error);
        }
    };
    let mut active = ActiveTerminal {
        terminal: ManuallyDrop::new(terminal),
    };
    session(&mut active.terminal)
}

fn init() -> io::Result<DefaultTerminal> {
    install_restoring_panic_hook();
    enable_raw_mode()?;
    crossterm::execute!(io::stdout(), EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(io::stdout()))
}

fn install_restoring_panic_hook() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let _ = ratatui::try_restore();
            previous(info);
        }));
    });
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
// notice termination signals and the closed terminal, stop the workers, and remove saved plans.
// The thread is never joined; it ends with the process, and until then it spins on the EOF.
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
