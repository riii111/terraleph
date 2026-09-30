mod input;
mod keys;
mod render;

use crate::app::execution::ExecutionProgress;
use crate::ui::primitives::atoms::scroll;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionScroll {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Top,
    LeftEdge,
    RightEdge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VerticalScroll {
    Initial,
    FollowLatest,
    Manual(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExecutionTargetMove {
    Previous,
    Next,
}

// Widest rendered line among the first `entries` entries of a log panel. Logs only grow, so a
// later measurement continues from here instead of measuring every entry again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct LogWidth {
    entries: usize,
    width: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExecutionViewState {
    vertical: VerticalScroll,
    target_vertical: VerticalScroll,
    horizontal: usize,
    logs_open: bool,
    selected_target: Option<usize>,
    log_width: LogWidth,
    // Measured for the selected target only, keyed by its index.
    target_log_width: Option<(usize, LogWidth)>,
}

impl Default for ExecutionViewState {
    fn default() -> Self {
        Self {
            vertical: VerticalScroll::Initial,
            target_vertical: VerticalScroll::Initial,
            horizontal: 0,
            logs_open: false,
            selected_target: None,
            log_width: LogWidth::default(),
            target_log_width: None,
        }
    }
}

impl ExecutionViewState {
    pub(crate) fn apply_scroll(
        &mut self,
        action: ExecutionScroll,
        current_offset: usize,
        max_offset: usize,
        page_height: u16,
    ) {
        let page_height = usize::from(page_height.max(1));
        let offset = match action {
            ExecutionScroll::Up => current_offset.saturating_sub(1),
            ExecutionScroll::Down => current_offset.saturating_add(1).min(max_offset),
            ExecutionScroll::PageUp => current_offset.saturating_sub(page_height),
            ExecutionScroll::PageDown => current_offset.saturating_add(page_height).min(max_offset),
            ExecutionScroll::Top => 0,
            ExecutionScroll::Left
            | ExecutionScroll::Right
            | ExecutionScroll::LeftEdge
            | ExecutionScroll::RightEdge => current_offset,
        };
        self.vertical = VerticalScroll::Manual(offset);
    }

    fn apply_target_scroll(
        &mut self,
        action: ExecutionScroll,
        current_offset: usize,
        max_offset: usize,
        page_height: u16,
    ) {
        let page_height = usize::from(page_height.max(1));
        let offset = match action {
            ExecutionScroll::Up => current_offset.saturating_sub(1),
            ExecutionScroll::Down => current_offset.saturating_add(1).min(max_offset),
            ExecutionScroll::PageUp => current_offset.saturating_sub(page_height),
            ExecutionScroll::PageDown => current_offset.saturating_add(page_height).min(max_offset),
            ExecutionScroll::Top => 0,
            ExecutionScroll::Left
            | ExecutionScroll::Right
            | ExecutionScroll::LeftEdge
            | ExecutionScroll::RightEdge => current_offset,
        };
        self.target_vertical = VerticalScroll::Manual(offset);
    }

    pub(crate) fn apply_horizontal_scroll(
        &mut self,
        action: ExecutionScroll,
        current_offset: usize,
        max_offset: usize,
        current_vertical: usize,
    ) {
        self.vertical = VerticalScroll::Manual(current_vertical);
        self.horizontal = match action {
            ExecutionScroll::Left => current_offset.saturating_sub(1),
            ExecutionScroll::Right => current_offset.saturating_add(1).min(max_offset),
            ExecutionScroll::LeftEdge => 0,
            ExecutionScroll::RightEdge => max_offset,
            _ => current_offset,
        };
    }

    // Measures the entries appended since the last call. Rendering measures any entries this has
    // not seen yet on every frame, so callers refresh this cache when the apply log or the
    // selected target changes. A plan view never calls it: the plan log holds at most the read
    // failure, which rendering measures directly. A view belongs to one execution; the runtime
    // resets it when an apply starts.
    pub(crate) fn measure_log(&mut self, progress: &ExecutionProgress) {
        self.log_width = render::measure_log_width(self.log_width, progress.log(), None);
        self.target_log_width = self.selected_target.and_then(|index| {
            let target = progress.targets().get(index)?;
            let cached = self.measured_log_width(Some(index));
            Some((
                index,
                render::measure_log_width(cached, progress.log(), Some(target.log_ids())),
            ))
        });
    }

    // Keeps what `previous` measured of all logs when the view is reset for the same execution,
    // so a long log is not measured again at once.
    pub(crate) const fn keep_log_measurement(&mut self, previous: Self) {
        self.log_width = previous.log_width;
    }

    // What `measure_log` has measured for all logs, or for the target at `target`.
    fn measured_log_width(self, target: Option<usize>) -> LogWidth {
        target.map_or(self.log_width, |index| {
            self.target_log_width
                .filter(|(cached, _)| *cached == index)
                .map(|(_, width)| width)
                .unwrap_or_default()
        })
    }

    const fn end(&mut self) {
        self.vertical = VerticalScroll::FollowLatest;
        self.target_vertical = VerticalScroll::FollowLatest;
    }

    pub(crate) const fn open_logs(&mut self) {
        self.logs_open = true;
        self.vertical = VerticalScroll::FollowLatest;
        self.horizontal = 0;
    }

    const fn close_logs(&mut self) {
        self.logs_open = false;
    }

    const fn toggle_focus(&mut self) {
        if self.logs_open {
            self.close_logs();
        } else {
            self.logs_open = true;
        }
    }

    pub(crate) const fn initialize_target_selection(&mut self, targets: &[usize]) {
        self.selected_target = targets.first().copied();
    }

    pub(crate) fn select_result_target(
        &mut self,
        targets: &[usize],
        first_failed: Option<usize>,
        first_failed_error_line: Option<usize>,
        successful: bool,
    ) {
        self.selected_target =
            first_failed.or_else(|| successful.then(|| targets.first().copied()).flatten());
        self.logs_open = first_failed.is_some();
        self.vertical = if first_failed.is_some() {
            VerticalScroll::Manual(first_failed_error_line.unwrap_or(0))
        } else {
            VerticalScroll::Initial
        };
        self.target_vertical = VerticalScroll::Initial;
    }

    fn select_target(&mut self, direction: ExecutionTargetMove, targets: &[usize]) {
        if targets.is_empty() {
            self.selected_target = None;
            return;
        }
        self.selected_target = match direction {
            ExecutionTargetMove::Next => match self
                .selected_target
                .and_then(|selected| targets.iter().position(|index| *index == selected))
            {
                Some(position) if position + 1 < targets.len() => Some(targets[position + 1]),
                Some(_) => None,
                None => Some(targets[0]),
            },
            ExecutionTargetMove::Previous => match self
                .selected_target
                .and_then(|selected| targets.iter().position(|index| *index == selected))
            {
                Some(position) if position > 0 => Some(targets[position - 1]),
                Some(_) => None,
                None => Some(targets[targets.len() - 1]),
            },
        };
        self.target_vertical = VerticalScroll::Initial;
        self.vertical = VerticalScroll::Initial;
    }

    fn ensure_target_visible(&mut self, position: usize, height: u16, max: usize) {
        let current = self.target_vertical_offset(0, max);
        let next =
            scroll::offset_showing_range(current, (position, position), usize::from(height.max(1)));
        self.target_vertical = VerticalScroll::Manual(next.min(max));
    }

    #[must_use]
    pub(crate) const fn selected_target(self) -> Option<usize> {
        self.selected_target
    }

    #[must_use]
    pub(crate) const fn logs_open(self) -> bool {
        self.logs_open
    }

    #[must_use]
    pub(crate) const fn horizontal(self) -> usize {
        self.horizontal
    }

    #[must_use]
    pub(crate) const fn vertical_offset(self, initial: usize, max: usize) -> usize {
        match self.vertical {
            VerticalScroll::Initial => {
                if initial < max {
                    initial
                } else {
                    max
                }
            }
            VerticalScroll::FollowLatest => max,
            VerticalScroll::Manual(offset) => {
                if offset < max {
                    offset
                } else {
                    max
                }
            }
        }
    }

    #[must_use]
    pub(crate) const fn target_vertical_offset(self, initial: usize, max: usize) -> usize {
        match self.target_vertical {
            VerticalScroll::Initial => {
                if initial < max {
                    initial
                } else {
                    max
                }
            }
            VerticalScroll::FollowLatest => max,
            VerticalScroll::Manual(offset) => {
                if offset < max {
                    offset
                } else {
                    max
                }
            }
        }
    }
}

pub(crate) use render::render_execution_with_quit_confirmation;

#[cfg(test)]
pub(crate) mod test_support {
    pub(crate) use super::render::{
        execution_layout_with_view, execution_scroll_position_with_view,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_scroll_keeps_the_effective_follow_position() {
        let mut view = ExecutionViewState::default();

        view.apply_horizontal_scroll(ExecutionScroll::Right, 0, 5, 42);

        assert_eq!(view.vertical_offset(0, 42), 42);
        assert_eq!(view.horizontal(), 1);
        assert_eq!(view.vertical_offset(0, 50), 42);
    }

    #[test]
    fn initial_and_follow_latest_use_different_vertical_modes() {
        let mut view = ExecutionViewState::default();

        assert_eq!(view.vertical_offset(2, 90), 2);
        view.end();
        assert_eq!(view.vertical_offset(2, 90), 90);
    }

    #[test]
    fn horizontal_scroll_preserves_manual_vertical_position() {
        let mut view = ExecutionViewState::default();
        view.apply_scroll(ExecutionScroll::Down, 4, 10, 5);

        view.apply_horizontal_scroll(ExecutionScroll::Right, 0, 5, 5);

        assert_eq!(view.vertical_offset(0, 5), 5);
        assert_eq!(view.horizontal(), 1);
    }

    #[test]
    fn target_selection_wraps_and_focus_switches_between_panels() {
        let mut view = ExecutionViewState::default();
        view.initialize_target_selection(&[2, 4]);

        view.select_target(ExecutionTargetMove::Next, &[2, 4]);
        assert_eq!(view.selected_target(), Some(4));
        view.select_target(ExecutionTargetMove::Next, &[2, 4]);
        assert_eq!(view.selected_target(), None);
        view.select_target(ExecutionTargetMove::Next, &[2, 4]);
        assert_eq!(view.selected_target(), Some(2));
        view.select_target(ExecutionTargetMove::Previous, &[2, 4]);
        assert_eq!(view.selected_target(), None);
        view.select_target(ExecutionTargetMove::Previous, &[2, 4]);
        assert_eq!(view.selected_target(), Some(4));
        view.toggle_focus();
        assert!(view.logs_open());
        view.toggle_focus();
        assert!(!view.logs_open());
    }

    #[test]
    fn result_selection_prioritizes_bound_failures_and_falls_back_to_all_logs() {
        let mut view = ExecutionViewState::default();

        view.select_result_target(&[2, 1, 0], Some(1), Some(7), false);
        assert_eq!(view.selected_target(), Some(1));
        assert!(view.logs_open());
        assert_eq!(view.vertical_offset(8, 20), 7);

        view.select_result_target(&[1, 0], None, None, false);
        assert_eq!(view.selected_target(), None);
        assert!(!view.logs_open());

        view.select_result_target(&[2, 0], None, None, true);
        assert_eq!(view.selected_target(), Some(2));
    }
}
