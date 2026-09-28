//! Text as the terminal should show it. ratatui skips control characters when it draws, so a tab
//! in a heredoc or a carriage return from a Windows file would vanish and pull the text after it
//! left. Shown text expands tabs to the next tab stop and spells other control characters out in
//! caret notation; copying and filtering keep the original text.

use std::borrow::Cow;

use ratatui::{buffer::CellWidth, style::Style, text::Span};

/// Columns between tab stops, as terminals set them by default.
const TAB_WIDTH: usize = 8;
const TAB_STOP: &str = "        ";
const _: () = assert!(TAB_STOP.len() == TAB_WIDTH);

/// The drawn column of a line shown piece by piece, so a tab in any piece stops where it would in
/// the whole line.
#[derive(Default)]
pub(crate) struct DisplayColumns {
    column: usize,
}

impl DisplayColumns {
    pub(crate) const fn column(&self) -> usize {
        self.column
    }

    /// Returns `text` as shown from the current column and moves past it.
    pub(crate) fn show<'a>(&mut self, text: &'a str) -> Cow<'a, str> {
        if is_printable_ascii(text) {
            self.column += text.len();
            return Cow::Borrowed(text);
        }
        if !text.contains(char::is_control) {
            self.column += cells(text);
            return Cow::Borrowed(text);
        }
        let mut shown = String::with_capacity(text.len() + TAB_WIDTH);
        let mut rest = text;
        loop {
            let (plain, tail) = rest.split_at(rest.find(char::is_control).unwrap_or(rest.len()));
            self.column += cells(plain);
            shown.push_str(plain);
            let mut tail = tail.chars();
            let Some(control) = tail.next() else {
                break;
            };
            rest = tail.as_str();
            let spelled = if control == '\t' {
                Cow::Borrowed(&TAB_STOP[..TAB_WIDTH - self.column % TAB_WIDTH])
            } else {
                caret_notation(control)
            };
            // Both spellings are ASCII, so each byte takes one cell.
            self.column += spelled.len();
            shown.push_str(&spelled);
        }
        Cow::Owned(shown)
    }
}

/// Printable ASCII takes one cell per byte and is shown as it is.
pub(crate) fn is_printable_ascii(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_graphic() || byte == b' ')
}

// C0 controls and DEL use the caret notation of `cat -v` and less; C1 controls have none, so they
// show their code point.
fn caret_notation(control: char) -> Cow<'static, str> {
    match u8::try_from(control) {
        Ok(byte @ 0..=0x1f) => Cow::Owned(format!("^{}", char::from(byte + b'@'))),
        Ok(0x7f) => Cow::Borrowed("^?"),
        _ => Cow::Owned(format!("<U+{:04X}>", u32::from(control))),
    }
}

// Control characters split graphemes, so text between them has the graphemes ratatui draws.
fn cells(text: &str) -> usize {
    if is_printable_ascii(text) {
        return text.len();
    }
    Span::raw(text)
        .styled_graphemes(Style::default())
        .map(|grapheme| usize::from(grapheme.symbol.cell_width()))
        .sum()
}

#[cfg(test)]
mod tests {
    use ratatui::{Terminal, backend::TestBackend, text::Line, widgets::Paragraph};

    use super::*;

    fn shown(text: &str) -> (String, usize) {
        let mut columns = DisplayColumns::default();
        let shown = columns.show(text).into_owned();
        (shown, columns.column())
    }

    // The first cell after the drawn text, found by drawing a marker right behind it.
    fn drawn_cells(text: &str) -> usize {
        let mut terminal = Terminal::new(TestBackend::new(40, 1)).expect("test terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new(Line::from(vec![Span::raw(text), Span::raw("|")])),
                    frame.area(),
                );
            })
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..40)
            .position(|x| buffer[(x, 0)].symbol() == "|")
            .expect("marker should be drawn")
    }

    #[test]
    fn tabs_expand_to_the_next_stop_after_the_cells_before_them() {
        for (text, expected) in [
            ("\tx", "        x"),
            ("a\tb", "a       b"),
            ("1234567\tx", "1234567 x"),
            ("12345678\tx", "12345678        x"),
            ("\t\tx", "                x"),
            // Wide and zero-width graphemes move the stop by the cells they take.
            ("あ\tx", "あ      x"),
            ("e\u{301}\tx", "e\u{301}       x"),
            ("ｶﾞ\tx", "ｶﾞ      x"),
        ] {
            let (text_shown, column) = shown(text);
            assert_eq!(text_shown, expected, "{text:?}");
            assert_eq!(column, drawn_cells(&text_shown), "{text:?}");
        }
    }

    #[test]
    fn other_control_characters_are_spelled_out() {
        for (text, expected) in [
            ("crlf\r", "crlf^M"),
            ("\u{1b}[31mred\u{1b}[0m", "^[[31mred^[[0m"),
            ("nul\0", "nul^@"),
            ("bell\u{7}", "bell^G"),
            ("del\u{7f}", "del^?"),
            ("next\u{85}line", "next<U+0085>line"),
            ("^M", "^M"),
        ] {
            let (text_shown, column) = shown(text);
            assert_eq!(text_shown, expected, "{text:?}");
            assert_eq!(column, drawn_cells(&text_shown), "{text:?}");
        }
    }

    #[test]
    fn pieces_of_one_line_keep_its_tab_stops() {
        let mut columns = DisplayColumns::default();
        let pieces = ["ab", "c\td", "\t", "e\r"]
            .map(|piece| columns.show(piece).into_owned())
            .concat();

        assert_eq!(pieces, shown("abc\td\te\r").0);
        assert_eq!(columns.column(), 19);
    }

    #[test]
    fn text_without_control_characters_is_borrowed_and_counted_as_drawn() {
        for text in ["", "plain", "ｶﾞｷﾟ全角", "e\u{301}"] {
            let mut columns = DisplayColumns::default();
            assert!(matches!(columns.show(text), Cow::Borrowed(borrowed) if borrowed == text));
            assert_eq!(columns.column(), drawn_cells(text), "{text:?}");
        }
    }
}
