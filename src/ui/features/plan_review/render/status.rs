use ratatui::{
    Frame,
    style::Style,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::app::{review::ResourcePosition, session::ReviewSessionState};
use crate::ui::features::plan_review::PlanReviewViewState;
use crate::ui::primitives::atoms::separator;
use crate::ui::shell::context::{display_width, truncate_middle};
use crate::ui::theme;

use super::{content::PlanContent, filter_active, layout::PlanReviewLayout};

pub(super) fn render_status(
    frame: &mut Frame<'_>,
    layout: &PlanReviewLayout,
    state: &ReviewSessionState,
    view: &PlanReviewViewState,
) {
    let (line, horizontal) = if filter_active(view.searching(), state) {
        filter_status_line(view, state, layout.status().width)
    } else {
        (resource_status_line(layout, state, view.scroll().0), 0)
    };
    frame.render_widget(
        Paragraph::new(line)
            .style(theme::body_style())
            .scroll((0, horizontal)),
        layout.status(),
    );
    frame.render_widget(
        separator::render(layout.separator().width),
        layout.separator(),
    );
}

/// Returns the resource block being read at `row`, the first visible plan row. Blank rows between
/// blocks belong to the block above, so the first line with text picks the block.
pub(super) fn top_resource<'a>(
    state: &'a ReviewSessionState,
    content: &PlanContent,
    row: usize,
) -> Option<ResourcePosition<'a>> {
    let document = state.review().document();
    let line = (row..)
        .map_while(|row| content.source_line(row))
        .find(|line| !document.line(*line).trim().is_empty())?;
    document.resource_at_line(line)
}

fn resource_status_line(
    layout: &PlanReviewLayout,
    state: &ReviewSessionState,
    row: usize,
) -> Line<'static> {
    let Some(resource) = top_resource(state, layout.content(), row) else {
        return Line::default();
    };
    let counter = format!("  {}/{}", resource.number(), resource.total());
    let address_width = usize::from(layout.status().width).saturating_sub(display_width(&counter));
    Line::from(vec![
        Span::styled(
            truncate_middle(resource.address(), address_width),
            theme::plan_resource_header_style(),
        ),
        Span::styled(counter, theme::header_label_style()),
    ])
}

fn filter_status_line(
    view: &PlanReviewViewState,
    state: &ReviewSessionState,
    width: u16,
) -> (Line<'static>, u16) {
    let searching = view.searching();
    let (query_line, cursor) = if searching {
        let Some((line, cursor)) = search_query_line(view) else {
            return (Line::default(), 0);
        };
        (line, Some(cursor))
    } else {
        (
            Line::from(vec![
                Span::styled("/", theme::secondary_style()),
                Span::styled(
                    state.review().search_query().to_owned(),
                    theme::secondary_style(),
                ),
            ]),
            None,
        )
    };
    let prefix = Span::styled("Filter: ", theme::secondary_style());
    let prefix_width = Line::from(prefix.clone()).width();
    let mut line = Line::from(prefix);
    line.extend(query_line.spans);
    let horizontal = cursor.map_or(0, |(start, end)| {
        horizontal_offset(
            prefix_width + start,
            prefix_width + end,
            line.width(),
            width,
        )
    });
    (line, horizontal)
}

pub(super) fn search_query_line(
    view: &PlanReviewViewState,
) -> Option<(Line<'static>, (usize, usize))> {
    let query = view.search_query()?;
    let cursor = view.search_cursor()?;
    let before = query[..cursor].to_owned();
    let after = query[cursor..].to_owned();
    let (cursor_grapheme, after_cursor) = next_grapheme(&after);
    let line = Line::from(vec![
        Span::styled("/", theme::accent_style()),
        Span::styled(before.clone(), theme::body_style()),
        Span::styled(cursor_grapheme.clone(), theme::search_cursor_style()),
        Span::styled(after_cursor, theme::body_style()),
    ]);
    let cursor_start = 1 + Line::from(before).width();
    let cursor_end = cursor_start + Line::from(cursor_grapheme).width().max(1);
    Some((line, (cursor_start, cursor_end)))
}

fn next_grapheme(text: &str) -> (String, String) {
    let line = Line::from(text);
    let mut graphemes = line.styled_graphemes(Style::default());
    let Some(grapheme) = graphemes.next() else {
        return (" ".to_owned(), String::new());
    };
    let cursor = grapheme.symbol.len();
    (grapheme.symbol.to_owned(), text[cursor..].to_owned())
}

pub(super) fn horizontal_offset(start: usize, end: usize, line_width: usize, width: u16) -> u16 {
    let width = usize::from(width);
    if width == 0 {
        return 0;
    }
    let offset = if end.saturating_sub(start) >= width {
        start
    } else {
        end.saturating_sub(width)
    };
    u16::try_from(offset.min(line_width.saturating_sub(width))).unwrap_or(u16::MAX)
}
