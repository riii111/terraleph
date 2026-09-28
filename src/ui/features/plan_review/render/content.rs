use std::{cell::RefCell, fmt, ops::Range, rc::Rc};

use ratatui::{
    buffer::CellWidth,
    style::Style,
    text::{Line, Span},
};

use crate::app::{
    execution::{Diagnostic, DiagnosticSeverity},
    review::{PlanDocument, PlanDocumentKey, PlanLineKind, PlanReview},
};
use crate::ui::features::plan_review::PlanReviewMatch;
use crate::ui::theme;

/// The plan body for one document, its diagnostics, a filter query, and the filter visibility.
/// Plan rows keep only their source line, so each frame styles just the rows it shows.
pub(super) struct PlanContent {
    filter_query: String,
    rows: Vec<ContentRow>,
    metrics: ContentMetrics,
    matches: Vec<PlanReviewMatch>,
}

enum ContentRow {
    // Diagnostics and the no-match notice are few, so they keep their rendered line.
    Rendered(Line<'static>),
    Plan(PlanSource),
}

#[derive(Clone, Copy)]
struct PlanSource {
    kind: PlanLineKind,
    line_number: usize,
}

#[derive(Clone, Copy)]
pub(super) struct ContentMetrics {
    pub(super) line_count: usize,
    pub(super) max_width: usize,
}

impl PlanContent {
    pub(super) fn prepare(review: &PlanReview, filtered_view: bool, filter_query: &str) -> Self {
        let filtered = review.document().filter(filter_query);
        let mut rendered = diagnostic_lines(review);
        if filtered.matching_resources() == 0
            && filtered.matching_outputs() == 0
            && !filter_query.is_empty()
        {
            rendered.push(Line::from(Span::styled(
                "No matching changes.",
                theme::warning_style(),
            )));
            rendered.push(Line::default());
        }
        // Widths are kept only to find the widest row that remains after trimming.
        let mut widths = rendered.iter().map(display_width).collect::<Vec<_>>();
        let mut rows = rendered
            .into_iter()
            .map(ContentRow::Rendered)
            .collect::<Vec<_>>();
        let mut matches = Vec::new();
        for (line_number, line) in filtered.lines_with_indices() {
            let kind = review.document().line_kind(line_number);
            if kind == PlanLineKind::Intro {
                continue;
            }
            if filtered_view
                && filtered.matching_outputs() == 0
                && kind == PlanLineKind::OutputSection
            {
                continue;
            }
            // The styled spans give the same width and match columns that a frame draws.
            let (rendered, line_matches) =
                plan_line_and_matches(line, filter_query, rows.len(), None, kind);
            widths.push(display_width(&rendered));
            rows.push(ContentRow::Plan(PlanSource { kind, line_number }));
            matches.extend(line_matches);
        }
        // Trailing rows without text are dropped; only those rows are styled again to check.
        while rows
            .last()
            .is_some_and(|row| row_line(row, review.document(), filter_query, 0, None).width() == 0)
        {
            widths.pop();
            rows.pop();
        }
        Self {
            filter_query: filter_query.to_owned(),
            metrics: ContentMetrics {
                line_count: rows.len(),
                max_width: widths.into_iter().max().unwrap_or(0),
            },
            rows,
            matches,
        }
    }

    pub(super) const fn metrics(&self) -> ContentMetrics {
        self.metrics
    }

    pub(super) fn filter_query(&self) -> &str {
        &self.filter_query
    }

    pub(super) fn matches(&self) -> &[PlanReviewMatch] {
        &self.matches
    }

    /// Returns the plan line shown at `row`, or `None` for diagnostics and notices.
    pub(super) fn source_line(&self, row: usize) -> Option<usize> {
        match self.rows.get(row)? {
            ContentRow::Rendered(_) => None,
            ContentRow::Plan(source) => Some(source.line_number),
        }
    }

    /// Styles `rows` from the document they were prepared for. The selected match is highlighted
    /// only when it falls on one of them.
    pub(super) fn lines<'a>(
        &'a self,
        document: &'a PlanDocument,
        rows: Range<usize>,
        selected: Option<&PlanReviewMatch>,
    ) -> Vec<Line<'a>> {
        let end = rows.end.min(self.rows.len());
        let start = rows.start.min(end);
        self.rows[start..end]
            .iter()
            .zip(start..)
            .map(|(row, row_index)| {
                row_line(
                    row,
                    document,
                    &self.filter_query,
                    row_index,
                    selected.filter(|selected| selected.line() == row_index),
                )
            })
            .collect()
    }
}

fn row_line<'a>(
    row: &'a ContentRow,
    document: &'a PlanDocument,
    query: &str,
    row_index: usize,
    selected: Option<&PlanReviewMatch>,
) -> Line<'a> {
    match row {
        ContentRow::Rendered(line) => line.clone(),
        ContentRow::Plan(source) => {
            plan_line_and_matches(
                document.line(source.line_number),
                query,
                row_index,
                selected,
                source.kind,
            )
            .0
        }
    }
}

