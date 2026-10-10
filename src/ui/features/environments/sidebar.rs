use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::shell;
use crate::{
    app::{
        environments::{EnvironmentPlan, EnvironmentState},
        plan::PlanSummary,
    },
    ui::{primitives::atoms::focus_mark, shell::context::take_from_start, theme},
};

#[derive(Clone, Copy)]
struct CountWidths {
    additions: usize,
    updates: usize,
    deletions: usize,
    replacements: usize,
}

pub(crate) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    plans: &[EnvironmentPlan],
    selected: usize,
    compared: &[usize],
    focused: bool,
    walk_limit: Option<usize>,
) {
    let focus_style = theme::relation_frame_style(focused);
    let title = Line::from(vec![
        focus_mark::render(focused),
        Span::styled("[1] Envs", theme::overview_pane_title_style()),
    ]);
    let block = Block::new()
        .borders(Borders::ALL)
        .title(title)
        .border_style(focus_style)
        .style(theme::overview_text_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 || plans.is_empty() {
        return;
    }

    let widths = count_widths(plans);
    let wrap_counts = count_width(widths).saturating_add(4) > usize::from(inner.width);
    let mut lines = Vec::new();
    let mut selected_range = None;
    let mut visual_offset = 0_usize;
    for (position, plan) in plans.iter().enumerate() {
        let start = visual_offset;
        let is_selected = position == selected;
        let is_compared = compared.contains(&position);
        let environment_lines = environment_lines(
            plan,
            inner.width,
            is_selected,
            is_compared,
            widths,
            wrap_counts,
        );
        for line in environment_lines {
            visual_offset += visual_line_count(&line, inner.width);
            lines.push(line);
        }
        if is_selected {
            selected_range = Some(start..visual_offset);
        }
    }
    if let Some(depth) = walk_limit {
        for line in walk_limit_lines(depth) {
            visual_offset += visual_line_count(&line, inner.width);
            lines.push(line);
        }
    }
    let viewport = usize::from(inner.height);
    let max_scroll = visual_offset.saturating_sub(viewport);
    let scroll = selected_range.map_or(0, |range| {
        range
            .start
            .saturating_sub(viewport.saturating_sub(range.len()) / 2)
            .min(max_scroll)
    });
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(theme::overview_text_style())
            .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
        inner,
    );
}

fn environment_lines(
    plan: &EnvironmentPlan,
    width: u16,
    selected: bool,
    compared: bool,
    widths: CountWidths,
    wrap_counts: bool,
) -> Vec<Line<'static>> {
    let mut lines = vec![environment_name_line(plan, width, selected, compared)];
    let status = status_line(plan, width);
    match plan.state() {
        EnvironmentState::Ready { .. } => {
            let review = plan.plan_review().expect("ready environment has a review");
            if !review.has_changes() {
                let mut line = status;
                line.push_span(Span::styled("  No changes", theme::overview_muted_style()));
                lines.push(line);
                return lines;
            }
            let (first_counts, second_counts) =
                count_lines(review.summary(), widths, wrap_counts, usize::from(width));
            let combined = append_counts(status.clone(), first_counts.clone());
            if combined.width() <= usize::from(width) {
                lines.push(combined);
            } else {
                lines.push(status);
                lines.push(first_counts);
            }
            if second_counts.width() > 0 {
                lines.push(second_counts);
            }
        }
        EnvironmentState::Error => {
            lines.push(status);
            lines.push(error_reason_line(plan, width));
            lines.push(retry_hint_line());
        }
        EnvironmentState::Unselected
        | EnvironmentState::Pending
        | EnvironmentState::Running
        | EnvironmentState::ExcludedHcp => {
            lines.push(status);
        }
    }
    lines
}

fn append_counts(mut status: Line<'static>, counts: Line<'static>) -> Line<'static> {
    status.push_span(Span::styled(" ", theme::overview_text_style()));
    status.spans.extend(counts.spans.into_iter().skip(1));
    status
}

fn visual_line_count(line: &Line<'static>, width: u16) -> usize {
    Paragraph::new(line.clone())
        .wrap(Wrap { trim: false })
        .line_count(width.max(1))
        .max(1)
}

fn environment_name_line(
    plan: &EnvironmentPlan,
    width: u16,
    selected: bool,
    compared: bool,
) -> Line<'static> {
    let marker = if selected { "> " } else { "  " };
    // A candidate that was never planned has no comparison choice, so it shows no checkbox.
    let checkbox = if !plan.is_target() {
        "   "
    } else if compared {
        "[x]"
    } else {
        "[ ]"
    };
    let production = plan.is_production();
    let suffix = if production { " [PROD]" } else { "" };
    let reserved =
        Line::from(marker).width() + Line::from(checkbox).width() + 1 + Line::from(suffix).width();
    let name_width = usize::from(width).saturating_sub(reserved);
    let name = take_from_start(&plan.display_name(), name_width);
    let selection_style = if selected {
        theme::overview_header_selected_style()
    } else if !compared {
        theme::overview_muted_style()
    } else {
        theme::overview_text_style()
    };
    let checkbox_style = if compared {
        theme::overview_text_style()
    } else {
        theme::overview_muted_style()
    };
    let mut spans = vec![
        Span::styled(
            marker,
            if selected {
                theme::overview_selection_marker_style()
            } else {
                theme::overview_muted_style()
            },
        ),
        Span::styled(checkbox, checkbox_style),
        Span::styled(" ", theme::overview_muted_style()),
        Span::styled(name, selection_style),
    ];
    if production {
        spans.push(Span::styled(
            suffix,
            theme::overview_text_style().add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn status_line(plan: &EnvironmentPlan, width: u16) -> Line<'static> {
    let status = if matches!(plan.state(), EnvironmentState::ExcludedHcp) {
        "Excluded"
    } else {
        shell::status(plan)
    };
    Line::from(vec![
        Span::styled("    ", theme::overview_text_style()),
        shell::status_marker(plan.state()),
        Span::styled(
            take_from_start(status, usize::from(width).saturating_sub(6)),
            shell::status_style(plan.state()),
        ),
    ])
}

fn error_reason_line(plan: &EnvironmentPlan, width: u16) -> Line<'static> {
    let diagnostic = plan.diagnostic();
    let reason = diagnostic
        .text()
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Terraform plan failed");
    Line::from(vec![
        Span::styled("    ", theme::overview_text_style()),
        Span::styled(
            take_from_start(reason, usize::from(width).saturating_sub(4)),
            theme::overview_muted_style(),
        ),
    ])
}

fn retry_hint_line() -> Line<'static> {
    Line::from(vec![
        Span::styled("    ", theme::overview_text_style()),
        Span::styled("r", theme::overview_footer_key_style()),
        Span::styled(" retry", theme::overview_text_style()),
    ])
}

