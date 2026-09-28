use std::io::{self, Write};

use arboard::Clipboard;

use crate::app::copy::{CopyEffect, CopyResult};

// Inside tmux, applications can set the clipboard with OSC 52 only under `set-clipboard on`; the
// default `external` ignores the sequence. Passthrough is not used, so tmux keeps a paste buffer.
const OSC52_PREFIX: &str = "\x1b]52;c;";
// BEL is accepted as the OSC terminator by more terminals than ST.
const OSC52_TERMINATOR: &str = "\x07";
// tmux drops, rather than truncates, any sequence that does not fit its default 1 MiB
// `input-buffer-size`, so a larger sequence could never reach the terminal through it.
const OSC52_MAX_SEQUENCE_LEN: usize = 1_000_000;
const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// The terminal fallback shares stdout with ratatui. Effects run on the event-loop thread between
// frames, and every frame is flushed before `Terminal::draw` returns, so a sequence written and
// flushed here in one call never lands inside a frame.
// On X11 the copied text is served by this process until it exits. Dropping the clipboard on the
// normal exit path lets arboard hand the text to a clipboard manager when one is running.
pub(crate) struct SystemClipboard<W: Write = io::Stdout> {
    clipboard: Option<Clipboard>,
    terminal: W,
}

impl SystemClipboard {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self {
            clipboard: Clipboard::new().ok(),
            terminal: io::stdout(),
        }
    }
}

impl<W: Write> SystemClipboard<W> {
    pub(crate) fn execute(&mut self, effect: &CopyEffect) -> CopyResult {
        if self.write(effect.text()) {
            CopyResult::Written
        } else if self.send_to_terminal(effect.text()).is_ok() {
            CopyResult::SentToTerminal
        } else {
            CopyResult::Failed
        }
    }

    fn write(&mut self, text: &str) -> bool {
        self.clipboard
            .as_mut()
            .is_some_and(|clipboard| clipboard.set_text(text).is_ok())
    }

    // Without a display, as over SSH, the terminal can still set its own clipboard with OSC 52.
    // Terminals do not acknowledge the sequence, so success only means it was written.
    fn send_to_terminal(&mut self, text: &str) -> io::Result<()> {
        let sequence = osc52_sequence(text).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "text is too large for the terminal clipboard",
            )
        })?;
        self.terminal.write_all(sequence.as_bytes())?;
        self.terminal.flush()
    }
}

fn osc52_sequence(text: &str) -> Option<String> {
    let length = OSC52_PREFIX.len() + text.len().div_ceil(3) * 4 + OSC52_TERMINATOR.len();
    if length > OSC52_MAX_SEQUENCE_LEN {
        return None;
    }
    let mut sequence = String::with_capacity(length);
    sequence.push_str(OSC52_PREFIX);
    push_base64(&mut sequence, text.as_bytes());
    sequence.push_str(OSC52_TERMINATOR);
    Some(sequence)
}

fn push_base64(output: &mut String, bytes: &[u8]) {
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0, |group, (index, byte)| {
            group | (usize::from(*byte) << (16 - 8 * index))
        });
        for index in 0..4 {
            if index <= chunk.len() {
                let digit = (group >> (18 - 6 * index)) & 0x3f;
                output.push(char::from(BASE64_ALPHABET[digit]));
            } else {
                output.push('=');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::copy::CopyTarget;

    use super::*;

    fn failing_clipboard() -> SystemClipboard<Vec<u8>> {
        SystemClipboard {
            clipboard: None,
            terminal: Vec::new(),
        }
    }

    #[test]
    fn unavailable_system_clipboard_sends_the_text_to_the_terminal() {
        let mut clipboard = failing_clipboard();
        let effect = CopyEffect::new(CopyTarget::Plan, "value = (sensitive value)\n".to_owned());

        let result = clipboard.execute(&effect);

        assert_eq!(result, CopyResult::SentToTerminal);
        assert_eq!(
            String::from_utf8(clipboard.terminal).expect("sequence should be UTF-8"),
            "\x1b]52;c;dmFsdWUgPSAoc2Vuc2l0aXZlIHZhbHVlKQo=\x07"
        );
    }

    #[test]
    fn text_too_large_for_the_terminal_is_not_sent() {
        let mut clipboard = failing_clipboard();
        let largest =
            (OSC52_MAX_SEQUENCE_LEN - OSC52_PREFIX.len() - OSC52_TERMINATOR.len()) / 4 * 3;

        assert_eq!(
            osc52_sequence(&"a".repeat(largest)).map(|sequence| sequence.len()),
            Some(OSC52_MAX_SEQUENCE_LEN)
        );
        let result = clipboard.execute(&CopyEffect::new(
            CopyTarget::Execution,
            "a".repeat(largest + 1),
        ));

        assert_eq!(result, CopyResult::Failed);
        assert!(clipboard.terminal.is_empty());
    }

    #[test]
    fn base64_pads_every_remainder_length() {
        for (text, expected) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("日本", "5pel5pys"),
            ("\u{ff}\u{7f}", "w79/"),
        ] {
            let mut output = String::new();
            push_base64(&mut output, text.as_bytes());
            assert_eq!(output, expected, "text: {text:?}");
        }
    }
}
