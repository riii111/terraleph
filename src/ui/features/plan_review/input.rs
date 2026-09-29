use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ui::input::normalize_key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanReviewInput {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Top,
    Bottom,
    LeftEdge,
    RightEdge,
    SearchStart,
    SearchChar(char),
    SearchBackspace,
    SearchLeft,
    SearchRight,
    SearchHome,
    SearchEnd,
    SearchConfirm,
    SearchCancel,
    SearchNext,
    SearchPrevious,
    OpenHelp,
    OpenContext,
    OpenOverview,
    Apply,
    Copy,
    Quit,
}

pub(crate) fn key_to_input(
    key: KeyEvent,
    searching: bool,
    filter_confirmed: bool,
) -> Option<PlanReviewInput> {
    let key = normalize_key(key);
    if searching {
        return search_key_to_input(key);
    }
    if filter_confirmed {
        return confirmed_filter_key_to_input(key);
    }
    if let Some(input) = navigation_key_to_input(key) {
        return Some(input);
    }
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::Quit)
        }
        (KeyCode::Char('/'), KeyModifiers::NONE) => Some(PlanReviewInput::SearchStart),
        (KeyCode::Esc, KeyModifiers::NONE) => Some(PlanReviewInput::SearchCancel),
        (KeyCode::Char('n'), KeyModifiers::NONE) => Some(PlanReviewInput::SearchNext),
        (KeyCode::Char('N'), KeyModifiers::NONE) => Some(PlanReviewInput::SearchPrevious),
        (KeyCode::Char('?'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            Some(PlanReviewInput::OpenHelp)
        }
        (KeyCode::Char('c'), KeyModifiers::NONE) => Some(PlanReviewInput::OpenContext),
        (KeyCode::Char('s'), KeyModifiers::NONE) => Some(PlanReviewInput::OpenOverview),
        (KeyCode::Char('y'), KeyModifiers::NONE) => Some(PlanReviewInput::Copy),
        (KeyCode::Char('a'), KeyModifiers::NONE) => Some(PlanReviewInput::Apply),
        (KeyCode::Char('q'), KeyModifiers::NONE) => Some(PlanReviewInput::Quit),
        _ => None,
    }
}

const fn confirmed_filter_key_to_input(key: KeyEvent) -> Option<PlanReviewInput> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::Quit)
        }
        (KeyCode::Char('/'), KeyModifiers::NONE) => Some(PlanReviewInput::SearchStart),
        (KeyCode::Esc, KeyModifiers::NONE) => Some(PlanReviewInput::SearchCancel),
        (KeyCode::Char('n'), KeyModifiers::NONE) => Some(PlanReviewInput::SearchNext),
        (KeyCode::Char('N'), KeyModifiers::NONE) => Some(PlanReviewInput::SearchPrevious),
        (KeyCode::Char('?'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            Some(PlanReviewInput::OpenHelp)
        }
        (KeyCode::Char('c'), KeyModifiers::NONE) => Some(PlanReviewInput::OpenContext),
        (KeyCode::Char('y'), KeyModifiers::NONE) => Some(PlanReviewInput::Copy),
        (KeyCode::Char('a'), KeyModifiers::NONE) => Some(PlanReviewInput::Apply),
        (KeyCode::Char('s'), KeyModifiers::NONE) => Some(PlanReviewInput::OpenOverview),
        (KeyCode::Char('q'), KeyModifiers::NONE) => Some(PlanReviewInput::Quit),
        _ => navigation_key_to_input(key),
    }
}