// The list may be incomplete, so the note stays after the last environment instead of in a
// border title that a narrow sidebar would cut off.
fn walk_limit_lines(depth: usize) -> [Line<'static>; 2] {
    [
        Line::default(),
        Line::styled(
            format!("Searched {depth} levels down. Name deeper environments with --env-dir."),
            theme::overview_muted_style(),
        ),
    ]
}

fn count_widths(plans: &[EnvironmentPlan]) -> CountWidths {
    let mut widths = CountWidths {
        additions: 2,
        updates: 2,
        deletions: 2,
        replacements: "0 replace".len(),
    };
    for plan in plans {
        let Some(review) = plan.review() else {
            continue;
        };
        let counts = review.review().summary();
        widths.additions = widths.additions.max(counts.creates.to_string().len() + 1);
        widths.updates = widths.updates.max(counts.updates.to_string().len() + 1);
        widths.deletions = widths.deletions.max(counts.deletes.to_string().len() + 1);
        widths.replacements = widths
            .replacements
            .max(counts.replaces.to_string().len() + " replace".len());
    }
    widths
}

const fn count_width(widths: CountWidths) -> usize {
    widths.additions + widths.updates + widths.deletions + widths.replacements + 6
}

fn count_lines(
    counts: PlanSummary,
    widths: CountWidths,
    wrapped: bool,
    max_width: usize,
) -> (Line<'static>, Line<'static>) {
    let add = count_span(
        "+",
        counts.creates,
        widths.additions,
        theme::overview_total_add_style(),
    );
    let update = count_span(
        "~",
        counts.updates,
        widths.updates,
        theme::overview_total_update_style(),
    );
    let delete = count_span(
        "-",
        counts.deletes,
        widths.deletions,
        theme::overview_total_destroy_style(),
    );
    let replace = count_span(
        "",
        counts.replaces,
        widths.replacements,
        theme::overview_total_replace_style(),
    );
    if wrapped {
        let (first_indent, first_gap) = count_spacing(widths.additions + widths.updates, max_width);
        let (second_indent, second_gap) =
            count_spacing(widths.deletions + widths.replacements, max_width);
        (
            Line::from(vec![
                Span::styled(" ".repeat(first_indent), theme::overview_text_style()),
                add,
                Span::styled(" ".repeat(first_gap), theme::overview_text_style()),
                update,
            ]),
            Line::from(vec![
                Span::styled(" ".repeat(second_indent), theme::overview_text_style()),
                delete,
                Span::styled(" ".repeat(second_gap), theme::overview_text_style()),
                replace,
            ]),
        )
    } else {
        (
            Line::from(vec![
                Span::styled("    ", theme::overview_text_style()),
                add,
                Span::styled("  ", theme::overview_text_style()),
                update,
                Span::styled("  ", theme::overview_text_style()),
                delete,
                Span::styled("  ", theme::overview_text_style()),
                replace,
            ]),
            Line::default(),
        )
    }
}

fn count_spacing(content_width: usize, max_width: usize) -> (usize, usize) {
    let gap = 2.min(max_width.saturating_sub(content_width));
    let indent = 4.min(max_width.saturating_sub(content_width.saturating_add(gap)));
    (indent, gap)
}

fn count_span(prefix: &str, count: usize, width: usize, style: Style) -> Span<'static> {
    let text = if count == 0 {
        String::new()
    } else if prefix.is_empty() {
        format!("{count} replace")
    } else {
        format!("{prefix}{count}")
    };
    Span::styled(format!("{text:<width$}"), style)
}

#[cfg(test)]
mod tests {
    use super::{CountWidths, count_lines};
    use crate::app::plan::PlanSummary;

    #[test]
    fn wrapped_count_lines_fit_the_sidebar_inner_width() {
        let counts = PlanSummary {
            deletes: 1000,
            replaces: 1000,
            ..PlanSummary::default()
        };
        let widths = CountWidths {
            additions: 2,
            updates: 2,
            deletions: 5,
            replacements: 12,
        };

        let (first, second) = count_lines(counts, widths, true, 22);
        assert!(first.width() <= 22, "{first}");
        assert!(second.width() <= 22, "{second}");
        assert!(second.to_string().contains("-1000"));
        assert!(second.to_string().contains("1000 replace"));
    }
}
