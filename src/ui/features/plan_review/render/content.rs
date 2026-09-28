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
    // Also keeps the plan text alive, so text replaced by a new plan is freed only once the view
    // prepares the next body.
    document: PlanDocumentKey,
    filter_query: String,
    // Diagnostics and the no-match notice are few and always come first, so they keep their
    // rendered lines; the plan rows follow them.
    notices: Vec<Line<'static>>,
    plan: Vec<PlanSource>,
    metrics: ContentMetrics,
    matches: Vec<PlanReviewMatch>,
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
        let document = review.document();
        let filtered = document.filter(filter_query);
        let mut notices = diagnostic_lines(review);
        if filtered.matching_resources() == 0
            && filtered.matching_outputs() == 0
            && !filter_query.is_empty()
        {
            notices.push(Line::from(Span::styled(
                "No matching changes.",
                theme::warning_style(),
            )));
            notices.push(Line::default());
        }
        // Widths are kept only to find the widest row that remains after trimming.
        let mut notice_widths = notices.iter().map(display_width).collect::<Vec<_>>();
        let mut plan = Vec::new();
        let mut plan_widths = Vec::new();
        let mut matches = Vec::new();
        for (line_number, line) in filtered.lines_with_indices() {
            let kind = document.line_kind(line_number);
            if kind == PlanLineKind::Intro {
                continue;
            }
            if filtered_view
                && filtered.matching_outputs() == 0
                && kind == PlanLineKind::OutputSection
            {
                continue;
            }
            let row = notices.len() + plan.len();
            // A frame measures each drawn span on its own, so a grapheme cluster cut where a match
            // or emphasis splits the line counts per part. ASCII text has no cluster to cut, so
            // only other lines are styled here to get the width a frame draws.
            let width = if line.is_ascii() {
                text_width(line)
            } else {
                display_width(&plan_line_and_matches(line, filter_query, row, None, kind).0)
            };
            plan_widths.push(width);
            plan.push(PlanSource { kind, line_number });
            matches.extend(
                search_matches(line, filter_query, row)
                    .into_iter()
                    .map(|(_, found)| found),
            );
        }
        while plan
            .last()
            .is_some_and(|source| plan_line(document, *source, filter_query, 0, None).width() == 0)
        {
            plan_widths.pop();
            plan.pop();
        }
        if plan.is_empty() {
            while notices.last().is_some_and(|line| line.width() == 0) {
                notice_widths.pop();
                notices.pop();
            }
        }
        Self {
            document: document.key(),
            filter_query: filter_query.to_owned(),
            metrics: ContentMetrics {
                line_count: notices.len() + plan.len(),
                max_width: notice_widths
                    .into_iter()
                    .chain(plan_widths)
                    .max()
                    .unwrap_or(0),
            },
            notices,
            plan,
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
        let plan_row = row.checked_sub(self.notices.len())?;
        self.plan.get(plan_row).map(|source| source.line_number)
    }

    /// Styles `rows` from the document they were prepared for. The selected match is highlighted
    /// only when it falls on one of them.
    pub(super) fn lines<'a>(
        &'a self,
        document: &'a PlanDocument,
        rows: Range<usize>,
        selected: Option<&PlanReviewMatch>,
    ) -> Vec<Line<'a>> {
        debug_assert!(
            self.document.is_for(document),
            "the body is styled from the document it was prepared for"
        );
        let end = rows.end.min(self.metrics.line_count);
        let start = rows.start.min(end);
        let notices = &self.notices[start.min(self.notices.len())..end.min(self.notices.len())];
        let plan_start = start.saturating_sub(self.notices.len());
        let plan_rows = &self.plan[plan_start..end.saturating_sub(self.notices.len())];
        notices
            .iter()
            .cloned()
            .chain(
                plan_rows
                    .iter()
                    .zip(self.notices.len() + plan_start..)
                    .map(|(source, row)| {
                        plan_line(
                            document,
                            *source,
                            &self.filter_query,
                            row,
                            selected.filter(|selected| selected.line() == row),
                        )
                    }),
            )
            .collect()
    }
}

fn plan_line<'a>(
    document: &'a PlanDocument,
    source: PlanSource,
    query: &str,
    row: usize,
    selected: Option<&PlanReviewMatch>,
) -> Line<'a> {
    plan_line_and_matches(
        document.line(source.line_number),
        query,
        row,
        selected,
        source.kind,
    )
    .0
}

/// Keeps the prepared plan body between frames and key presses. The body is prepared again only
/// when the document, its diagnostics, the filter query, or the filter visibility changes.
#[derive(Clone, Default)]
pub(crate) struct PlanContentCache(RefCell<Option<CachedContent>>);

