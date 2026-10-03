use crossterm::event::KeyEvent;
use ratatui::layout::{Rect, Size};

use crate::app::{execution::ExecutionState, session::Action};

use super::{
    ExecutionScroll, ExecutionTargetMove, ExecutionViewState,
    input::{ExecutionInput, execution_key_to_input},
    render::{
        execution_horizontal_scroll_position_with_view, execution_layout_with_view,
        execution_scroll_position_with_view, execution_target_scroll_position_with_view,
    },
};

impl ExecutionViewState {
    pub(crate) fn handle_key(
        &mut self,
        state: &ExecutionState,
        key: KeyEvent,
        size: Size,
    ) -> Option<Action> {
        match execution_key_to_input(key, state.stage(), self.logs_open()) {
            Some(ExecutionInput::Quit) => Some(Action::Quit),
            Some(ExecutionInput::RequestCancellation) => Some(Action::RequestCancellation),
            Some(ExecutionInput::SelectTarget(direction)) => {
                self.move_target_selection(state, direction, size);
                None
            }
            Some(ExecutionInput::ToggleFocus) => {
                self.toggle_focus();
                None
            }
            Some(ExecutionInput::OpenLogs) => {
                self.open_logs();
                None
            }
            Some(ExecutionInput::CloseLogs) => {
                self.close_logs();
                None
            }
            Some(ExecutionInput::End) => {
                self.end();
                None
            }
            Some(ExecutionInput::Scroll(scroll)) => {
                self.scroll(state, scroll, size);
                None
            }
            Some(ExecutionInput::Copy(target)) => Some(Action::Copy(target)),
            None => None,
        }
    }

    fn move_target_selection(
        &mut self,
        state: &ExecutionState,
        direction: ExecutionTargetMove,
        size: Size,
    ) {
        let targets = state
            .progress()
            .display_target_indices(state.result().is_some());
        self.select_target(direction, &targets);
        self.measure_log(state.progress());
        let layout = execution_layout_with_view(Rect::from(size), state, *self);
        if let Some(position) = self
            .selected_target()
            .and_then(|selected| targets.iter().position(|index| *index == selected))
        {
            self.ensure_target_visible(
                position,
                layout.target_body().height,
                layout.target_max_vertical(),
            );
        }
    }

    fn scroll(&mut self, state: &ExecutionState, scroll: ExecutionScroll, size: Size) {
        let layout = execution_layout_with_view(Rect::from(size), state, *self);
        if state.is_apply() && !self.logs_open() {
            let (current, max) = execution_target_scroll_position_with_view(*self, &layout);
            self.apply_target_scroll(scroll, current, max, layout.target_body().height);
            return;
        }
        let (current_vertical, _) = execution_scroll_position_with_view(state, *self, &layout);
        match scroll {
            ExecutionScroll::Left
            | ExecutionScroll::Right
            | ExecutionScroll::LeftEdge
            | ExecutionScroll::RightEdge => {
                let (current, max) = execution_horizontal_scroll_position_with_view(*self, &layout);
                self.apply_horizontal_scroll(scroll, current, max, current_vertical);
            }
            _ => {
                let (current, max) = execution_scroll_position_with_view(state, *self, &layout);
                self.apply_scroll(scroll, current, max, layout.body().height);
            }
        }
    }
}
