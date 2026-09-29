use ratatui::{style::Style, text::Line};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct TextInput {
    text: String,
    cursor: usize,
}

impl TextInput {
    pub(crate) const fn with_cursor_at_end(text: String) -> Self {
        let cursor = text.len();
        Self { text, cursor }
    }

    pub(crate) const fn text(&self) -> &str {
        self.text.as_str()
    }

    pub(crate) const fn cursor(&self) -> usize {
        self.cursor
    }

    pub(crate) fn into_text(self) -> String {
        self.text
    }

    pub(crate) fn insert(&mut self, character: char) {
        self.text.insert(self.cursor, character);
        self.cursor =
            next_grapheme_boundary_at_or_after(&self.text, self.cursor + character.len_utf8());
    }

    /// Returns whether a grapheme was removed.
    pub(crate) fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let previous = previous_grapheme_boundary(&self.text, self.cursor);
        self.text.drain(previous..self.cursor);
        self.cursor = previous;
        true
    }

    pub(crate) fn move_left(&mut self) {
        self.cursor = previous_grapheme_boundary(&self.text, self.cursor);
    }

    pub(crate) fn move_right(&mut self) {
        self.cursor = next_grapheme_boundary(&self.text, self.cursor);
    }

    pub(crate) const fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub(crate) const fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    pub(crate) fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
}

fn previous_grapheme_boundary(text: &str, cursor: usize) -> usize {
    grapheme_boundaries(text)
        .into_iter()
        .rev()
        .find(|&boundary| boundary < cursor)
        .unwrap_or(0)
}

fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    grapheme_boundaries(text)
        .into_iter()
        .find(|&boundary| boundary > cursor)
        .unwrap_or(text.len())
}

fn next_grapheme_boundary_at_or_after(text: &str, cursor: usize) -> usize {
    grapheme_boundaries(text)
        .into_iter()
        .find(|&boundary| boundary >= cursor)
        .unwrap_or(text.len())
}

fn grapheme_boundaries(text: &str) -> Vec<usize> {
    let mut boundaries = vec![0];
    let mut offset = 0;
    for grapheme in Line::from(text).styled_graphemes(Style::default()) {
        offset += grapheme.symbol.len();
        boundaries.push(offset);
    }
    if boundaries.last().copied() != Some(text.len()) {
        boundaries.push(text.len());
    }
    boundaries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backspace_at_the_start_removes_nothing() {
        let mut input = TextInput::with_cursor_at_end("a".to_owned());
        input.move_home();

        assert!(!input.backspace());

        assert_eq!(input.text(), "a");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn edits_and_moves_follow_grapheme_boundaries() {
        let mut input = TextInput::with_cursor_at_end("aあe\u{301}👩💻".to_owned());

        input.move_left();
        input.insert('\u{200d}');
        input.move_left();
        input.move_left();
        assert!(input.backspace());

        assert_eq!(input.text(), "ae\u{301}👩\u{200d}💻");
        assert_eq!(input.cursor(), "a".len());

        input.move_right();
        input.move_right();
        assert_eq!(input.cursor(), input.text().len());
    }

    #[test]
    fn clear_empties_the_text_and_resets_the_cursor() {
        let mut input = TextInput::with_cursor_at_end("abc".to_owned());

        input.clear();

        assert_eq!(input, TextInput::default());
    }
}
