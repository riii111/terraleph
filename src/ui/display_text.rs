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
            let spelled = spelling(control, self.column);
            self.column += spelled.len();
            shown.push_str(&spelled);
        }
        Cow::Owned(shown)
    }
}

// The graphemes ratatui draws for the shown text of a line, read from the line only as far as they
// are taken. Graphemes away from control characters are borrowed from the line.
struct ShownGraphemes<'a> {
    rest: &'a str,
    column: usize,
    // False while every grapheme taken so far is the line's own text.
    spelled: bool,
    after_control: bool,
    // A spelling can join the grapheme next to its control character, such as `^M` followed by a
    // combining accent, so text around control characters is segmented as shown. Its first `ready`
    // bytes are whole graphemes; the rest may still grow.
    pending: String,
    ready: usize,
}

impl<'a> ShownGraphemes<'a> {
    const fn new(text: &'a str) -> Self {
        Self {
            rest: text,
            column: 0,
            spelled: false,
            after_control: false,
            pending: String::new(),
            ready: 0,
        }
    }

    fn take_ready(&mut self) -> Cow<'a, str> {
        let length = first_grapheme_len(&self.pending[..self.ready]);
        let grapheme = static_ascii(&self.pending[..length]).map_or_else(
            || Cow::Owned(self.pending[..length].to_owned()),
            Cow::Borrowed,
        );
        self.pending.drain(..length);
        self.ready -= length;
        grapheme
    }

    fn mark_ready_before_last_grapheme(&mut self) {
        self.ready = Span::raw(self.pending.as_str())
            .styled_graphemes(Style::default())
            .last()
            .map_or(0, |last| self.pending.len() - last.symbol.len());
    }
}

impl<'a> Iterator for ShownGraphemes<'a> {
    type Item = Cow<'a, str>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.ready > 0 {
                return Some(self.take_ready());
            }
            let Some(first) = self.rest.chars().next() else {
                if self.pending.is_empty() {
                    return None;
                }
                self.ready = self.pending.len();
                continue;
            };
            if first.is_control() {
                self.rest = &self.rest[first.len_utf8()..];
                let spelled = spelling(first, self.column);
                self.column += spelled.len();
                self.pending.push_str(&spelled);
                self.spelled = true;
                self.after_control = true;
                self.mark_ready_before_last_grapheme();
                continue;
            }
            // Two graphemes of the line with no control character between them stay apart when
            // shown.
            if !self.after_control && !self.pending.is_empty() {
                self.ready = self.pending.len();
                continue;
            }
            let (grapheme, tail) = self.rest.split_at(first_grapheme_len(self.rest));
            self.rest = tail;
            self.column += usize::from(grapheme.cell_width());
            if self.after_control || tail.starts_with(char::is_control) {
                self.after_control = false;
                self.pending.push_str(grapheme);
                self.mark_ready_before_last_grapheme();
                continue;
            }
            return Some(Cow::Borrowed(grapheme));
        }
    }
}

/// The shown part of `text` drawn in a row `width` cells wide, starting `offset` cells into the
/// line. A wide character cut by the left edge leaves its remaining cells blank, so every line moves
/// by exactly `offset` and the end of the widest line can reach the right edge. `Paragraph::scroll`
/// would draw such a character whole instead. This keeps only what `Paragraph` draws within
/// `width`, so a long line costs the cells up to the right edge rather than its length, and the row
/// is drawn exactly as the whole shown remainder would be.
pub(crate) fn visible_cells(text: &str, offset: usize, width: usize) -> Cow<'_, str> {
    if width == 0 {
        return Cow::Borrowed("");
    }
    if offset == 0 {
        let mut graphemes = ShownGraphemes::new(text);
        keep_visible(&mut graphemes, 0, width, |_| {});
        // `Paragraph` stops at the right edge on its own.
        if !graphemes.spelled {
            return Cow::Borrowed(text);
        }
    }
    let mut kept = String::new();
    keep_visible(&mut ShownGraphemes::new(text), offset, width, |grapheme| {
        kept.push_str(grapheme);
    });
    Cow::Owned(kept)
}

fn keep_visible(
    graphemes: &mut ShownGraphemes<'_>,
    offset: usize,
    width: usize,
    mut keep: impl FnMut(&str),
) {
    let mut skipped = 0;
    let mut kept_width = 0;
    for grapheme in graphemes {
        let cells = usize::from(grapheme.cell_width());
        if skipped < offset {
            skipped += cells;
            let blank = skipped.saturating_sub(offset);
            (0..blank).for_each(|_| keep(" "));
            kept_width += blank;
            continue;
        }
        // `Paragraph` leaves out a grapheme wider than the row and stops at the first one that
        // does not fit, so a wide character straddling the right edge is not drawn.
        if cells > width {
            continue;
        }
        if kept_width + cells > width {
            break;
        }
        kept_width += cells;
        keep(&grapheme);
    }
}

pub(crate) fn shown_width(text: &str) -> usize {
    let mut columns = DisplayColumns::default();
    columns.show(text);
    columns.column()
}

pub(crate) fn is_printable_ascii(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_graphic() || byte == b' ')
}

// Both spellings are ASCII, so each byte takes one cell.
fn spelling(control: char, column: usize) -> Cow<'static, str> {
    if control == '\t' {
        Cow::Borrowed(&TAB_STOP[..TAB_WIDTH - column % TAB_WIDTH])
    } else {
        caret_notation(control)
    }
}

// `text` does not start with a control character, which ratatui would leave out.
fn first_grapheme_len(text: &str) -> usize {
    Span::raw(text)
        .styled_graphemes(Style::default())
        .next()
        .map_or(text.len(), |grapheme| grapheme.symbol.len())
}

