use ratatui::{
    buffer::CellWidth,
    style::Style,
    text::{Line, Span},
};

use crate::app::{
    execution::DiagnosticSeverity,
    review::{FilteredPlan, PlanLineKind, PlanReview},
    session::ReviewSessionState,
};
use crate::ui::features::plan_review::PlanReviewMatch;
use crate::ui::theme;

pub(super) struct PreparedContent<'a> {
    pub(super) filter_query: &'a str,
    pub(super) lines: Vec<Line<'a>>,
    metrics: ContentMetrics,
    pub(super) sources: Vec<Option<PlanSource<'a>>>,
    pub(super) matches: Vec<PlanReviewMatch>,
}

#[derive(Clone, Copy)]
pub(super) struct ContentMetrics {
    pub(super) line_count: usize,
    pub(super) max_width: usize,
}

impl PreparedContent<'_> {
    pub(super) const fn metrics(&self) -> ContentMetrics {
        self.metrics
    }
}

#[derive(Clone, Copy)]
pub(super) struct PlanSource<'a> {
    text: &'a str,
    kind: PlanLineKind,
    pub(super) line_number: usize,
}

pub(super) fn prepare_content<'a>(
    review: &'a PlanReview,
    filtered_view: bool,
    filter_query: &'a str,
) -> PreparedContent<'a> {
    let filtered = review.document().filter(filter_query);
    let (lines, sources, matches) = review_lines(review, &filtered, filtered_view, filter_query);
    let metrics = ContentMetrics {
        line_count: lines.len(),
        max_width: max_line_width(&lines),
    };
    PreparedContent {
        filter_query,
        lines,
        metrics,
        sources,
        matches,
    }
}

pub(super) fn prepare_view_content(
    state: &ReviewSessionState,
    filtered_view: bool,
) -> PreparedContent<'_> {
    let review = state.review();
    prepare_content(review, filtered_view, review.search_query())
}

pub(super) fn review_lines<'a>(
    review: &'a PlanReview,
    filtered: &FilteredPlan<'a>,
    filtered_view: bool,
    filter_query: &str,
) -> (
    Vec<Line<'a>>,
    Vec<Option<PlanSource<'a>>>,
    Vec<PlanReviewMatch>,
) {
    let mut lines = diagnostic_lines(review);
    let mut sources = vec![None; lines.len()];
    let mut matches = Vec::new();
    if filtered.matching_resources() == 0
        && filtered.matching_outputs() == 0
        && !filter_query.is_empty()
    {
        lines.push(Line::from(Span::styled(
            "No matching changes.",
            theme::warning_style(),
        )));
        sources.push(None);
        lines.push(Line::default());
        sources.push(None);
    }
    for (line_number, line) in filtered.lines_with_indices() {
        let kind = review.document().line_kind(line_number);
        if kind == PlanLineKind::Intro {
            continue;
        }
        if filtered_view && filtered.matching_outputs() == 0 && kind == PlanLineKind::OutputSection
        {
            continue;
        }
        let line_index = lines.len();
        let (rendered, line_matches) =
            plan_line_and_matches(line, filter_query, line_index, None, kind);
        lines.push(rendered);
        sources.push(Some(PlanSource {
            text: line,
            kind,
            line_number,
        }));
        matches.extend(line_matches);
    }
    while lines.last().is_some_and(|line| line.width() == 0) {
        lines.pop();
        sources.pop();
    }
    (lines, sources, matches)
}

fn diagnostic_lines(review: &PlanReview) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    for diagnostic in review.diagnostics() {
        let style = match diagnostic.severity {
            DiagnosticSeverity::Error => theme::error_style(),
            _ => theme::warning_style(),
        };
        lines.push(Line::from(vec![
            Span::styled(severity_label(diagnostic.severity), style),
            Span::styled(": ", style),
            Span::styled(diagnostic.summary.as_str(), style),
        ]));
        if let Some(detail) = diagnostic.detail.as_deref() {
            lines.extend(detail.lines().map(Line::from));
        }
    }
    if !lines.is_empty() && !review.document().text().is_empty() {
        lines.push(Line::default());
    }
    lines
}

