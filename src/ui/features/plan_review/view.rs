use ratatui::layout::Rect;

use crate::ui::{
    features::overview::OverviewViewState, primitives::molecules::dialog_scroll::DialogScroll,
    text_input,
};

use super::PlanReviewInput;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanReviewOverlay {
    Help,
    Context,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlanReviewMatch {
    line: usize,
    start: usize,
    end: usize,
}

impl PlanReviewMatch {
    pub(crate) const fn new(line: usize, start: usize, end: usize) -> Self {
        Self { line, start, end }
    }

    pub(crate) const fn line(self) -> usize {
        self.line
    }

    pub(crate) const fn start(self) -> usize {
        self.start
    }

    pub(crate) const fn end(self) -> usize {
        self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchInputState {
    query: String,
    cursor: usize,
    previous_query: String,
    previous_vertical: usize,
    previous_horizontal: usize,
    previous_selected: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PlanReviewViewState {
    vertical: usize,
    horizontal: usize,
    search: Option<SearchInputState>,
    selected: Option<usize>,
    overlay: Option<PlanReviewOverlay>,
    overlay_scroll: DialogScroll,
    overview: OverviewViewState,
}

impl PlanReviewViewState {
    pub(crate) fn apply_with_matches(
        &mut self,
        input: PlanReviewInput,
        body: Rect,
        max_vertical: usize,
        max_horizontal: usize,
        current_query: &str,
        matches: &[PlanReviewMatch],
    ) -> Option<String> {
        self.clamp_scroll(max_vertical, max_horizontal);
        if self.search.is_some() {
            return self.apply_search_input(input, body, max_vertical, max_horizontal, matches);
        }

        match input {
            PlanReviewInput::SearchStart => {
                let query = current_query.to_owned();
                self.search = Some(SearchInputState {
                    cursor: text_input::last_grapheme_boundary(&query),
                    previous_query: query.clone(),
                    query,
                    previous_vertical: self.vertical,
                    previous_horizontal: self.horizontal,
                    previous_selected: self.selected,
                });
                self.selected = None;
                None
            }
            PlanReviewInput::Up => self.scroll_vertical(-1, max_vertical),
            PlanReviewInput::Down => self.scroll_vertical(1, max_vertical),
            PlanReviewInput::Left => self.scroll_horizontal(-1, max_horizontal),
            PlanReviewInput::Right => self.scroll_horizontal(1, max_horizontal),
            PlanReviewInput::PageUp => {
                self.vertical = self
                    .vertical
                    .saturating_sub(usize::from(body.height.max(1)));
                None
            }
            PlanReviewInput::PageDown => {
                self.vertical = self
                    .vertical
                    .saturating_add(usize::from(body.height.max(1)))
                    .min(max_vertical);
                None
            }
            PlanReviewInput::Top => {
                self.vertical = 0;
                None
            }
            PlanReviewInput::Bottom => {
                self.vertical = max_vertical;
                None
            }
            PlanReviewInput::LeftEdge => {
                self.horizontal = 0;
                None
            }
            PlanReviewInput::RightEdge => {
                self.horizontal = max_horizontal;
                None
            }
            PlanReviewInput::SearchCancel => {
                if current_query.is_empty() {
                    return None;
                }
                self.selected = None;
                self.vertical = 0;
                self.horizontal = 0;
                Some(String::new())
            }
            PlanReviewInput::SearchNext => {
                self.move_selection(1, body, max_vertical, max_horizontal, matches);
                None
            }
            PlanReviewInput::SearchPrevious => {
                self.move_selection(-1, body, max_vertical, max_horizontal, matches);
                None
            }
            PlanReviewInput::OpenHelp => {
                self.overlay = Some(PlanReviewOverlay::Help);
                self.overlay_scroll.reset();
                None
            }
            PlanReviewInput::OpenContext => {
                self.overlay = Some(PlanReviewOverlay::Context);
                self.overlay_scroll.reset();
                None
            }
            PlanReviewInput::SearchChar(_)
            | PlanReviewInput::SearchBackspace
            | PlanReviewInput::SearchLeft
            | PlanReviewInput::SearchRight
            | PlanReviewInput::SearchHome
            | PlanReviewInput::SearchEnd
            | PlanReviewInput::SearchConfirm
            | PlanReviewInput::Apply
            | PlanReviewInput::Copy
            | PlanReviewInput::Quit
            | PlanReviewInput::OpenOverview => None,
        }
    }

    pub(crate) fn reconcile_scroll(&mut self, max_vertical: usize, max_horizontal: usize) {
        self.clamp_scroll(max_vertical, max_horizontal);
    }

    pub(crate) fn reconcile(
        &mut self,
        body: Rect,
        max_vertical: usize,
        max_horizontal: usize,
        matches: &[PlanReviewMatch],
    ) {
        self.clamp_scroll(max_vertical, max_horizontal);
        let Some(selected) = self.selected else {
            return;
        };
        if selected >= matches.len() {
            self.selected = None;
            return;
        }
        self.ensure_selected_visible(body, max_vertical, max_horizontal, matches);
    }

    pub(crate) const fn searching(&self) -> bool {
        self.search.is_some()
    }

    pub(crate) fn search_query(&self) -> Option<&str> {
        self.search.as_ref().map(|search| search.query.as_str())
    }

    pub(crate) const fn search_cursor(&self) -> Option<usize> {
        match &self.search {
            Some(search) => Some(search.cursor),
            None => None,
        }
    }

    pub(crate) const fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub(crate) const fn scroll(&self) -> (usize, usize) {
        (self.vertical, self.horizontal)
    }

    pub(crate) const fn overlay(&self) -> Option<PlanReviewOverlay> {
        self.overlay
    }

    pub(crate) const fn overlay_scroll(&self) -> &DialogScroll {
        &self.overlay_scroll
    }

    pub(crate) const fn overview(&self) -> &OverviewViewState {
        &self.overview
    }

    pub(crate) const fn overview_mut(&mut self) -> &mut OverviewViewState {
        &mut self.overview
    }

    pub(crate) fn jump_to_line(&mut self, line: usize, max_vertical: usize) {
        self.vertical = line.min(max_vertical);
        self.horizontal = 0;
        self.selected = None;
    }

    pub(crate) fn scroll_overlay(&mut self, delta: i16) {
        self.overlay_scroll.scroll_by(delta);
    }

    pub(crate) fn scroll_overlay_left(&mut self) {
        self.overlay_scroll.scroll_left();
    }

    pub(crate) fn scroll_overlay_right(&mut self) {
        self.overlay_scroll.scroll_right();
    }

    pub(crate) const fn overlay_top(&mut self) {
        self.overlay_scroll.top();
    }

    pub(crate) const fn overlay_bottom(&mut self) {
        self.overlay_scroll.bottom();
    }

    pub(crate) const fn close_overlay(&mut self) {
        self.overlay = None;
    }

    fn apply_search_input(
        &mut self,
        input: PlanReviewInput,
        body: Rect,
        max_vertical: usize,
        max_horizontal: usize,
        matches: &[PlanReviewMatch],
    ) -> Option<String> {
        if matches!(
            input,
            PlanReviewInput::SearchConfirm | PlanReviewInput::SearchCancel
        ) {
            let search = self.search.take()?;
            return match input {
                PlanReviewInput::SearchConfirm => {
                    self.selected = (!search.query.is_empty() && !matches.is_empty()).then_some(0);
                    if self.selected.is_some() {
                        self.ensure_selected_visible(body, max_vertical, max_horizontal, matches);
                    }
                    None
                }
                PlanReviewInput::SearchCancel => {
                    self.vertical = search.previous_vertical;
                    self.horizontal = search.previous_horizontal;
                    self.selected = search.previous_selected;
                    Some(search.previous_query)
                }
                _ => unreachable!("search lifecycle input should match the guard"),
            };
        }

        let search = self.search.as_mut()?;
        match input {
            PlanReviewInput::SearchChar(character) => {
                search.query.insert(search.cursor, character);
                search.cursor = text_input::next_grapheme_boundary_at_or_after(
                    &search.query,
                    search.cursor + character.len_utf8(),
                );
                self.selected = None;
                self.vertical = 0;
                self.horizontal = 0;
                Some(search.query.clone())
            }
            PlanReviewInput::SearchBackspace => {
                if search.cursor > 0 {
                    let previous =
                        text_input::previous_grapheme_boundary(&search.query, search.cursor);
                    search.query.drain(previous..search.cursor);
                    search.cursor = previous;
                    self.selected = None;
                    self.vertical = 0;
                    self.horizontal = 0;
                }
                Some(search.query.clone())
            }
            PlanReviewInput::SearchLeft => {
                search.cursor =
                    text_input::previous_grapheme_boundary(&search.query, search.cursor);
                None
            }
            PlanReviewInput::SearchRight => {
                search.cursor = text_input::next_grapheme_boundary(&search.query, search.cursor);
                None
            }
            PlanReviewInput::SearchHome => {
                search.cursor = 0;
                None
            }
            PlanReviewInput::SearchEnd => {
                search.cursor = search.query.len();
                None
            }
            _ => None,
        }
    }

    fn move_selection(
        &mut self,
        direction: i8,
        body: Rect,
        max_vertical: usize,
        max_horizontal: usize,
        matches: &[PlanReviewMatch],
    ) {
        if matches.is_empty() {
            return;
        }
        let next = match (self.selected, direction.is_negative()) {
            (Some(selected), false) => selected.saturating_add(1) % matches.len(),
            (Some(selected), true) => selected.checked_sub(1).unwrap_or(matches.len() - 1),
            (None, false) => 0,
            (None, true) => matches.len() - 1,
        };
        self.selected = Some(next);
        self.ensure_selected_visible(body, max_vertical, max_horizontal, matches);
    }

    fn ensure_selected_visible(
        &mut self,
        body: Rect,
        max_vertical: usize,
        max_horizontal: usize,
        matches: &[PlanReviewMatch],
    ) {
        let Some(selected) = self.selected else {
            return;
        };
        let Some(selected) = matches.get(selected).copied() else {
            return;
        };
        let height = usize::from(body.height);
        if height > 0 {
            let line = selected.line();
            if line < self.vertical {
                self.vertical = line;
            } else if line >= self.vertical.saturating_add(height) {
                self.vertical = line.saturating_sub(height - 1).min(max_vertical);
            }
        }
        let width = usize::from(body.width);
        if width == 0 {
            return;
        }
        let start = selected.start();
        let end = selected.end();
        let match_width = end.saturating_sub(start);
        if match_width >= width {
            self.horizontal = start.min(max_horizontal);
        } else if start < self.horizontal {
            self.horizontal = start;
        } else if end > self.horizontal.saturating_add(width) {
            self.horizontal = end.saturating_sub(width).min(max_horizontal);
        }
    }

    fn clamp_scroll(&mut self, max_vertical: usize, max_horizontal: usize) {
        self.vertical = self.vertical.min(max_vertical);
        self.horizontal = self.horizontal.min(max_horizontal);
    }

    fn scroll_vertical(&mut self, delta: isize, max: usize) -> Option<String> {
        self.vertical = if delta.is_negative() {
            self.vertical.saturating_sub(delta.unsigned_abs())
        } else {
            self.vertical.saturating_add(delta.unsigned_abs()).min(max)
        };
        None
    }

    fn scroll_horizontal(&mut self, delta: isize, max: usize) -> Option<String> {
        self.horizontal = if delta.is_negative() {
            self.horizontal.saturating_sub(delta.unsigned_abs())
        } else {
            self.horizontal
                .saturating_add(delta.unsigned_abs())
                .min(max)
        };
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: Rect = Rect::new(0, 0, 10, 3);
    const MAX_VERTICAL: usize = 20;
    const MAX_HORIZONTAL: usize = 30;

    #[test]
    fn search_input_changes_query_and_resets_scroll() {
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::Bottom,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::RightEdge,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );

        assert_eq!(
            view.apply_with_matches(
                PlanReviewInput::SearchChar('a'),
                BODY,
                MAX_VERTICAL,
                MAX_HORIZONTAL,
                "existing",
                &[]
            ),
            Some("existinga".to_owned())
        );
        assert_eq!(view.search_query(), Some("existinga"));
        assert_eq!(view.scroll(), (0, 0));
    }

    #[test]
    fn escape_restores_query_position_and_selection() {
        let matches = [PlanReviewMatch {
            line: 1,
            start: 0,
            end: 4,
        }];
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &matches,
        );
        view.apply_with_matches(
            PlanReviewInput::SearchConfirm,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &matches,
        );
        view.apply_with_matches(
            PlanReviewInput::Bottom,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &matches,
        );
        let previous = view.scroll();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchChar('a'),
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );

        assert_eq!(
            view.apply_with_matches(
                PlanReviewInput::SearchCancel,
                BODY,
                MAX_VERTICAL,
                MAX_HORIZONTAL,
                "existing",
                &[]
            ),
            Some("existing".to_owned())
        );
        assert!(!view.searching());
        assert_eq!(view.scroll(), previous);
        assert_eq!(view.selected(), Some(0));
    }

    #[test]
    fn search_right_moves_to_the_next_grapheme_boundary() {
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchHome,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchChar('あ'),
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchChar('b'),
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchHome,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchRight,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchChar('X'),
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );

        assert_eq!(view.search_query(), Some("あXbexisting"));
        assert_eq!(view.search_cursor(), Some("あX".len()));
    }

    #[test]
    fn backspace_removes_a_combining_grapheme_as_one_input_unit() {
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchHome,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchChar('e'),
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchChar('\u{301}'),
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );

        view.apply_with_matches(
            PlanReviewInput::SearchBackspace,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "existing",
            &[],
        );

        assert_eq!(view.search_query(), Some("existing"));
        assert_eq!(view.search_cursor(), Some(0));
    }

    #[test]
    fn inserted_zwj_keeps_the_cursor_at_the_joined_grapheme_boundary() {
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchHome,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchChar('👩'),
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchChar('💻'),
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchLeft,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &[],
        );
        view.apply_with_matches(
            PlanReviewInput::SearchChar('\u{200d}'),
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &[],
        );

        assert_eq!(view.search_query(), Some("👩\u{200d}💻"));
        assert_eq!(view.search_cursor(), Some("👩\u{200d}💻".len()));
        view.apply_with_matches(
            PlanReviewInput::SearchBackspace,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &[],
        );
        assert_eq!(view.search_query(), Some(""));
        assert_eq!(view.search_cursor(), Some(0));
    }

    #[test]
    fn next_and_previous_wrap_and_keep_a_single_selection() {
        let matches = [
            PlanReviewMatch {
                line: 0,
                start: 0,
                end: 2,
            },
            PlanReviewMatch {
                line: 4,
                start: 1,
                end: 3,
            },
        ];
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "x",
            &matches,
        );
        view.apply_with_matches(
            PlanReviewInput::SearchConfirm,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "x",
            &matches,
        );
        assert_eq!(view.selected(), Some(0));

        view.apply_with_matches(
            PlanReviewInput::SearchNext,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "x",
            &matches,
        );
        assert_eq!(view.selected(), Some(1));
        view.apply_with_matches(
            PlanReviewInput::SearchNext,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "x",
            &matches,
        );
        assert_eq!(view.selected(), Some(0));
        view.apply_with_matches(
            PlanReviewInput::SearchPrevious,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "x",
            &matches,
        );
        assert_eq!(view.selected(), Some(1));
    }

    #[test]
    fn resize_clamps_scroll_without_clearing_search_selection() {
        let matches = [PlanReviewMatch::new(8, 9, 40)];
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::SearchStart,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "x",
            &matches,
        );
        view.apply_with_matches(
            PlanReviewInput::SearchConfirm,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "x",
            &matches,
        );
        assert_eq!(view.selected(), Some(0));
        assert_eq!(view.scroll(), (6, 9));

        view.apply_with_matches(
            PlanReviewInput::Bottom,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "x",
            &matches,
        );
        assert_eq!(view.scroll(), (MAX_VERTICAL, 9));

        view.reconcile_scroll(0, 10);

        assert_eq!(view.selected(), Some(0));
        assert_eq!(view.scroll(), (0, 9));
    }

    #[test]
    fn scroll_stays_within_the_calculated_offsets() {
        let mut view = PlanReviewViewState::default();

        for _ in 0..30 {
            view.apply_with_matches(
                PlanReviewInput::Down,
                BODY,
                MAX_VERTICAL,
                MAX_HORIZONTAL,
                "existing",
                &[],
            );
            view.apply_with_matches(
                PlanReviewInput::Right,
                BODY,
                MAX_VERTICAL,
                MAX_HORIZONTAL,
                "existing",
                &[],
            );
        }

        assert_eq!(view.scroll(), (MAX_VERTICAL, MAX_HORIZONTAL));
    }

    #[test]
    fn overlays_preserve_scroll_and_close_without_changing_selection() {
        let matches = [PlanReviewMatch::new(4, 0, 3)];
        let mut view = PlanReviewViewState::default();
        view.apply_with_matches(
            PlanReviewInput::Bottom,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &matches,
        );
        view.apply_with_matches(
            PlanReviewInput::SearchConfirm,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &matches,
        );
        let position = view.scroll();
        let selected = view.selected();

        view.apply_with_matches(
            PlanReviewInput::OpenHelp,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &matches,
        );
        assert_eq!(view.overlay(), Some(PlanReviewOverlay::Help));
        assert_eq!(view.scroll(), position);
        assert_eq!(view.selected(), selected);
        view.scroll_overlay(3);
        assert_eq!(view.overlay_scroll().offset_for_test(), 3);
        view.close_overlay();
        assert_eq!(view.overlay(), None);

        view.apply_with_matches(
            PlanReviewInput::OpenContext,
            BODY,
            MAX_VERTICAL,
            MAX_HORIZONTAL,
            "",
            &matches,
        );
        assert_eq!(view.overlay(), Some(PlanReviewOverlay::Context));
        assert_eq!(view.overlay_scroll().offset_for_test(), 0);
        assert_eq!(view.scroll(), position);
        assert_eq!(view.selected(), selected);
    }
}