/// Keeps the prepared plan body between frames and key presses. The body is prepared again only
/// when the document, its diagnostics, the filter query, or the filter visibility changes.
#[derive(Clone, Default)]
pub(crate) struct PlanContentCache(RefCell<Option<CachedContent>>);

#[derive(Clone)]
struct CachedContent {
    document: PlanDocumentKey,
    diagnostics: Vec<Diagnostic>,
    filtered_view: bool,
    content: Rc<PlanContent>,
}

impl PlanContentCache {
    pub(super) fn get(
        &self,
        review: &PlanReview,
        filtered_view: bool,
        filter_query: &str,
    ) -> Rc<PlanContent> {
        let mut cached = self.0.borrow_mut();
        if let Some(cached) = cached.as_ref().filter(|cached| {
            cached.document.is_for(review.document())
                && cached.diagnostics.as_slice() == review.diagnostics()
                && cached.filtered_view == filtered_view
                && cached.content.filter_query == filter_query
        }) {
            return Rc::clone(&cached.content);
        }
        let content = Rc::new(PlanContent::prepare(review, filtered_view, filter_query));
        *cached = Some(CachedContent {
            document: review.document().key(),
            diagnostics: review.diagnostics().to_vec(),
            filtered_view,
            content: Rc::clone(&content),
        });
        content
    }
}

// The cache only mirrors the review, so views compare and print as if it were absent; printing it
// could also expose plan text.
impl PartialEq for PlanContentCache {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for PlanContentCache {}

impl fmt::Debug for PlanContentCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PlanContentCache")
            .finish_non_exhaustive()
    }
}

fn diagnostic_lines(review: &PlanReview) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for diagnostic in review.diagnostics() {
        let style = match diagnostic.severity {
            DiagnosticSeverity::Error => theme::error_style(),
            _ => theme::warning_style(),
        };
        lines.push(Line::from(vec![
            Span::styled(severity_label(diagnostic.severity), style),
            Span::styled(": ", style),
            Span::styled(diagnostic.summary.clone(), style),
        ]));
        if let Some(detail) = diagnostic.detail.as_deref() {
            lines.extend(detail.lines().map(|line| Line::from(line.to_owned())));
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
        rendered_column += display_width(&Line::from(before));
        let (match_text, after) = matched_and_after.split_at(query.len());
        let start_column = rendered_column;
        rendered_column += display_width(&Line::from(match_text));
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
    match kind {
        PlanLineKind::Note => theme::plan_note_style(),
        PlanLineKind::ResourceHeader => theme::plan_resource_header_style(),
        PlanLineKind::HeredocBody { marker_column } => {
            theme::plan_marker_style(heredoc_marker(line, marker_column))
        }
        PlanLineKind::Body
        | PlanLineKind::Intro
        | PlanLineKind::Summary
        | PlanLineKind::OutputSection => theme::plan_line_style(line),
    }
}

// Heredoc text can start with the same characters as a diff marker, so only a marker in the
// document's marker column, after spaces and before a space or the line end, counts.
fn heredoc_marker(line: &str, marker_column: usize) -> Option<char> {
    let (indent, rest) = line.split_at_checked(marker_column)?;
    let mut rest = rest.chars();
    let marker = rest
        .next()
        .filter(|marker| matches!(marker, '+' | '-' | '~'))?;
    (indent.bytes().all(|byte| byte == b' ') && rest.next().is_none_or(|next| next == ' '))
        .then_some(marker)
}

pub(super) fn flash_lines(lines: &[Line<'_>]) -> Vec<Line<'static>> {
    lines
        .iter()
        .map(|line| Line::from(Span::styled(line.to_string(), theme::copy_flash_style())))
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
        let next = column + grapheme_width(grapheme.symbol);
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

// Line::width measures the whole string, which differs from the drawn cells for halfwidth sound
// marks, some joined scripts, and control characters. Scroll limits, match columns, and the visible
// window all measure the graphemes ratatui draws instead.
pub(super) fn display_width(line: &Line<'_>) -> usize {
    line.styled_graphemes(Style::default())
        .map(|grapheme| grapheme_width(grapheme.symbol))
        .sum()
}

fn grapheme_width(symbol: &str) -> usize {
    usize::from(symbol.cell_width())
}

const fn severity_label(severity: DiagnosticSeverity) -> &'static str {
    match severity {
        DiagnosticSeverity::Error => "Error",
        DiagnosticSeverity::Warning => "Warning",
        DiagnosticSeverity::Info => "Info",
        DiagnosticSeverity::Unknown => "Diagnostic",
    }
}

#[cfg(test)]
mod test_support {
    use std::rc::Rc;

    use super::{PlanContent, PlanContentCache};

    impl PlanContentCache {
        // Tests observe what a render or key left behind without preparing the body themselves.
        pub(in crate::ui::features::plan_review::render) fn cached_for_test(
            &self,
        ) -> Option<Rc<PlanContent>> {
            self.0
                .borrow()
                .as_ref()
                .map(|cached| Rc::clone(&cached.content))
        }
    }
}
