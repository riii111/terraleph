use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::Paragraph,
};

use super::{
    MatrixCell, MatrixView,
    view::{Row, address_widths, row_lead},
};
use crate::app::{
    environments::{
        EnvironmentSession,
        comparison::{CellState, ComparisonScope, DifferenceReason},
    },
    plan::{PlanAction, ResourceChangeKind},
};
use crate::ui::{
    primitives::atoms::{scroll, scrollbar},
    theme,
};

const WHY_WIDTH: usize = 7;
const MIN_CELL_WIDTH: usize = 9;
const COLUMN_GAP: usize = 1;
const MIN_ADDRESS_WIDTH: usize = 12;
const MAX_ADDRESS_WIDTH: usize = 52;
const DIVIDER: &str = "│ ";
const DIVIDER_WIDTH: usize = 2;
const WIDE_WIDTH: u16 = 64;

pub(crate) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &EnvironmentSession,
    view: &mut MatrixView,
    show_same_change_toggle: bool,
) {
    if area.width < 30 || area.height < 5 {
        frame.render_widget(
            Paragraph::new("Resize to view the matrix. ? help   q quit")
                .style(theme::overview_muted_style()),
            area,
        );
        return;
    }
    let wide = area.width >= WIDE_WIDTH;
    let address_width = address_width(area, view);
    let column_widths = column_widths(state, view);
    let column_budget = usize::from(area.width)
        .saturating_sub(address_width + WHY_WIDTH + divider_width(wide))
        .saturating_sub(if wide { DIVIDER_WIDTH } else { 0 });
    let columns = visible_columns(view, &column_widths, column_budget);

    render_column_headers(frame, area, state, view, &columns, address_width);
    if wide {
        render_header_rule(frame, area, &columns, address_width);
    }
    render_content(
        frame,
        area,
        state,
        view,
        &columns,
        address_width,
        show_same_change_toggle,
    );
}

fn render_content(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &EnvironmentSession,
    view: &mut MatrixView,
    columns: &[(usize, usize)],
    address_width: usize,
    show_same_change_toggle: bool,
) {
    let legend = symbol_legend(area.width);
    let mut body = Rect::new(
        area.x,
        area.y.saturating_add(2),
        area.width,
        area.height
            .saturating_sub(2)
            .saturating_sub(u16::try_from(legend.len()).unwrap_or(u16::MAX)),
    );
    let body_height = usize::from(body.height);
    let wide = area.width >= WIDE_WIDTH;
    let content = |width| {
        content_lines(
            view,
            state,
            width,
            wide,
            columns,
            address_width,
            show_same_change_toggle,
        )
    };
    let (mut lines, mut selected_lines) = content(body.width);
    // An overflowing list gives its last column to the scrollbar so no text sits under it.
    if lines.len() > body_height {
        body.width = body.width.saturating_sub(1);
        (lines, selected_lines) = content(body.width);
    }
    let max_vertical = lines.len().saturating_sub(body_height);
    view.vertical = view.vertical.min(max_vertical);
    if let Some(selected_lines) = selected_lines.filter(|_| body_height > 0) {
        view.vertical = scroll::offset_showing_range(view.vertical, selected_lines, body_height);
    }
    let content_length = lines.len();
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::overview_text_style())
            .scroll((u16::try_from(view.vertical).unwrap_or(u16::MAX), 0)),
        body,
    );
    scrollbar::render_vertical(
        frame,
        Rect::new(area.x, body.y, area.width, body.height),
        content_length,
        body_height,
        view.vertical,
    );
    for (index, line) in legend.into_iter().enumerate() {
        let y = area
            .bottom()
            .saturating_sub(u16::try_from(legend_height(area.width)).unwrap_or(u16::MAX))
            .saturating_add(u16::try_from(index).unwrap_or(u16::MAX));
        frame.render_widget(Paragraph::new(line), Rect::new(area.x, y, area.width, 1));
    }
}

