use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Rect, Size};

use crate::app::session::{Action, ReviewSessionState};
use crate::ui::{primitives::molecules::dialog_scroll::DialogScroll, text_input::TextInput};

use super::{ApplyConfirmationInput, apply_confirmation_key_to_input, apply_confirmation_layout};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ApplyConfirmationViewState {
    input: TextInput,
    scroll: u16,
    rejected: bool,
    overlay: Option<ConfirmationOverlay>,
    overlay_scroll: DialogScroll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmationOverlay {
    Help,
    Context,
}

impl ApplyConfirmationViewState {
    pub(crate) fn handle_key(
        &mut self,
        state: &ReviewSessionState,
        key: KeyEvent,
        size: Size,
        now: Instant,
    ) -> Option<Action> {
        if self.overlay().is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.close_overlay();
            } else {
                self.overlay_scroll_mut().handle_key(key.code, 8);
            }
            return None;
        }
        let layout = apply_confirmation_layout(Rect::from(size), state, now);
        let input = apply_confirmation_key_to_input(key);
        let input = match input {
            Some(ApplyConfirmationInput::Cancel) => input,
            Some(_) if layout.renderable() => input,
            _ => None,
        };
        let expected = state.review().confirmation_input();
        input.and_then(|input| self.apply(input, &expected, layout.max_vertical()))
    }

    pub(crate) fn apply(
        &mut self,
        input: ApplyConfirmationInput,
        expected: &str,
        max_vertical: u16,
    ) -> Option<Action> {
        self.scroll = self.scroll.min(max_vertical);
        match input {
            ApplyConfirmationInput::Character(character) => {
                self.input.insert(character);
                self.rejected = false;
                None
            }
            ApplyConfirmationInput::Backspace => {
                if self.input.backspace() {
                    self.rejected = false;
                }
                None
            }
            ApplyConfirmationInput::Left => {
                self.input.move_left();
                None
            }
            ApplyConfirmationInput::Right => {
                self.input.move_right();
                None
            }
            ApplyConfirmationInput::Home => {
                self.input.move_home();
                None
            }
            ApplyConfirmationInput::End => {
                self.input.move_end();
                None
            }
            ApplyConfirmationInput::Confirm if self.input.text() == expected => {
                self.reset();
                Some(Action::ConfirmApply(expected.to_owned()))
            }
            ApplyConfirmationInput::Cancel => {
                self.reset();
                Some(Action::CancelApply)
            }
            ApplyConfirmationInput::ScrollUp => {
                self.scroll = self.scroll.saturating_sub(1);
                None
            }
            ApplyConfirmationInput::ScrollDown => {
                self.scroll = self.scroll.saturating_add(1).min(max_vertical);
                None
            }
            ApplyConfirmationInput::PageUp => {
                self.scroll = self.scroll.saturating_sub(5);
                None
            }
            ApplyConfirmationInput::PageDown => {
                self.scroll = self.scroll.saturating_add(5).min(max_vertical);
                None
            }
            ApplyConfirmationInput::OpenHelp => {
                self.overlay = Some(ConfirmationOverlay::Help);
                self.overlay_scroll.reset();
                None
            }
            ApplyConfirmationInput::OpenContext => {
                self.overlay = Some(ConfirmationOverlay::Context);
                self.overlay_scroll.reset();
                None
            }
            ApplyConfirmationInput::Confirm => {
                self.rejected = true;
                None
            }
        }
    }

    pub(crate) const fn input(&self) -> &str {
        self.input.text()
    }

    pub(crate) const fn cursor(&self) -> usize {
        self.input.cursor()
    }

    pub(crate) const fn scroll(&self) -> u16 {
        self.scroll
    }

    pub(crate) const fn rejected(&self) -> bool {
        self.rejected
    }

    pub(crate) const fn overlay(&self) -> Option<ConfirmationOverlay> {
        self.overlay
    }

    pub(crate) const fn overlay_scroll(&self) -> &DialogScroll {
        &self.overlay_scroll
    }

    pub(crate) const fn overlay_scroll_mut(&mut self) -> &mut DialogScroll {
        &mut self.overlay_scroll
    }

    pub(crate) fn close_overlay(&mut self) {
        self.overlay = None;
        self.overlay_scroll.reset();
    }

    fn reset(&mut self) {
        self.input.clear();
        self.scroll = 0;
        self.rejected = false;
        self.overlay = None;
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    fn enter(view: &mut ApplyConfirmationViewState, value: &str) {
        for character in value.chars() {
            assert_eq!(
                view.apply(ApplyConfirmationInput::Character(character), "yes", 0),
                None
            );
        }
    }

    #[test]
    fn yes_and_no_confirmations_reset_input() {
        let mut view = ApplyConfirmationViewState::default();
        enter(&mut view, "yes");
        assert_eq!(
            view.apply(ApplyConfirmationInput::Confirm, "yes", 0),
            Some(Action::ConfirmApply("yes".to_owned()))
        );
        assert_eq!(view.input(), "");
        assert_eq!(view.cursor(), 0);

        enter(&mut view, "no");
        assert_eq!(
            view.apply(ApplyConfirmationInput::Confirm, "no", 0),
            Some(Action::ConfirmApply("no".to_owned()))
        );
        assert_eq!(view.input(), "");
        assert_eq!(view.cursor(), 0);
    }

    #[test]
    fn escape_cancels_and_resets_input() {
        let mut view = ApplyConfirmationViewState::default();
        enter(&mut view, "maybe");

        assert_eq!(
            view.apply(ApplyConfirmationInput::Cancel, "yes", 0),
            Some(Action::CancelApply)
        );
        assert_eq!(view.input(), "");
        assert_eq!(view.cursor(), 0);
    }

    #[rstest]
    #[case::empty("")]
    #[case::invalid("maybe")]
    #[case::no_when_yes_is_required("no")]
    fn confirmation_requires_exact_yes_or_no(#[case] value: &str) {
        let mut view = ApplyConfirmationViewState::default();
        enter(&mut view, value);
        let cursor = view.cursor();

        assert_eq!(view.apply(ApplyConfirmationInput::Confirm, "yes", 0), None);
        assert_eq!(view.input(), value);
        assert_eq!(view.cursor(), cursor);
        assert!(view.rejected());
    }

    #[rstest]
    #[case::character(ApplyConfirmationInput::Character('s'))]
    #[case::backspace(ApplyConfirmationInput::Backspace)]
    #[case::cancel(ApplyConfirmationInput::Cancel)]
    fn editing_or_cancelling_clears_a_rejected_confirmation(#[case] input: ApplyConfirmationInput) {
        let mut view = ApplyConfirmationViewState::default();
        enter(&mut view, "ye");
        view.apply(ApplyConfirmationInput::Confirm, "yes", 0);

        view.apply(input, "yes", 0);

        assert!(!view.rejected());
    }

    #[test]
    fn moving_the_cursor_keeps_a_rejected_confirmation() {
        let mut view = ApplyConfirmationViewState::default();
        enter(&mut view, "ye");
        view.apply(ApplyConfirmationInput::Confirm, "yes", 0);

        view.apply(ApplyConfirmationInput::Left, "yes", 0);

        assert!(view.rejected());
    }

    #[test]
    fn cursor_moves_and_backspace_follow_grapheme_boundaries() {
        let mut view = ApplyConfirmationViewState::default();
        enter(&mut view, "aあe\u{301}👩💻");

        view.apply(ApplyConfirmationInput::Home, "yes", 0);
        view.apply(ApplyConfirmationInput::Right, "yes", 0);
        view.apply(ApplyConfirmationInput::Right, "yes", 0);
        view.apply(ApplyConfirmationInput::Backspace, "yes", 0);

        assert_eq!(view.input(), "ae\u{301}👩💻");
        assert_eq!(view.cursor(), 1);

        view.apply(ApplyConfirmationInput::End, "yes", 0);
        view.apply(ApplyConfirmationInput::Left, "yes", 0);
        view.apply(ApplyConfirmationInput::Character('\u{200d}'), "yes", 0);
        view.apply(ApplyConfirmationInput::Character('x'), "yes", 0);

        assert_eq!(view.input(), "ae\u{301}👩\u{200d}💻x");
        assert_eq!(view.cursor(), view.input().len());

        view.apply(ApplyConfirmationInput::Backspace, "yes", 0);
        view.apply(ApplyConfirmationInput::Backspace, "yes", 0);
        view.apply(ApplyConfirmationInput::Backspace, "yes", 0);

        assert_eq!(view.input(), "a");
        assert_eq!(view.cursor(), 1);

        view.apply(ApplyConfirmationInput::Home, "yes", 0);
        view.apply(ApplyConfirmationInput::Character('X'), "yes", 0);
        assert_eq!(view.input(), "Xa");
    }

    #[test]
    fn opening_an_overlay_resets_its_scroll_position() {
        let mut view = ApplyConfirmationViewState::default();

        view.apply(ApplyConfirmationInput::OpenContext, "yes", 0);
        view.overlay_scroll_mut().scroll_by(4);
        assert_eq!(view.overlay_scroll().offset_for_test(), 4);

        view.close_overlay();
        view.apply(ApplyConfirmationInput::OpenHelp, "yes", 0);
        assert_eq!(view.overlay_scroll().offset_for_test(), 0);
    }
}
