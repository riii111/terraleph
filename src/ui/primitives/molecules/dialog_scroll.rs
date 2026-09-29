use std::cell::Cell;

use crossterm::event::KeyCode;

const COLUMN_STEP: u16 = 4;

// The limit comes from the last render because only the dialog layout knows its wrapped height.
// Relative moves start from the clamped offset, so End (u16::MAX) is followed by visible movement.
// Before a render reports a limit, moves are not clamped.
// Columns follow the same rule; dialogs that wrap never report a column limit above zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DialogScroll {
    offset: u16,
    max: Cell<Option<u16>>,
    column: u16,
    max_column: Cell<Option<u16>>,
}

impl DialogScroll {
    /// Applies the scroll keys every help, context, and message dialog shares and ignores the rest.
    pub(crate) fn handle_key(&mut self, code: KeyCode, page: i16) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
            KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::PageDown => self.scroll_by(page),
            KeyCode::Left | KeyCode::Char('h') => self.scroll_left(),
            KeyCode::Right | KeyCode::Char('l') => self.scroll_right(),
            KeyCode::Home => self.top(),
            KeyCode::End => self.bottom(),
            _ => {}
        }
    }

    pub(crate) fn scroll_by(&mut self, delta: i16) {
        let current = self.clamped(self.offset);
        let next = if delta.is_negative() {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current.saturating_add(delta.cast_unsigned())
        };
        self.offset = self.clamped(next);
    }

    pub(crate) fn scroll_left(&mut self) {
        self.column = self.clamped_column(self.column).saturating_sub(COLUMN_STEP);
    }

    pub(crate) fn scroll_right(&mut self) {
        let next = self.clamped_column(self.column).saturating_add(COLUMN_STEP);
        self.column = self.clamped_column(next);
    }

    pub(crate) const fn top(&mut self) {
        self.offset = 0;
    }

    pub(crate) const fn bottom(&mut self) {
        self.offset = u16::MAX;
    }

    pub(crate) fn reset(&mut self) {
        self.offset = 0;
        self.max.set(None);
        self.column = 0;
        self.max_column.set(None);
    }

    pub(crate) fn clamp_for_render(&self, max: u16) -> u16 {
        self.max.set(Some(max));
        self.offset.min(max)
    }

    pub(crate) fn clamp_column_for_render(&self, max: u16) -> u16 {
        self.max_column.set(Some(max));
        self.column.min(max)
    }

    fn clamped(&self, offset: u16) -> u16 {
        self.max.get().map_or(offset, |max| offset.min(max))
    }

    fn clamped_column(&self, column: u16) -> u16 {
        self.max_column.get().map_or(column, |max| column.min(max))
    }
}

#[cfg(test)]
mod test_support {
    use super::DialogScroll;

    // Key-mapping tests observe the raw offset without a render; renders read it only
    // through the clamped value.
    impl DialogScroll {
        pub(crate) const fn offset_for_test(&self) -> u16 {
            self.offset
        }

        pub(crate) const fn column_for_test(&self) -> u16 {
            self.column
        }
    }
}

#[cfg(test)]
mod tests {
    use super::DialogScroll;

    #[test]
    fn moves_start_from_the_rendered_limit_and_stop_at_it() {
        let mut scroll = DialogScroll::default();
        scroll.bottom();
        assert_eq!(scroll.clamp_for_render(10), 10);

        scroll.scroll_by(-1);
        assert_eq!(scroll.offset, 9);

        scroll.scroll_by(8);
        assert_eq!(scroll.offset, 10);
    }

    #[test]
    fn reset_forgets_the_limit_of_the_previous_dialog() {
        let mut scroll = DialogScroll::default();
        scroll.clamp_for_render(3);

        scroll.reset();
        scroll.scroll_by(20);

        assert_eq!(scroll.offset, 20);
    }

    #[test]
    fn columns_stop_at_the_rendered_limit_and_reset_with_the_dialog() {
        let mut scroll = DialogScroll::default();
        assert_eq!(scroll.clamp_column_for_render(5), 0);

        scroll.scroll_right();
        scroll.scroll_right();
        assert_eq!(scroll.clamp_column_for_render(5), 5);

        scroll.scroll_left();
        assert_eq!(scroll.clamp_column_for_render(5), 1);

        scroll.reset();
        assert_eq!(scroll.clamp_column_for_render(5), 0);
    }
}