fn content_lines(
    view: &MatrixView,
    state: &EnvironmentSession,
    width: u16,
    wide: bool,
    columns: &[(usize, usize)],
    address_width: usize,
    show_same_change_toggle: bool,
) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    let names = view
        .environments
        .iter()
        .map(|environment| state.plans()[*environment].display_name())
        .collect::<Vec<_>>();
    let mut lines = Vec::new();
    let mut selected_lines = None;
    let mut section = None;
    let mut summary_seen = false;
    for row in &view.rows {
        let selected = row.selection.as_ref() == view.selected.as_ref();
        if let Some(summary) = &row.summary {
            if section.is_some() {
                lines.push(Line::default());
            }
            section = Some(false);
            summary_seen = true;
            let summary_start = lines.len();
            lines.push(summary_line(
                summary,
                selected,
                view.same_expanded,
                show_same_change_toggle,
                width,
            ));
            lines.extend(summary_note_lines(summary, width));
            if selected {
                selected_lines = Some((summary_start, lines.len() - 1));
            }
            continue;
        }
        if row.difference.is_some() && !row.child && section != Some(true) {
            if section.is_some() {
                lines.push(Line::default());
            }
            section = Some(true);
            lines.push(Line::styled(
                if view.environments.len() == 1 {
                    "Changes"
                } else {
                    "Differs across envs"
                },
                theme::overview_section_heading_style(),
            ));
        } else if row.difference.is_none() && !row.child && !summary_seen && section != Some(false)
        {
            if section.is_some() {
                lines.push(Line::default());
            }
            section = Some(false);
            lines.push(Line::styled(
                same_section_title(view, state),
                theme::overview_section_heading_style(),
            ));
        }
        let row_line_index = lines.len();
        if selected {
            selected_lines = Some((row_line_index, row_line_index));
        }
        lines.push(row_line(
            row,
            view,
            selected,
            columns,
            address_width,
            wide,
            &fitted_why(row, &names, width, wide, columns, address_width),
        ));
        if !wide && row.has_unknown {
            let note_line = lines.len();
            lines.push(unknown_note_line(row, view.environments.len() > 1));
            if selected {
                selected_lines = Some((row_line_index, note_line));
            }
        }
    }
    if lines.is_empty() {
        let waiting = view
            .overview
            .as_ref()
            .is_none_or(|overview| matches!(overview.scope, ComparisonScope::Waiting));
        lines.push(Line::from(if waiting {
            "Waiting for environment plans."
        } else {
            "No matching resource changes. v opens the full plan."
        }));
    }

    (lines, selected_lines)
}

/// Returns the rows the matrix needs inside a pane of `width` columns: the column headers, every
/// body line, and the legend.
pub(crate) fn body_height(view: &MatrixView, width: u16) -> usize {
    2 + content_line_count(view, width) + legend_height(width)
}

// Counts what `content_lines` builds without building it, so the layout can size the pane before
// the rows are drawn.
fn content_line_count(view: &MatrixView, width: u16) -> usize {
    let wide = width >= WIDE_WIDTH;
    let mut count = 0;
    let mut section = None;
    let mut summary_seen = false;
    for row in &view.rows {
        if let Some(summary) = &row.summary {
            count += usize::from(section.is_some()) + 1 + summary_note_lines(summary, width).len();
            section = Some(false);
            summary_seen = true;
            continue;
        }
        if row.difference.is_some() && !row.child && section != Some(true) {
            count += usize::from(section.is_some()) + 1;
            section = Some(true);
        } else if row.difference.is_none() && !row.child && !summary_seen && section != Some(false)
        {
            count += usize::from(section.is_some()) + 1;
            section = Some(false);
        }
        count += 1 + usize::from(!wide && row.has_unknown);
    }
    count.max(1)
}

fn same_section_title(view: &MatrixView, state: &EnvironmentSession) -> &'static str {
    let filtered = view.environments.len() != state.plans().len();
    let partial = view
        .overview
        .as_ref()
        .is_none_or(|overview| !matches!(overview.scope, ComparisonScope::All));
    if view.environments.len() == 1 {
        "Changes"
    } else if filtered && partial {
        "Same change across selected envs (Ready only)"
    } else if filtered {
        "Same change across selected envs"
    } else if partial {
        "Same change across envs (Ready only)"
    } else {
        "Same change across envs"
    }
}