const fn navigation_key_to_input(key: KeyEvent) -> Option<PlanReviewInput> {
    match (key.code, key.modifiers) {
        (KeyCode::Char('p'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::Up)
        }
        (KeyCode::Char('n'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::Down)
        }
        (KeyCode::Char('b'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::Left)
        }
        (KeyCode::Char('f'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::Right)
        }
        (KeyCode::Char('a'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::LeftEdge)
        }
        (KeyCode::Char('e'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::RightEdge)
        }
        (KeyCode::Char('<'), modifiers) if modifiers.contains(KeyModifiers::ALT) => {
            Some(PlanReviewInput::Top)
        }
        (KeyCode::Char('>'), modifiers) if modifiers.contains(KeyModifiers::ALT) => {
            Some(PlanReviewInput::Bottom)
        }
        (KeyCode::Char('v'), modifiers) if modifiers.contains(KeyModifiers::ALT) => {
            Some(PlanReviewInput::PageUp)
        }
        (KeyCode::Char('v'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::PageDown)
        }
        (KeyCode::Up | KeyCode::Char('k'), _) => Some(PlanReviewInput::Up),
        (KeyCode::Down | KeyCode::Char('j'), _) => Some(PlanReviewInput::Down),
        (KeyCode::Left | KeyCode::Char('h'), _) => Some(PlanReviewInput::Left),
        (KeyCode::Right | KeyCode::Char('l'), _) => Some(PlanReviewInput::Right),
        (KeyCode::PageUp, _) => Some(PlanReviewInput::PageUp),
        (KeyCode::PageDown, _) => Some(PlanReviewInput::PageDown),
        (KeyCode::Home, _) => Some(PlanReviewInput::Top),
        (KeyCode::End, _) => Some(PlanReviewInput::Bottom),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyConfirmationInput {
    Character(char),
    Backspace,
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    Left,
    Right,
    Home,
    End,
    OpenHelp,
    OpenContext,
    Confirm,
    Cancel,
}

pub(super) fn apply_confirmation_key_to_input(key: KeyEvent) -> Option<ApplyConfirmationInput> {
    let key = normalize_key(key);
    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => Some(ApplyConfirmationInput::Confirm),
        (KeyCode::Esc, _) => Some(ApplyConfirmationInput::Cancel),
        (KeyCode::Char('?'), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            Some(ApplyConfirmationInput::OpenHelp)
        }
        (KeyCode::Tab, _) => Some(ApplyConfirmationInput::OpenContext),
        (KeyCode::Backspace, _) => Some(ApplyConfirmationInput::Backspace),
        (KeyCode::Up, _) => Some(ApplyConfirmationInput::ScrollUp),
        (KeyCode::Down, _) => Some(ApplyConfirmationInput::ScrollDown),
        (KeyCode::PageUp, _) => Some(ApplyConfirmationInput::PageUp),
        (KeyCode::PageDown, _) => Some(ApplyConfirmationInput::PageDown),
        (KeyCode::Left, _) => Some(ApplyConfirmationInput::Left),
        (KeyCode::Right, _) => Some(ApplyConfirmationInput::Right),
        (KeyCode::Home, _) => Some(ApplyConfirmationInput::Home),
        (KeyCode::End, _) => Some(ApplyConfirmationInput::End),
        (KeyCode::Char('a'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(ApplyConfirmationInput::Home)
        }
        (KeyCode::Char('e'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(ApplyConfirmationInput::End)
        }
        (KeyCode::Char(character), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            Some(ApplyConfirmationInput::Character(character))
        }
        _ => None,
    }
}

const fn search_key_to_input(key: KeyEvent) -> Option<PlanReviewInput> {
    match (key.code, key.modifiers) {
        (KeyCode::Enter, _) => Some(PlanReviewInput::SearchConfirm),
        (KeyCode::Esc, _) => Some(PlanReviewInput::SearchCancel),
        (KeyCode::Char('c'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::SearchCancel)
        }
        (KeyCode::Backspace, _) => Some(PlanReviewInput::SearchBackspace),
        (KeyCode::Home, _) => Some(PlanReviewInput::SearchHome),
        (KeyCode::End, _) => Some(PlanReviewInput::SearchEnd),
        (KeyCode::Char('a'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::SearchHome)
        }
        (KeyCode::Char('e'), modifiers) if modifiers.contains(KeyModifiers::CONTROL) => {
            Some(PlanReviewInput::SearchEnd)
        }
        (KeyCode::Left, _) => Some(PlanReviewInput::SearchLeft),
        (KeyCode::Right, _) => Some(PlanReviewInput::SearchRight),
        (KeyCode::Char(character), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
            Some(PlanReviewInput::SearchChar(character))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_review_enter_has_no_special_action() {
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(key_to_input(key, false, false), None);
    }

    #[test]
    fn slash_starts_search_only_outside_input() {
        let key = KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE);

        assert_eq!(
            key_to_input(key, false, false),
            Some(PlanReviewInput::SearchStart)
        );
        assert_eq!(
            key_to_input(key, true, false),
            Some(PlanReviewInput::SearchChar('/'))
        );
    }

    #[test]
    fn search_input_prioritizes_editing_keys() {
        assert_eq!(
            key_to_input(
                KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
                true,
                false,
            ),
            Some(PlanReviewInput::SearchChar('x'))
        );
        assert_eq!(
            key_to_input(
                KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
                true,
                false,
            ),
            Some(PlanReviewInput::SearchBackspace)
        );
        assert_eq!(
            key_to_input(
                KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL),
                true,
                false,
            ),
            Some(PlanReviewInput::SearchRight)
        );
        assert_eq!(
            key_to_input(
                KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
                true,
                false,
            ),
            Some(PlanReviewInput::SearchChar('n'))
        );
    }

    #[test]
    fn confirmed_filter_keys_clear_or_move_matches() {
        assert_eq!(
            key_to_input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), false, true),
            Some(PlanReviewInput::SearchCancel)
        );
        assert_eq!(
            key_to_input(
                KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
                false,
                true,
            ),
            Some(PlanReviewInput::SearchNext)
        );
        assert_eq!(
            key_to_input(
                KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT),
                false,
                true,
            ),
            Some(PlanReviewInput::SearchPrevious)
        );
    }

    #[test]
    fn ctrl_c_quits_outside_search_and_cancels_search_input() {
        struct CtrlCCase {
            name: &'static str,
            searching: bool,
            expected: PlanReviewInput,
        }
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);

        for case in [
            CtrlCCase {
                name: "full_review",
                searching: false,
                expected: PlanReviewInput::Quit,
            },
            CtrlCCase {
                name: "search_input",
                searching: true,
                expected: PlanReviewInput::SearchCancel,
            },
        ] {
            assert_eq!(
                key_to_input(key, case.searching, false),
                Some(case.expected),
                "case: {}",
                case.name
            );
        }
    }

    #[test]
    fn help_and_context_keys_are_available_only_on_their_review_screens() {
        for filter_confirmed in [false, true] {
            assert_eq!(
                key_to_input(
                    KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
                    false,
                    filter_confirmed,
                ),
                Some(PlanReviewInput::OpenHelp)
            );
            assert_eq!(
                key_to_input(
                    KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE),
                    false,
                    filter_confirmed,
                ),
                Some(PlanReviewInput::OpenContext)
            );
        }
        assert_eq!(
            apply_confirmation_key_to_input(
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT,)
            ),
            Some(ApplyConfirmationInput::OpenHelp)
        );
        assert_eq!(
            apply_confirmation_key_to_input(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE,)),
            Some(ApplyConfirmationInput::OpenContext)
        );
        assert_eq!(
            apply_confirmation_key_to_input(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE,)),
            Some(ApplyConfirmationInput::Character('c'))
        );
        for character in ['j', 'k'] {
            assert_eq!(
                key_to_input(
                    KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE),
                    true,
                    false,
                ),
                Some(PlanReviewInput::SearchChar(character))
            );
            assert_eq!(
                apply_confirmation_key_to_input(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                )),
                Some(ApplyConfirmationInput::Character(character))
            );
        }
        assert_eq!(
            key_to_input(
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
                true,
                false,
            ),
            Some(PlanReviewInput::SearchChar('?'))
        );
    }

    #[test]
    fn apply_confirmation_accepts_only_explicit_confirmation_keys() {
        assert_eq!(
            apply_confirmation_key_to_input(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE,)),
            Some(ApplyConfirmationInput::Confirm)
        );
        assert_eq!(
            apply_confirmation_key_to_input(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Some(ApplyConfirmationInput::Cancel)
        );
        assert_eq!(
            apply_confirmation_key_to_input(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
            )),
            None
        );
        assert_eq!(
            apply_confirmation_key_to_input(KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::CONTROL,
            )),
            Some(ApplyConfirmationInput::Home)
        );
        assert_eq!(
            apply_confirmation_key_to_input(KeyEvent::new(
                KeyCode::Char('e'),
                KeyModifiers::CONTROL,
            )),
            Some(ApplyConfirmationInput::End)
        );
        assert_eq!(
            apply_confirmation_key_to_input(KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::CONTROL,
            )),
            Some(ApplyConfirmationInput::Left)
        );
        assert_eq!(
            apply_confirmation_key_to_input(KeyEvent::new(
                KeyCode::Char('f'),
                KeyModifiers::CONTROL,
            )),
            Some(ApplyConfirmationInput::Right)
        );
        for character in ['q', 'y'] {
            assert_eq!(
                apply_confirmation_key_to_input(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                )),
                Some(ApplyConfirmationInput::Character(character))
            );
        }
    }
}