// Spellings are mostly printable ASCII, so most of their graphemes need no allocation.
fn static_ascii(grapheme: &str) -> Option<&'static str> {
    const PRINTABLE: &str = " !\"#$%&'()*+,-./0123456789:;<=>?@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~";
    const _: () = assert!(PRINTABLE.len() == 95);
    match grapheme.as_bytes() {
        [byte @ b' '..=b'~'] => {
            let index = usize::from(byte - b' ');
            Some(&PRINTABLE[index..=index])
        }
        _ => None,
    }
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
    use rstest::rstest;

    use super::*;

    fn shown(text: &str) -> (String, usize) {
        let mut columns = DisplayColumns::default();
        let shown = columns.show(text).into_owned();
        (shown, columns.column())
    }

    fn drawn_graphemes(shown: &str) -> Vec<String> {
        Line::from(shown)
            .styled_graphemes(Style::default())
            .map(|grapheme| grapheme.symbol.to_owned())
            .collect()
    }

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

    #[rstest]
    #[case::line_start("\tx", "        x")]
    #[case::after_one_cell("a\tb", "a       b")]
    #[case::one_cell_before_the_stop("1234567\tx", "1234567 x")]
    #[case::at_the_stop("12345678\tx", "12345678        x")]
    #[case::two_tabs("\t\tx", "                x")]
    #[case::after_a_wide_character("あ\tx", "あ      x")]
    #[case::after_a_combining_accent("e\u{301}\tx", "e\u{301}       x")]
    #[case::after_a_voiced_halfwidth_kana("ｶﾞ\tx", "ｶﾞ      x")]
    fn tabs_expand_to_the_next_stop_after_the_cells_before_them(
        #[case] text: &str,
        #[case] expected: &str,
    ) {
        let (text_shown, column) = shown(text);
        assert_eq!(text_shown, expected);
        assert_eq!(column, drawn_cells(&text_shown));
    }

    #[rstest]
    #[case::carriage_return("crlf\r", "crlf^M")]
    #[case::escape("\u{1b}[31mred\u{1b}[0m", "^[[31mred^[[0m")]
    #[case::nul("nul\0", "nul^@")]
    #[case::bell("bell\u{7}", "bell^G")]
    #[case::delete("del\u{7f}", "del^?")]
    #[case::c1_control("next\u{85}line", "next<U+0085>line")]
    #[case::caret_text("^M", "^M")]
    fn other_control_characters_are_spelled_out(#[case] text: &str, #[case] expected: &str) {
        let (text_shown, column) = shown(text);
        assert_eq!(text_shown, expected);
        assert_eq!(column, drawn_cells(&text_shown));
    }

    // Control characters split graphemes in the line, but their spellings can join the graphemes
    // next to them.
    #[rstest]
    #[case::empty("")]
    #[case::plain("plain ｶﾞ全角 e\u{301}")]
    #[case::tabs_between_graphemes("a\tb\tｶﾞ\t\tc")]
    #[case::escapes("\u{1b}[31mred\u{1b}[0m")]
    #[case::carriage_return_and_line_feed("x\r\n")]
    #[case::accent_after_a_carriage_return("\r\u{301}x")]
    #[case::joiner_after_a_tab("\t\u{200d}x")]
    #[case::prepended_mark_before_a_tab("\u{600}\tx")]
    #[case::flags_between_tabs("🇯🇵\t🇺🇸\t🇯")]
    fn shown_graphemes_are_the_graphemes_drawn_for_the_shown_line(#[case] text: &str) {
        let graphemes = ShownGraphemes::new(text).collect::<Vec<_>>();
        let (text_shown, _) = shown(text);

        assert_eq!(graphemes, drawn_graphemes(&text_shown));
    }

    #[test]
    fn graphemes_away_from_control_characters_are_borrowed() {
        let graphemes = ShownGraphemes::new("ab\tcd").collect::<Vec<_>>();

        assert!(matches!(graphemes[0], Cow::Borrowed("a")));
        assert!(matches!(graphemes[graphemes.len() - 1], Cow::Borrowed("d")));
    }

    #[test]
    fn a_visible_window_reads_a_long_line_only_to_its_right_edge() {
        let wide = "ｶﾞ".repeat(50);
        let line = format!("{wide}\t{}", "\u{1b}y\t".repeat(200_000));
        let mut graphemes = ShownGraphemes::new(&line);
        let mut kept = String::new();
        keep_visible(&mut graphemes, 99, 20, |grapheme| kept.push_str(grapheme));

        assert_eq!(kept, "     ^[y     ^[y    ");
        let read = line.len() - graphemes.rest.len();
        assert!(read < wide.len() + 100, "read {read} bytes");
    }

    #[rstest]
    #[case::plain_text("plain", 0, 3, "plain", true)]
    #[case::control_characters_past_the_right_edge("abcde\tf", 0, 3, "abcde\tf", true)]
    #[case::control_characters_in_the_window("a\tb\r", 0, 20, "a       b^M", false)]
    #[case::scrolled("abc", 1, 5, "bc", false)]
    #[case::wide_character_cut_by_the_left_edge("ｶﾞ\tx", 1, 20, "       x", false)]
    #[case::no_columns("a\tb", 0, 0, "", true)]
    fn visible_cells_borrow_a_line_drawn_as_it_is_from_the_left_edge(
        #[case] text: &str,
        #[case] offset: usize,
        #[case] width: usize,
        #[case] expected: &str,
        #[case] borrowed: bool,
    ) {
        let visible = visible_cells(text, offset, width);

        assert_eq!(visible, expected);
        assert_eq!(matches!(visible, Cow::Borrowed(_)), borrowed);
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