fn render_column_headers(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &EnvironmentSession,
    view: &MatrixView,
    columns: &[(usize, usize)],
    address_width: usize,
) {
    let mut header = vec![Span::styled("Address", theme::overview_text_style())];
    header.push(Span::styled(
        " ".repeat(address_width.saturating_sub(7)),
        theme::overview_text_style(),
    ));
    let wide = area.width >= WIDE_WIDTH;
    if wide {
        header.push(Span::styled(DIVIDER, theme::overview_muted_style()));
    }
    for &(column, column_width) in columns {
        let environment = view.environments[column];
        let label = state.plans()[environment].display_name();
        let (label, padding) = fit_parts(&label, column_width.saturating_sub(COLUMN_GAP), false);
        header.push(Span::styled(
            format!("{label}{} ", " ".repeat(padding)),
            theme::overview_text_style(),
        ));
    }
    header.push(why_separator(wide));
    header.push(Span::styled("why", theme::overview_text_style()));
    header.push(Span::styled(
        " ".repeat(WHY_WIDTH.saturating_sub(3)),
        theme::overview_text_style(),
    ));
    frame.render_widget(
        Paragraph::new(Line::from(header)).style(theme::overview_text_style()),
        Rect::new(area.x, area.y, area.width, 1),
    );
}

fn render_header_rule(
    frame: &mut Frame<'_>,
    area: Rect,
    columns: &[(usize, usize)],
    address_width: usize,
) {
    let cells_width: usize = columns.iter().map(|(_, width)| width).sum();
    let rest =
        usize::from(area.width).saturating_sub(address_width + cells_width + DIVIDER_WIDTH * 2);
    let rule = format!(
        "{}┼─{}┼─{}",
        "─".repeat(address_width),
        "─".repeat(cells_width),
        "─".repeat(rest),
    );
    frame.render_widget(
        Paragraph::new(Line::styled(rule, theme::overview_muted_style())),
        Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
    );
}

const fn divider_width(wide: bool) -> usize {
    if wide { DIVIDER_WIDTH } else { COLUMN_GAP }
}

fn why_separator(wide: bool) -> Span<'static> {
    if wide {
        Span::styled(DIVIDER, theme::overview_muted_style())
    } else {
        Span::styled(" ", theme::overview_text_style())
    }
}

fn address_width(area: Rect, view: &MatrixView) -> usize {
    let (visible_content_width, visible_unknown_width) =
        address_widths(&view.rows, view.environments.len() > 1);
    let content_width = view.address_content_width.max(visible_content_width);
    let unknown_label_width = if area.width >= WIDE_WIDTH {
        view.unknown_address_width.max(visible_unknown_width)
    } else {
        0
    };
    let content_width = content_width.max(unknown_label_width);
    let max_width = usize::from(area.width)
        .saturating_sub(WHY_WIDTH + MIN_CELL_WIDTH + COLUMN_GAP * 2)
        .clamp(MIN_ADDRESS_WIDTH, MAX_ADDRESS_WIDTH);
    let address_width = content_width.clamp(MIN_ADDRESS_WIDTH, max_width);
    address_width.max(unknown_label_width)
}

fn column_widths(state: &EnvironmentSession, view: &MatrixView) -> Vec<usize> {
    view.environments
        .iter()
        .enumerate()
        .map(|(column, environment)| {
            let plan = &state.plans()[*environment];
            let widest_cell = view
                .rows
                .iter()
                .filter(|row| row.summary.is_none())
                .map(|row| Line::from(cell_text(&row.cells[column], row.group.is_some())).width())
                .max()
                .unwrap_or(0);
            (Line::from(plan.display_name().as_str()).width() + 2)
                .max(widest_cell)
                .max(MIN_CELL_WIDTH)
                + COLUMN_GAP * 3
        })
        .collect()
}

fn visible_columns(view: &mut MatrixView, widths: &[usize], budget: usize) -> Vec<(usize, usize)> {
    if widths.is_empty() {
        view.first_column = 0;
        return Vec::new();
    }
    if budget == 0 {
        return Vec::new();
    }

    let mut first = view.first_column.min(widths.len() - 1);
    if !view.manual_horizontal_scroll
        && let Some(environment) = view.selected_environment
        && let Some(selected) = view.selected_column(environment)
        && !visible_columns_from(first, widths, budget)
            .iter()
            .any(|(column, _)| *column == selected)
    {
        first = if selected < first {
            selected
        } else {
            first_column_showing_selection(first, selected, widths, budget)
        };
    }
    view.first_column = first;
    visible_columns_from(first, widths, budget)
}

