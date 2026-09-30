use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::copy::CopyTarget;
use crate::app::execution::{ExecutionAction, ExecutionStage};
use crate::ui::input::normalize_key;

use super::{ExecutionScroll, ExecutionTargetMove};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ExecutionInput {
    Action(ExecutionAction),
    SelectTarget(ExecutionTargetMove),
    ToggleFocus,
    Scroll(ExecutionScroll),
    Copy(CopyTarget),
    End,
    OpenLogs,
    CloseLogs,
    Quit,
}

pub(super) fn execution_key_to_input(
    key: KeyEvent,
    stage: ExecutionStage,
    logs_open: bool,
) -> Option<ExecutionInput> {
    let key = normalize_key(key);
    let apply_in_progress = stage == ExecutionStage::Applying;

    let finished = matches!(
        stage,
        ExecutionStage::Failed
            | ExecutionStage::ApplySucceeded
            | ExecutionStage::ApplyFailed
            | ExecutionStage::ApplyInterrupted
    );
    let apply_result = matches!(
        stage,
        ExecutionStage::ApplySucceeded
            | ExecutionStage::ApplyFailed
            | ExecutionStage::ApplyInterrupted
    );
    let apply_screen = apply_in_progress || apply_result;

    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(if finished {
            ExecutionInput::Quit
        } else {
            ExecutionInput::Action(ExecutionAction::RequestCancellation)
        });
    }

    if finished && key.code == KeyCode::Char('q') {
        return Some(ExecutionInput::Quit);
    }
    if finished && key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Char('y') {
        return Some(ExecutionInput::Copy(if apply_result {
            CopyTarget::Execution
        } else {
            CopyTarget::Diagnostic
        }));
    }

    if apply_screen && key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Tab {
        return Some(ExecutionInput::ToggleFocus);
    }

    if apply_in_progress && key.modifiers == KeyModifiers::NONE {
        if key.code == KeyCode::Char('v') {
            return Some(if logs_open {
                ExecutionInput::CloseLogs
            } else {
                ExecutionInput::OpenLogs
            });
        }
        if logs_open && key.code == KeyCode::Esc {
            return Some(ExecutionInput::CloseLogs);
        }
    }

    if apply_screen && !logs_open {
        return match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                Some(ExecutionInput::SelectTarget(ExecutionTargetMove::Previous))
            }
            KeyCode::Down | KeyCode::Char('j') => {
                Some(ExecutionInput::SelectTarget(ExecutionTargetMove::Next))
            }
            _ => None,
        };
    }

    if key.code == KeyCode::Char('a') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(ExecutionInput::Scroll(ExecutionScroll::LeftEdge));
    }
    if key.code == KeyCode::Char('e') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(ExecutionInput::Scroll(ExecutionScroll::RightEdge));
    }
    if key.code == KeyCode::Char('<') && key.modifiers.contains(KeyModifiers::ALT) {
        return Some(ExecutionInput::Scroll(ExecutionScroll::Top));
    }
    if key.code == KeyCode::Char('>') && key.modifiers.contains(KeyModifiers::ALT) {
        return Some(ExecutionInput::End);
    }
    if key.code == KeyCode::Char('v') && key.modifiers.contains(KeyModifiers::ALT) {
        return Some(ExecutionInput::Scroll(ExecutionScroll::PageUp));
    }
    if key.code == KeyCode::Char('v') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Some(ExecutionInput::Scroll(ExecutionScroll::PageDown));
    }

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => Some(ExecutionInput::Scroll(ExecutionScroll::Up)),
        KeyCode::Down | KeyCode::Char('j') => Some(ExecutionInput::Scroll(ExecutionScroll::Down)),
        KeyCode::Left | KeyCode::Char('h') => Some(ExecutionInput::Scroll(ExecutionScroll::Left)),
        KeyCode::Right | KeyCode::Char('l') => Some(ExecutionInput::Scroll(ExecutionScroll::Right)),
        KeyCode::PageUp => Some(ExecutionInput::Scroll(ExecutionScroll::PageUp)),
        KeyCode::PageDown => Some(ExecutionInput::Scroll(ExecutionScroll::PageDown)),
        KeyCode::Home => Some(ExecutionInput::Scroll(ExecutionScroll::Top)),
        KeyCode::End => Some(ExecutionInput::End),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEventKind, KeyEventState};
    use rstest::rstest;

    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn key_mapping_respects_execution_stage() {
        let cases = [
            (
                "control_c_cancels_running",
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                ExecutionStage::Reading,
                Some(ExecutionInput::Action(ExecutionAction::RequestCancellation)),
            ),
            (
                "q_is_ignored_while_running",
                key(KeyCode::Char('q'), KeyModifiers::NONE),
                ExecutionStage::Reading,
                None,
            ),
            (
                "q_quits_failed",
                key(KeyCode::Char('q'), KeyModifiers::NONE),
                ExecutionStage::Failed,
                Some(ExecutionInput::Quit),
            ),
            (
                "control_c_quits_failed",
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                ExecutionStage::Failed,
                Some(ExecutionInput::Quit),
            ),
            (
                "y_copies_diagnostic_after_failure",
                key(KeyCode::Char('y'), KeyModifiers::NONE),
                ExecutionStage::Failed,
                Some(ExecutionInput::Copy(CopyTarget::Diagnostic)),
            ),
            (
                "alt_v_pages_up",
                key(KeyCode::Char('v'), KeyModifiers::ALT),
                ExecutionStage::Reading,
                Some(ExecutionInput::Scroll(ExecutionScroll::PageUp)),
            ),
            (
                "control_v_pages_down",
                key(KeyCode::Char('v'), KeyModifiers::CONTROL),
                ExecutionStage::Reading,
                Some(ExecutionInput::Scroll(ExecutionScroll::PageDown)),
            ),
        ];

        for (name, input, stage, expected) in cases {
            assert_eq!(
                execution_key_to_input(input, stage, false),
                expected,
                "case: {name}"
            );
        }
    }

    #[test]
    fn uppercase_y_does_not_copy_after_failure() {
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Char('Y'), KeyModifiers::NONE),
                ExecutionStage::Failed,
                false,
            ),
            None
        );
    }

    #[rstest]
    #[case::lowercase(KeyCode::Char('y'), KeyModifiers::NONE)]
    #[case::uppercase(KeyCode::Char('Y'), KeyModifiers::NONE)]
    fn copy_keys_are_ignored_while_running(#[case] code: KeyCode, #[case] modifiers: KeyModifiers) {
        assert_eq!(
            execution_key_to_input(key(code, modifiers), ExecutionStage::Reading, false),
            None
        );
    }

    #[test]
    fn plain_v_opens_and_closes_apply_logs_without_changing_other_v_keys() {
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Char('v'), KeyModifiers::NONE),
                ExecutionStage::Applying,
                false,
            ),
            Some(ExecutionInput::OpenLogs)
        );
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Char('v'), KeyModifiers::NONE),
                ExecutionStage::Applying,
                true,
            ),
            Some(ExecutionInput::CloseLogs)
        );
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Esc, KeyModifiers::NONE),
                ExecutionStage::Applying,
                true,
            ),
            Some(ExecutionInput::CloseLogs)
        );
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Char('v'), KeyModifiers::ALT),
                ExecutionStage::Applying,
                false,
            ),
            None
        );
    }

    #[test]
    fn focus_switches_target_selection_and_cancellation_remain_available_while_running() {
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Tab, KeyModifiers::NONE),
                ExecutionStage::Applying,
                false,
            ),
            Some(ExecutionInput::ToggleFocus)
        );
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Down, KeyModifiers::NONE),
                ExecutionStage::Applying,
                false,
            ),
            Some(ExecutionInput::SelectTarget(ExecutionTargetMove::Next))
        );
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                ExecutionStage::Applying,
                false,
            ),
            Some(ExecutionInput::Action(ExecutionAction::RequestCancellation))
        );
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Char('c'), KeyModifiers::CONTROL),
                ExecutionStage::Applying,
                true,
            ),
            Some(ExecutionInput::Action(ExecutionAction::RequestCancellation))
        );
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Down, KeyModifiers::NONE),
                ExecutionStage::Applying,
                true,
            ),
            Some(ExecutionInput::Scroll(ExecutionScroll::Down))
        );
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Tab, KeyModifiers::NONE),
                ExecutionStage::ApplySucceeded,
                false,
            ),
            Some(ExecutionInput::ToggleFocus)
        );
        assert_eq!(
            execution_key_to_input(
                key(KeyCode::Down, KeyModifiers::NONE),
                ExecutionStage::ApplySucceeded,
                false,
            ),
            Some(ExecutionInput::SelectTarget(ExecutionTargetMove::Next))
        );
    }
}
