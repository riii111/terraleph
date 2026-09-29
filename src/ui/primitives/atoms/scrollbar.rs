use ratatui::{
    Frame,
    layout::Rect,
    symbols::scrollbar::Set,
    widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState},
};

use crate::ui::theme;

pub(crate) fn render_vertical(
    frame: &mut Frame<'_>,
    area: Rect,
    content_length: usize,
    viewport_length: usize,
    position: usize,
) {
    if content_length <= viewport_length {
        return;
    }
    let position = position.min(max_scroll_position(content_length, viewport_length));
    let mut state = ScrollbarState::new(scrollbar_content_length(content_length, viewport_length))
        .viewport_content_length(viewport_length)
        .position(position);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .symbols(Set {
            track: "│",
            thumb: "┃",
            begin: "▲",
            end: "▼",
        })
        .thumb_style(theme::scrollbar_thumb_style())
        .track_style(theme::scrollbar_track_style())
        .begin_style(begin_style(position))
        .end_style(end_style(position, content_length, viewport_length));
    frame.render_stateful_widget(scrollbar, area, &mut state);
}

pub(crate) fn render_horizontal(
    frame: &mut Frame<'_>,
    area: Rect,
    content_length: usize,
    viewport_length: usize,
    position: usize,
) {
    if content_length <= viewport_length {
        return;
    }
    let position = position.min(max_scroll_position(content_length, viewport_length));
    let mut state = ScrollbarState::new(scrollbar_content_length(content_length, viewport_length))
        .viewport_content_length(viewport_length)
        .position(position);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
        .symbols(Set {
            track: "─",
            thumb: "═",
            begin: "◀︎",
            end: "▶︎",
        })
        .thumb_style(theme::scrollbar_thumb_style())
        .track_style(theme::scrollbar_track_style())
        .begin_style(begin_style(position))
        .end_style(end_style(position, content_length, viewport_length));
    frame.render_stateful_widget(scrollbar, area, &mut state);
}

/// Returns whether the vertical and horizontal bars take a column and a row from `area`. Each
/// bar can make the other one necessary, so the answer is settled by iterating.
pub(crate) fn reservations(line_count: usize, line_width: usize, area: Rect) -> (bool, bool) {
    let mut vertical = false;
    let mut horizontal = false;
    loop {
        let next_vertical =
            line_count > usize::from(area.height.saturating_sub(u16::from(horizontal)));
        let next_horizontal =
            line_width > usize::from(area.width.saturating_sub(u16::from(vertical)));
        if next_vertical == vertical && next_horizontal == horizontal {
            return (vertical, horizontal);
        }
        vertical = next_vertical;
        horizontal = next_horizontal;
    }
}

const fn scrollbar_content_length(content_length: usize, viewport_length: usize) -> usize {
    max_scroll_position(content_length, viewport_length).saturating_add(1)
}

const fn max_scroll_position(content_length: usize, viewport_length: usize) -> usize {
    content_length.saturating_sub(viewport_length)
}

fn begin_style(position: usize) -> ratatui::style::Style {
    if position == 0 {
        theme::scrollbar_track_style()
    } else {
        theme::scrollbar_thumb_style()
    }
}

fn end_style(
    position: usize,
    content_length: usize,
    viewport_length: usize,
) -> ratatui::style::Style {
    if position.saturating_add(viewport_length) >= content_length {
        theme::scrollbar_track_style()
    } else {
        theme::scrollbar_thumb_style()
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::fits_exactly(4, 10, (false, false))]
    #[case::tall_only(5, 5, (true, false))]
    #[case::wide_only(3, 11, (false, true))]
    #[case::tall_bar_makes_width_overflow(5, 10, (true, true))]
    #[case::wide_bar_makes_height_overflow(4, 11, (true, true))]
    fn reserves_the_bars_that_the_content_needs(
        #[case] line_count: usize,
        #[case] line_width: usize,
        #[case] expected: (bool, bool),
    ) {
        let area = Rect::new(0, 0, 10, 4);

        assert_eq!(reservations(line_count, line_width, area), expected);
    }
}