fn first_column_showing_selection(
    current: usize,
    selected: usize,
    widths: &[usize],
    budget: usize,
) -> usize {
    let mut first = current.saturating_add(1);
    let mut last = selected;
    while first < last {
        let middle = first + last.saturating_sub(first) / 2;
        if visible_columns_from(middle, widths, budget)
            .iter()
            .any(|(column, _)| *column == selected)
        {
            last = middle;
        } else {
            first = middle.saturating_add(1);
        }
    }
    first
}

fn visible_columns_from(first: usize, widths: &[usize], budget: usize) -> Vec<(usize, usize)> {
    let mut last = first;
    let mut used = 0;
    while last < widths.len() {
        let remaining = budget.saturating_sub(used);
        if remaining == 0 {
            break;
        }
        if last > first && widths[last] > remaining {
            break;
        }
        let width = widths[last].min(remaining);
        used += width;
        last += 1;
        if width < widths[last - 1] {
            break;
        }
    }
    (first..last)
        .map(|index| {
            let width = if index == first {
                widths[index].min(budget)
            } else {
                widths[index]
            };
            (index, width)
        })
        .collect()
}

fn symbol_legend(width: u16) -> Vec<Line<'static>> {
    if width < 50 {
        vec![
            Line::styled("blank: absent   .: unchanged", theme::overview_text_style()),
            Line::styled("?: plan unavailable", theme::overview_text_style()),
        ]
    } else {
        vec![Line::styled(
            "blank: absent   .: unchanged   ?: plan unavailable",
            theme::overview_text_style(),
        )]
    }
}

fn legend_height(width: u16) -> usize {
    symbol_legend(width).len()
}

fn row_line(
    row: &Row,
    view: &MatrixView,
    selected: bool,
    columns: &[(usize, usize)],
    address_width: usize,
    wide: bool,
    why: &str,
) -> Line<'static> {
    let expanded = row
        .group
        .as_ref()
        .is_some_and(|id| view.expanded.contains(id));
    let lead = row_lead(row, expanded, view.environments.len() > 1);
    let address_budget = address_width.saturating_sub(2 + Line::from(lead.as_str()).width());
    let unknown_label = (row.has_unknown && wide).then_some("[unknown values]");
    let label_width = unknown_label.map_or(0, |label| Line::from(Span::raw(label)).width() + 2);
    let address_text_budget = address_budget.saturating_sub(label_width);
    let (address, address_padding) = fit_parts(&row.address, address_text_budget, true);
    let mut spans = vec![
        Span::styled(
            if selected { ">" } else { " " },
            theme::overview_text_style(),
        ),
        Span::raw(" "),
        Span::styled(lead, theme::overview_text_style()),
        Span::styled(
            address,
            if selected {
                theme::overview_text_style().add_modifier(Modifier::UNDERLINED)
            } else {
                theme::overview_text_style()
            },
        ),
        Span::raw(" ".repeat(address_padding)),
    ];
    if let Some(label) = unknown_label {
        spans.push(Span::styled(
            format!(" {label} "),
            theme::overview_text_style(),
        ));
    }
    if wide {
        spans.push(Span::styled(DIVIDER, theme::overview_muted_style()));
    }
    for &(index, column_width) in columns {
        let cell = &row.cells[index];
        let (text, padding) = fit_parts(
            &cell_text(cell, row.group.is_some()),
            column_width.saturating_sub(COLUMN_GAP),
            false,
        );
        spans.push(Span::styled(
            format!("{text}{} ", " ".repeat(padding)),
            cell_style(cell),
        ));
    }
    spans.push(why_separator(wide));
    spans.push(Span::styled(why.to_owned(), theme::overview_text_style()));
    Line::from(spans)
}

fn fitted_why(
    row: &Row,
    names: &[String],
    width: u16,
    wide: bool,
    columns: &[(usize, usize)],
    address_width: usize,
) -> String {
    let leading_divider = if wide { DIVIDER_WIDTH } else { 0 };
    let available = usize::from(width)
        .saturating_sub(address_width + leading_divider + divider_width(wide))
        .saturating_sub(columns.iter().map(|(_, column_width)| column_width).sum())
        .max(WHY_WIDTH);
    fit(&why_text(row, names), available, false)
}