pub(super) fn plan_line_and_matches<'a>(
    line: &'a str,
    query: &str,
    line_index: usize,
    selected: Option<&PlanReviewMatch>,
    kind: PlanLineKind,
) -> (Line<'a>, Vec<PlanReviewMatch>) {
    if query.is_empty() {
        return (
            Line::from(Span::styled(line, plan_line_style(line, kind))),
            Vec::new(),
        );
    }
    let mut result = Line::default();
    let mut matches = Vec::new();
    let mut rest = line;
    let mut rendered_column = 0;
    while let Some(index) = rest.find(query) {
        let (before, matched_and_after) = rest.split_at(index);
        if !before.is_empty() {
            result.push_span(Span::styled(before, plan_line_style(line, kind)));
        }
        rendered_column += Line::from(before).width();
        let (match_text, after) = matched_and_after.split_at(query.len());
        let start_column = rendered_column;
        rendered_column += Line::from(match_text).width();
        let end_column = rendered_column;
        let rendered_match = PlanReviewMatch::new(line_index, start_column, end_column);
        let style = selected
            .filter(|selected| {
                selected.start() == rendered_match.start() && selected.end() == rendered_match.end()
            })
            .map_or_else(theme::search_match_style, |_| {
                theme::selected_search_match_style()
            });
        result.push_span(Span::styled(match_text, style));
        matches.push(rendered_match);
        rest = after;
    }
    if !rest.is_empty() {
        result.push_span(Span::styled(rest, plan_line_style(line, kind)));
    }
    (result, matches)
}

fn plan_line_style(line: &str, kind: PlanLineKind) -> Style {
    if kind == PlanLineKind::Note {
        theme::plan_note_style()
    } else {
        theme::plan_line_style(line)
    }
}

pub(super) fn flash_lines(lines: &[Line<'_>]) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| Line::from(Span::styled(line.to_string(), theme::copy_flash_style())))
        .collect()
}

pub(super) fn content_lines_with_selection<'a>(
    content: &'a PreparedContent<'a>,
    query: &str,
    selected: Option<&PlanReviewMatch>,
) -> Vec<Line<'a>> {
    content
        .lines
        .iter()
        .zip(&content.sources)
        .enumerate()
        .map(|(line_index, (line, source))| {
            source.map_or_else(
                || line.clone(),
                |source| {
                    selected
                        .filter(|selected| selected.line() == line_index)
                        .map_or_else(
                            || line.clone(),
                            |selected| {
                                plan_line_and_matches(
                                    source.text,
                                    query,
                                    line_index,
                                    Some(selected),
                                    source.kind,
                                )
                                .0
                            },
                        )
                },
            )
        })
        .collect()
}

// Paragraph::scroll takes u16 offsets, so the body receives only the visible window instead. The
// window walks the same graphemes and cell widths as ratatui's line truncation.
pub(super) fn visible_lines<'a>(
    lines: &'a [Line<'_>],
    vertical: usize,
    horizontal: usize,
    height: u16,
    width: u16,
) -> Vec<Line<'a>> {
    lines
        .iter()
        .skip(vertical)
        .take(usize::from(height))
        .map(|line| visible_columns(line, horizontal, usize::from(width)))
        .collect()
}

fn visible_columns<'a>(line: &'a Line<'_>, offset: usize, width: usize) -> Line<'a> {
    let end = offset.saturating_add(width);
    let mut visible = Line::default();
    let mut column = 0;
    for grapheme in line.styled_graphemes(Style::default()) {
        let next = column + usize::from(grapheme.symbol.cell_width());
        if next > end {
            break;
        }
        if next > offset {
            if column < offset {
                // A wide grapheme cut by the left edge leaves its visible cells blank, keeping the
                // columns aligned with the other lines.
                visible.push_span(Span::styled(" ".repeat(next - offset), grapheme.style));
            } else {
                visible.push_span(Span::styled(grapheme.symbol, grapheme.style));
            }
        }
        column = next;
    }
    visible
}

fn max_line_width(lines: &[Line<'_>]) -> usize {
    lines.iter().map(Line::width).max().unwrap_or(0)
}

const fn severity_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "Error",
        DiagnosticSeverity::Warning => "Warning",
        DiagnosticSeverity::Info => "Info",
        DiagnosticSeverity::Unknown => "Diagnostic",
    }
}