#[derive(Clone)]
struct CachedContent {
    // Shared like the body, so a view clone does not copy the diagnostic text.
    diagnostics: Rc<[Diagnostic]>,
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
            cached.content.document.is_for(review.document())
                && *cached.diagnostics == *review.diagnostics()
                && cached.filtered_view == filtered_view
                && cached.content.filter_query == filter_query
        }) {
            return Rc::clone(&cached.content);
        }
        let content = Rc::new(PlanContent::prepare(review, filtered_view, filter_query));
        *cached = Some(CachedContent {
            diagnostics: Rc::from(review.diagnostics()),
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
    let style = plan_line_style(line, kind);
    let emphasis = emphasized_ranges(line, kind);
    if query.is_empty() && emphasis.is_empty() {
        return (Line::from(Span::styled(line, style)), Vec::new());
    }
    let mut result = Line::default();
    let mut matches = Vec::new();
    let mut cursor = 0;
    for (range, rendered_match) in search_matches(line, query, line_index) {
        push_emphasized(&mut result, line, cursor..range.start, style, &emphasis);
        let match_style = selected
            .filter(|selected| {
                selected.start() == rendered_match.start() && selected.end() == rendered_match.end()
            })
            .map_or_else(theme::search_match_style, |_| {
                theme::selected_search_match_style()
            });
        // A match keeps its own style over the line style and any emphasis under it.
        result.push_span(Span::styled(&line[range.clone()], match_style));
        matches.push(rendered_match);
        cursor = range.end;
    }
    push_emphasized(&mut result, line, cursor..line.len(), style, &emphasis);
    (result, matches)
}

// Finds each match of `query` with its byte range and drawn columns. Columns measure the raw text
// between matches, so they do not depend on how the line is styled.
fn search_matches(
    line: &str,
    query: &str,
    line_index: usize,
) -> Vec<(Range<usize>, PlanReviewMatch)> {
    let mut found = Vec::new();
    if query.is_empty() {
        return found;
    }
    let mut cursor = 0;
    let mut column = 0;
    for (index, match_text) in line.match_indices(query) {
        column += text_width(&line[cursor..index]);
        let start_column = column;
        column += text_width(match_text);
        let end = index + match_text.len();
        found.push((
            index..end,
            PlanReviewMatch::new(line_index, start_column, column),
        ));
        cursor = end;
    }
    found
}

#[derive(Clone, Copy)]
enum Emphasis {
    HiddenValue,
    ChangeArrow,
}

const EMPHASIZED_TEXT: [(&str, Emphasis); 3] = [
    ("(known after apply)", Emphasis::HiddenValue),
    ("(sensitive value)", Emphasis::HiddenValue),
    ("->", Emphasis::ChangeArrow),
];

// Placeholders for values the plan cannot show and change arrows are marked only in the plan's own
// syntax; quoted strings and heredoc text are values that can contain the same text.
fn emphasized_ranges(line: &str, kind: PlanLineKind) -> Vec<(Range<usize>, Emphasis)> {
    let mut ranges = Vec::new();
    if matches!(kind, PlanLineKind::HeredocBody { .. }) {
        return ranges;
    }
    // Quotes and the emphasized text are ASCII, so the ranges found fall on character boundaries.
    let bytes = line.as_bytes();
    let mut quoted = false;
    let mut escaped = false;
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        } else if let Some(&(text, emphasis)) = EMPHASIZED_TEXT
            .iter()
            .find(|(text, _)| bytes[index..].starts_with(text.as_bytes()))
        {
            ranges.push((index..index + text.len(), emphasis));
            index += text.len();
            continue;
        }
        index += 1;
    }
    ranges
}

// Pushes `range` of `line` in the line style, adding the emphasis of each part it overlaps.
fn push_emphasized<'a>(
    result: &mut Line<'a>,
    line: &'a str,
    range: Range<usize>,
    style: Style,
    emphasis: &[(Range<usize>, Emphasis)],
) {
    let mut cursor = range.start;
    for (emphasized, kind) in emphasis {
        let start = emphasized.start.clamp(cursor, range.end);
        let end = emphasized.end.clamp(cursor, range.end);
        if start == end {
            continue;
        }
        if cursor < start {
            result.push_span(Span::styled(&line[cursor..start], style));
        }
        let emphasized_style = match kind {
            Emphasis::HiddenValue => theme::plan_hidden_value_style(style),
            Emphasis::ChangeArrow => theme::plan_change_arrow_style(style),
        };
        result.push_span(Span::styled(&line[start..end], emphasized_style));
        cursor = end;
    }
    if cursor < range.end {
        result.push_span(Span::styled(&line[cursor..range.end], style));
    }
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
// document's marker column, after spaces and before a space or the line end, counts. A saturated
// column stands for one too wide to store, so it marks no line.
fn heredoc_marker(line: &str, marker_column: u16) -> Option<char> {
    if marker_column == u16::MAX {
        return None;
    }
    let (indent, rest) = line.split_at_checked(usize::from(marker_column))?;
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
    horizontal: usize,
    width: u16,
) -> Vec<Line<'a>> {
    lines
        .iter()
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
    line.spans.iter().map(span_width).sum()
}

// Most plan text is printable ASCII, where every byte is one drawn cell, so only other spans pay
// for grapheme segmentation.
fn span_width(span: &Span<'_>) -> usize {
    let content = span.content.as_ref();
    if content
        .bytes()
        .all(|byte| byte.is_ascii_graphic() || byte == b' ')
    {
        return content.len();
    }
    span.styled_graphemes(Style::default())
        .map(|grapheme| grapheme_width(grapheme.symbol))
        .sum()
}

// The drawn width of text shown as one span.
fn text_width(text: &str) -> usize {
    span_width(&Span::raw(text))
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