fn why_text(row: &Row, names: &[String]) -> String {
    match row.difference {
        Some(DifferenceReason::Action) => "action".to_owned(),
        Some(DifferenceReason::Attrs) => "attrs".to_owned(),
        Some(DifferenceReason::Missing) => missing_reason(row, names),
        Some(DifferenceReason::Unknown) => "unknown".to_owned(),
        Some(DifferenceReason::Value) => "value".to_owned(),
        None => String::new(),
    }
}

fn missing_reason(row: &Row, names: &[String]) -> String {
    let mut present = Vec::new();
    let mut absent = Vec::new();
    for (cell, name) in row.cells.iter().zip(names) {
        match cell.state {
            CellState::Change { .. } | CellState::NoOp => present.push(name.as_str()),
            CellState::Missing => absent.push(name.as_str()),
            CellState::Unavailable => {}
        }
    }
    match (present.as_slice(), absent.as_slice()) {
        ([only], _) => format!("only in {only}"),
        (_, [gap]) => format!("not in {gap}"),
        _ => format!("in {}/{} envs", present.len(), present.len() + absent.len()),
    }
}

fn unknown_note_line(row: &Row, under_summary: bool) -> Line<'static> {
    let lead = Line::from(row_lead(row, false, under_summary)).width();
    Line::from(vec![
        Span::raw(" ".repeat(2 + lead)),
        Span::styled("[unknown values]", theme::overview_text_style()),
    ])
}

fn summary_line(
    summary: &super::view::SameChangeSummary,
    selected: bool,
    expanded: bool,
    show_toggle_hint: bool,
    width: u16,
) -> Line<'static> {
    // Creates and updates are omitted because they read like resource totals, which [1] already shows.
    let mut counts = Vec::new();
    push_count(
        &mut counts,
        summary.actions.deletes,
        "-",
        theme::overview_total_destroy_style(),
    );
    if summary.actions.replacements > 0 {
        counts.push(Span::styled(
            format!(" {} replace", summary.actions.replacements),
            theme::overview_total_replace_style(),
        ));
    }
    push_count(
        &mut counts,
        summary.actions.unknown,
        "?",
        theme::overview_warning_style(),
    );

    let rows = if summary.rows == 1 {
        "pattern"
    } else {
        "patterns"
    };
    let colon = if counts.is_empty() { "" } else { ":" };
    let label = if width < 64 {
        format!("Same: {} {rows}{colon}", summary.rows)
    } else {
        format!("Same change across envs: {} {rows}{colon}", summary.rows)
    };
    let mut spans = vec![
        Span::styled(
            if selected { ">" } else { " " },
            theme::overview_text_style(),
        ),
        Span::raw(" "),
        Span::styled(
            if expanded { "▾" } else { "▸" },
            theme::overview_section_heading_style(),
        ),
        Span::raw(" "),
        Span::styled(label, theme::overview_section_heading_style()),
    ];
    spans.extend(counts);
    let mut line = Line::from(spans);
    let hint = if expanded {
        "  Space collapse"
    } else {
        "  Space expand"
    };
    if show_toggle_hint
        && line.width().saturating_add(Line::from(hint).width()) <= usize::from(width)
    {
        line.push_span(Span::styled(hint, theme::overview_text_style()));
    }
    line
}

fn summary_note_lines(summary: &super::view::SameChangeSummary, width: u16) -> Vec<Line<'static>> {
    let mut notes = Vec::new();
    if summary.has_unknown {
        notes.push("[unknown values]");
    }
    if summary.instance_counts_differ {
        notes.push(if width < 64 {
            "counts differ"
        } else {
            "instance counts differ"
        });
    }
    let note_line = |text: String| {
        Line::from(vec![
            Span::raw("    "),
            Span::styled(text, theme::overview_text_style()),
        ])
    };
    let joined = notes.join(" · ");
    if notes.is_empty() {
        Vec::new()
    } else if 4 + Line::from(joined.as_str()).width() <= usize::from(width) {
        vec![note_line(joined)]
    } else {
        notes
            .into_iter()
            .map(|note| note_line(note.to_owned()))
            .collect()
    }
}

fn push_count(
    spans: &mut Vec<Span<'static>>,
    count: usize,
    label: &str,
    style: ratatui::style::Style,
) {
    if count > 0 {
        spans.push(Span::styled(format!(" {label}{count}"), style));
    }
}

fn cell_style(cell: &MatrixCell) -> ratatui::style::Style {
    match &cell.state {
        CellState::Change { kind, .. } => match kind {
            ResourceChangeKind::Create => theme::overview_total_add_style(),
            ResourceChangeKind::Update => theme::overview_total_update_style(),
            ResourceChangeKind::Delete => theme::overview_total_destroy_style(),
            ResourceChangeKind::Replace => theme::overview_total_replace_style(),
            ResourceChangeKind::Read
            | ResourceChangeKind::Move
            | ResourceChangeKind::Import
            | ResourceChangeKind::NoOp
            | ResourceChangeKind::Unknown
            | ResourceChangeKind::Unsupported => theme::overview_text_style(),
        },
        CellState::Unavailable => theme::overview_muted_style(),
        CellState::Missing | CellState::NoOp => theme::overview_text_style(),
    }
}

fn cell_text(cell: &MatrixCell, grouped: bool) -> String {
    let symbol = match &cell.state {
        CellState::NoOp => ".",
        CellState::Missing => "",
        CellState::Unavailable => "?",
        CellState::Change { actions, kind } => match kind {
            ResourceChangeKind::Create => "+",
            ResourceChangeKind::Update => "~",
            ResourceChangeKind::Delete => "-",
            ResourceChangeKind::Replace
                if actions.starts_with(&[PlanAction::Create, PlanAction::Delete]) =>
            {
                "+/-"
            }
            ResourceChangeKind::Replace => "-/+",
            ResourceChangeKind::Read => "read",
            ResourceChangeKind::Move => "move",
            ResourceChangeKind::Import => "import",
            ResourceChangeKind::NoOp => ".",
            ResourceChangeKind::Unknown | ResourceChangeKind::Unsupported => "?",
        },
    };
    if grouped && matches!(cell.state, CellState::Change { .. }) {
        if cell.members.is_empty() {
            String::new()
        } else {
            format!("{symbol} {}", cell.members.len())
        }
    } else {
        symbol.to_owned()
    }
}

fn fit(text: &str, width: usize, suffix: bool) -> String {
    let (value, padding) = fit_parts(text, width, suffix);
    format!("{value}{}", " ".repeat(padding))
}

fn fit_parts(text: &str, width: usize, suffix: bool) -> (String, usize) {
    let mut value = text.to_owned();
    if Line::from(value.as_str()).width() > width {
        let limit = width.saturating_sub(1);
        while Line::from(value.as_str()).width() > limit {
            if suffix {
                value.remove(0);
            } else {
                value.pop();
            }
        }
        if width > 0 {
            value = if suffix {
                format!("…{value}")
            } else {
                format!("{value}…")
            };
        }
    }
    let padding = width.saturating_sub(Line::from(value.as_str()).width());
    (value, padding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::features::overview::OverviewInput;
    use crate::ui::test_support::{buffer_text, render_to_buffer};
    use ratatui::style::Color;
    use rstest::rstest;

    fn matrix_view(first_column: usize) -> MatrixView {
        let mut view = MatrixView::default();
        view.first_column = first_column;
        view.environments = vec![0, 1, 2];
        view.selected_environment = Some(2);
        view
    }

    #[rstest]
    #[case::narrow(40)]
    #[case::just_below_wide(63)]
    #[case::wide(64)]
    #[case::roomy(120)]
    fn counted_body_lines_match_the_lines_that_are_built(#[case] width: u16) {
        use super::super::view::{ChangeCounts, SameChangeSummary};

        let row = |difference: Option<DifferenceReason>, child: bool, has_unknown: bool| Row {
            address: "terraform_data.server[*]".to_owned(),
            group: None,
            group_members: Vec::new(),
            selection: None,
            child,
            cells: Vec::new(),
            difference,
            summary: None,
            has_unknown,
        };
        let summary = |has_unknown, instance_counts_differ| Row {
            summary: Some(SameChangeSummary {
                rows: 2,
                actions: ChangeCounts::default(),
                has_unknown,
                instance_counts_differ,
            }),
            ..row(None, false, false)
        };
        let mut view = matrix_view(0);
        view.environments = Vec::new();
        let state = EnvironmentSession::new(Vec::new(), false);
        let layouts = [
            vec![],
            vec![row(Some(DifferenceReason::Value), false, true)],
            vec![row(None, false, false), row(None, true, true)],
            vec![
                row(Some(DifferenceReason::Action), false, false),
                row(Some(DifferenceReason::Value), true, true),
                summary(true, true),
                row(None, false, true),
            ],
            vec![summary(false, false), row(None, false, false)],
        ];

        for rows in layouts {
            view.rows = rows;
            let wide = width >= WIDE_WIDTH;
            let built = content_lines(&view, &state, width, wide, &[], 0, false).0;

            assert_eq!(
                content_line_count(&view, width),
                built.len(),
                "{} rows at width {width}",
                view.rows.len()
            );
        }
    }

    #[test]
    fn unknown_group_note_is_kept_when_the_address_column_is_narrow() {
        let row = Row {
            address: "terraform_data.server[*]".to_owned(),
            group: None,
            group_members: Vec::new(),
            selection: None,
            child: false,
            cells: Vec::new(),
            difference: None,
            summary: None,
            has_unknown: true,
        };
        let mut view = matrix_view(0);
        view.rows.push(row);
        let address_width = address_width(Rect::new(0, 0, 40, 16), &view);

        assert!(address_width >= 19);
        let line = row_line(&view.rows[0], &view, false, &[], address_width, false, "").to_string();

        assert!(line.contains("server[*]"), "{line}");
        let note = unknown_note_line(&view.rows[0], false).to_string();
        assert!(note.contains("[unknown values]"), "{note}");
    }

    #[test]
    fn one_line_body_keeps_the_selected_unknown_address_visible() {
        let selection = super::super::view::SelectionKey::SameSummary;
        let mut view = MatrixView::default();
        view.selected = Some(selection.clone());
        view.rows.push(Row {
            address: "terraform_data.server[*]".to_owned(),
            group: None,
            group_members: Vec::new(),
            selection: Some(selection),
            child: false,
            cells: Vec::new(),
            difference: None,
            summary: None,
            has_unknown: true,
        });
        let state = EnvironmentSession::new(Vec::new(), false);
        let output = render_to_buffer((40, 5), |frame| {
            render(frame, frame.area(), &state, &mut view, false);
        });
        let text = buffer_text(&output);

        assert!(text.contains("server[*]"), "{text}");
        assert!(!text.contains("[unknown values]"), "{text}");
    }

    #[test]
    fn collapsed_same_change_summary_keeps_unknown_group_note() {
        let summary = super::super::view::SameChangeSummary {
            rows: 1,
            actions: super::super::view::ChangeCounts::default(),
            has_unknown: true,
            instance_counts_differ: false,
        };

        let notes = summary_note_lines(&summary, 40)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        assert_eq!(notes, ["    [unknown values]"]);
    }

    #[test]
    fn same_change_summary_counts_patterns_and_keeps_only_warning_actions() {
        let routine = super::super::view::SameChangeSummary {
            rows: 2,
            actions: super::super::view::ChangeCounts::default(),
            has_unknown: false,
            instance_counts_differ: false,
        };
        let destructive = super::super::view::SameChangeSummary {
            rows: 3,
            actions: super::super::view::ChangeCounts {
                deletes: 1,
                replacements: 1,
                unknown: 0,
            },
            has_unknown: false,
            instance_counts_differ: false,
        };

        let routine = summary_line(&routine, false, false, false, 80).to_string();
        let destructive = summary_line(&destructive, false, false, false, 80).to_string();

        assert!(
            routine.ends_with("Same change across envs: 2 patterns"),
            "{routine}"
        );
        assert!(
            destructive.ends_with("Same change across envs: 3 patterns: -1 1 replace"),
            "{destructive}"
        );
    }

    #[test]
    fn same_change_summary_notes_unknown_values_and_instance_count_gaps() {
        let summary = super::super::view::SameChangeSummary {
            rows: 2,
            actions: super::super::view::ChangeCounts::default(),
            has_unknown: true,
            instance_counts_differ: true,
        };

        let wide = summary_line(&summary, false, false, false, 80).to_string();
        let notes = |width| {
            summary_note_lines(&summary, width)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
        };
        let narrow = summary_line(&summary, false, false, false, 40).to_string();

        assert!(
            wide.ends_with("Same change across envs: 2 patterns"),
            "{wide}"
        );
        assert_eq!(notes(80), ["    [unknown values] · instance counts differ"]);
        assert!(narrow.ends_with("Same: 2 patterns"), "{narrow}");
        assert_eq!(notes(40), ["    [unknown values] · counts differ"]);
        assert_eq!(notes(30), ["    [unknown values]", "    counts differ"]);
    }

    #[test]
    fn same_change_summary_shows_its_expansion_state_without_selection() {
        let summary = super::super::view::SameChangeSummary {
            rows: 1,
            actions: super::super::view::ChangeCounts::default(),
            has_unknown: false,
            instance_counts_differ: false,
        };

        let collapsed = summary_line(&summary, false, false, false, 80);
        let expanded = summary_line(&summary, false, true, false, 80);

        assert!(
            collapsed
                .to_string()
                .starts_with("  ▸ Same change across envs")
        );
        assert!(
            expanded
                .to_string()
                .starts_with("  ▾ Same change across envs")
        );
        assert!(!collapsed.to_string().contains("Space"));
        assert!(!expanded.to_string().contains("Space"));
        assert_eq!(collapsed.spans[2].style.fg, Some(Color::Reset));
        assert!(
            collapsed.spans[2]
                .style
                .add_modifier
                .contains(Modifier::BOLD)
        );
        assert_eq!(collapsed.spans[2].style.bg, Some(Color::Reset));
    }

    #[test]
    fn same_change_summary_only_adds_a_complete_hint_when_it_fits() {
        let summary = super::super::view::SameChangeSummary {
            rows: 1,
            actions: super::super::view::ChangeCounts::default(),
            has_unknown: false,
            instance_counts_differ: false,
        };

        let fits = summary_line(&summary, true, false, true, 80);
        let too_narrow = summary_line(&summary, true, false, true, 28);

        assert!(fits.to_string().ends_with("Space expand"));
        assert_eq!(fits.spans.last().unwrap().style.fg, Some(Color::Reset));
        assert!(!too_narrow.to_string().contains("Space"));
    }

    #[test]
    fn selected_left_column_becomes_the_start_when_it_is_hidden() {
        let mut view = matrix_view(2);
        view.selected_environment = Some(0);

        let columns = visible_columns(&mut view, &[12, 12, 12], 25);

        assert_eq!(view.first_column, 0);
        assert_eq!(columns, [(0, 12), (1, 12)]);
    }

    #[test]
    fn excluded_environment_keeps_the_current_start() {
        let mut view = MatrixView::default();
        view.first_column = 1;
        view.environments = vec![0, 1];
        view.selected_environment = Some(2);

        let columns = visible_columns(&mut view, &[12, 12], 12);

        assert_eq!(view.first_column, 1);
        assert_eq!(columns, [(1, 12)]);
    }

    #[test]
    fn manual_scroll_keeps_its_start_when_the_selected_column_is_hidden() {
        let mut view = matrix_view(0);
        view.apply(OverviewInput::Right, 1);
        view.apply(OverviewInput::Left, 1);

        let columns = visible_columns(&mut view, &[12, 12, 12], 12);

        assert_eq!(view.first_column, 0);
        assert_eq!(columns, [(0, 12)]);
    }

    #[test]
    fn a_narrower_view_keeps_the_selected_column_visible_after_a_wide_render() {
        let mut view = matrix_view(0);

        let wide = visible_columns(&mut view, &[12, 12, 12], 36);
        let narrow = visible_columns(&mut view, &[12, 12, 12], 25);

        assert_eq!(wide, [(0, 12), (1, 12), (2, 12)]);
        assert_eq!(view.first_column, 1);
        assert_eq!(narrow, [(1, 12), (2, 12)]);
    }
}
