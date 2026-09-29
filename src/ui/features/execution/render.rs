use std::time::{Duration, Instant};

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::app::{
    copy::CopyNotice,
    execution::{
        EventStream, ExecutionLogLine, ExecutionResult, ExecutionStage, ExecutionState,
        ExecutionTargetState, ExecutionTargetStatus, LogLineIndex,
    },
    plan::PlanAction,
};
use crate::ui::display_text::{shown_width, visible_cells};
use crate::ui::primitives::atoms::{copy_flash, scrollbar, separator};
use crate::ui::primitives::molecules::terminal_notice;
use crate::ui::shell::{
    context::{display_width, truncate_middle},
    footer, header, layout as shell_layout,
};
use crate::ui::theme;

use super::{ExecutionViewState, LogWidth};

const MIN_HEIGHT: u16 = 9;
const MIN_WIDTH: u16 = 32;
const STATUS_HEIGHT: u16 = 3;
const APPLY_STATUS_HEIGHT: u16 = 2;
// Narrow panels keep this much of the address and clip the trailing columns instead.
const TARGET_ADDRESS_MIN_WIDTH: usize = 24;
const TARGET_STATUS_WIDTH: usize = "Incomplete".len();
// A replacement shows both actions, which is the longest label Terraform produces.
const TARGET_ACTION_WIDTH: usize = "delete/create".len();
const TARGET_ELAPSED_WIDTH: usize = "Elapsed".len();
const TARGET_PREVIOUS_WIDTH: usize = "Previous".len();

pub(crate) fn render_execution_with_quit_confirmation(
    frame: &mut Frame<'_>,
    state: &ExecutionState,
    view: ExecutionViewState,
    now: Instant,
    quit_confirmation: bool,
) {
    let area = frame.area();
    if state.is_apply() {
        render_apply_execution(frame, state, view, now, quit_confirmation);
        return;
    }
    let content = prepare_content(state, view);
    let notice = state.copy_feedback().notice_at(now);
    let layout = log_view_layout(
        area,
        state,
        &content,
        notice.map(CopyNotice::message),
        quit_confirmation,
    );
    if area.width < MIN_WIDTH
        || area.height < MIN_HEIGHT
        || layout.body().width == 0
        || layout.body().height == 0
    {
        let message = if quit_confirmation {
            "Quit? Enter exit / Esc cancel"
        } else if state.stage() == ExecutionStage::Failed {
            "Terminal too small. Resize or press q to quit."
        } else {
            "Terminal too small. Resize or press Ctrl-C to cancel."
        };
        terminal_notice::render_wrapped(frame, area, message);
        return;
    }

    header::render_execution(frame, layout.shell.header(), state.context());
    let content_area =
        shell_layout::render_content_block(frame, layout.shell.content(), state.stage().title());
    debug_assert_eq!(content_area, layout.shell.content_inner());
    render_log_view(
        frame,
        &layout,
        state,
        view,
        now,
        &content,
        notice.filter(|_| !quit_confirmation),
    );
}

fn render_apply_execution(
    frame: &mut Frame<'_>,
    state: &ExecutionState,
    view: ExecutionViewState,
    now: Instant,
    quit_confirmation: bool,
) {
    let area = frame.area();
    let content = prepare_selected_content(state, view);
    let status = apply_status_lines(state, now);
    let notice = state.copy_feedback().notice_at(now);
    let layout = applying_layout(
        area,
        state,
        &content,
        &status,
        notice.map(CopyNotice::message),
        quit_confirmation,
    );
    if area.width < MIN_WIDTH
        || area.height < MIN_HEIGHT
        || layout.target_body().height == 0
        || layout.body().height == 0
    {
        let message = if quit_confirmation {
            "Quit? Enter exit / Esc cancel"
        } else if finished_apply(state) {
            "Terminal too small. Resize or press q to quit."
        } else {
            "Terminal too small. Resize or press Ctrl-C to cancel."
        };
        terminal_notice::render_wrapped(frame, area, message);
        return;
    }

    header::render_execution(frame, layout.shell.header(), state.context());
    let title = if finished_apply(state) {
        "Apply result"
    } else {
        "Applying"
    };
    let content_area = shell_layout::render_content_block(frame, layout.shell.content(), title);
    debug_assert_eq!(content_area, layout.shell.content_inner());
    frame.render_widget(status_paragraph(status, true), layout.status());
    render_target_panel(
        frame,
        layout.target_panel(),
        layout.target_body(),
        state,
        view,
        now,
    );
    render_log_panel(frame, &layout, state, view, &content, now);
    render_footer(
        frame,
        layout.shell.footer(),
        layout.shell.footer_lines(),
        notice.filter(|_| !quit_confirmation),
    );
}

fn render_target_panel(
    frame: &mut Frame<'_>,
    panel: Rect,
    body: Rect,
    state: &ExecutionState,
    view: ExecutionViewState,
    now: Instant,
) {
    let title = if view.logs_open() {
        "Targets"
    } else {
        "Targets *"
    };
    frame.render_widget(
        Block::new()
            .borders(Borders::ALL)
            .border_style(theme::frame_style())
            .title(title),
        panel,
    );
    let all_logs_style = if view.selected_target().is_none() {
        theme::accent_style().add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        theme::secondary_style()
    };
    let columns = TargetColumns::new(
        state.progress().targets(),
        usize::from(body.width),
        state.progress().has_previous(),
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            if view.selected_target().is_none() {
                "> All logs"
            } else {
                "  All logs"
            },
            all_logs_style,
        )))
        .style(theme::body_style()),
        Rect::new(body.x, body.y.saturating_sub(2), body.width, 1),
    );
    frame.render_widget(
        Paragraph::new(columns.row(
            "  ",
            "Resource",
            ["Status", "Action", "Elapsed", "Previous"],
        ))
        .style(theme::secondary_style()),
        Rect::new(body.x, body.y.saturating_sub(1), body.width, 1),
    );

    let finished = state.result().is_some();
    let indices = state.progress().display_target_indices(finished);
    let max = layout_target_max(indices.len(), body.height);
    let offset = view.target_vertical_offset(0, max);
    let lines = indices
        .iter()
        .skip(offset)
        .take(usize::from(body.height))
        .map(|index| target_line(state, *index, view.selected_target(), now, &columns))
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines).style(theme::body_style()), body);
    if max > 0 {
        scrollbar::render_vertical(
            frame,
            Rect::new(body.x, body.y, body.width.saturating_add(1), body.height),
            indices.len(),
            usize::from(body.height),
            offset,
        );
    }
}

fn render_log_panel(
    frame: &mut Frame<'_>,
    layout: &ExecutionLayout,
    state: &ExecutionState,
    view: ExecutionViewState,
    content: &PreparedContent<'_>,
    now: Instant,
) {
    let title = view
        .selected_target()
        .and_then(|index| state.progress().targets().get(index))
        .map_or_else(
            || "Logs: All logs".to_owned(),
            |target| format!("Logs: {}", target.address()),
        );
    frame.render_widget(
        Block::new()
            .borders(Borders::ALL)
            .border_style(theme::frame_style())
            .title(title),
        layout.log_panel(),
    );
    let line_count = content.line_count();
    let max_line_width = content.max_width();
    let max_vertical = layout.max_vertical();
    let scroll = view.vertical_offset(initial_scroll(state, view, max_vertical), max_vertical);
    let horizontal = view.horizontal().min(layout.max_horizontal());
    let lines = content.visible_lines(scroll, horizontal, layout.body());
    let lines = if state.copy_feedback().flash_active(now) {
        copy_flash::restyle_lines(lines)
    } else {
        lines
    };
    frame.render_widget(
        Paragraph::new(lines).style(theme::body_style()),
        layout.body(),
    );
    render_log_scrollbars(
        frame,
        layout,
        (line_count, max_line_width),
        (scroll, horizontal),
    );
}

fn render_log_view(
    frame: &mut Frame<'_>,
    layout: &ExecutionLayout,
    state: &ExecutionState,
    view: ExecutionViewState,
    now: Instant,
    content: &PreparedContent<'_>,
    notice: Option<CopyNotice>,
) {
    let status = status_lines(state, now);
    frame.render_widget(status_paragraph(status, false), layout.status());

    let line_count = content.line_count();
    let max_line_width = content.max_width();
    let max_vertical = layout.max_vertical();
    let max_horizontal = layout.max_horizontal();
    let scroll = view.vertical_offset(initial_scroll(state, view, max_vertical), max_vertical);
    let horizontal = view.horizontal().min(max_horizontal);
    // The log also fills the cells reserved for scrollbars; the bars are drawn over them.
    let lines = content.visible_lines(scroll, horizontal, layout.log_area());
    let lines = if state.copy_feedback().flash_active(now) {
        copy_flash::restyle_lines(lines)
    } else {
        lines
    };
    frame.render_widget(
        Paragraph::new(lines).style(theme::body_style()),
        layout.log_area(),
    );
    render_log_scrollbars(
        frame,
        layout,
        (line_count, max_line_width),
        (scroll, horizontal),
    );
    frame.render_widget(
        separator::render(layout.separator().width),
        layout.separator(),
    );
    render_footer(
        frame,
        layout.shell.footer(),
        layout.shell.footer_lines(),
        notice,
    );
}

// Bars are drawn only from the layout's reservation, so rendering never re-measures a body
// that already excludes the bar cells.
fn render_log_scrollbars(
    frame: &mut Frame<'_>,
    layout: &ExecutionLayout,
    (line_count, max_line_width): (usize, usize),
    (vertical_offset, horizontal_offset): (usize, usize),
) {
    let body = layout.body();
    let scrollbar_area = Rect::new(
        body.x,
        body.y,
        body.width
            .saturating_add(u16::from(layout.vertical_scrollbar())),
        body.height
            .saturating_add(u16::from(layout.horizontal_scrollbar())),
    );
    if layout.vertical_scrollbar() {
        scrollbar::render_vertical(
            frame,
            scrollbar_area,
            line_count,
            usize::from(body.height),
            vertical_offset,
        );
    }
    if layout.horizontal_scrollbar() {
        scrollbar::render_horizontal(
            frame,
            scrollbar_area,
            max_line_width,
            usize::from(body.width),
            horizontal_offset,
        );
    }
}

fn render_footer(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: &[Line<'static>],
    notice: Option<CopyNotice>,
) {
    footer::render(
        frame,
        area,
        lines,
        notice.map(|notice| {
            (
                notice.message(),
                if matches!(notice, CopyNotice::Failed) {
                    theme::error_style()
                } else {
                    theme::accent_style()
                },
            )
        }),
    );
}

pub(crate) struct ExecutionLayout {
    shell: shell_layout::ShellLayout,
    status: Rect,
    target_panel: Rect,
    target_body: Rect,
    log_area: Rect,
    log_panel: Rect,
    separator: Rect,
    body: Rect,
    target_max_vertical: usize,
    vertical_scrollbar: bool,
    horizontal_scrollbar: bool,
    max_vertical: usize,
    max_horizontal: usize,
}

impl ExecutionLayout {
    pub(crate) const fn status(&self) -> Rect {
        self.status
    }

    pub(crate) const fn log_area(&self) -> Rect {
        self.log_area
    }

    pub(crate) const fn target_panel(&self) -> Rect {
        self.target_panel
    }

    pub(crate) const fn target_body(&self) -> Rect {
        self.target_body
    }

    pub(crate) const fn log_panel(&self) -> Rect {
        self.log_panel
    }

    pub(crate) const fn separator(&self) -> Rect {
        self.separator
    }

    pub(crate) const fn body(&self) -> Rect {
        self.body
    }

    pub(crate) const fn vertical_scrollbar(&self) -> bool {
        self.vertical_scrollbar
    }

    pub(crate) const fn horizontal_scrollbar(&self) -> bool {
        self.horizontal_scrollbar
    }

    pub(crate) const fn max_vertical(&self) -> usize {
        self.max_vertical
    }

    pub(crate) const fn target_max_vertical(&self) -> usize {
        self.target_max_vertical
    }

    pub(crate) const fn max_horizontal(&self) -> usize {
        self.max_horizontal
    }
}

pub(crate) fn execution_layout_with_view(
    area: Rect,
    state: &ExecutionState,
    view: ExecutionViewState,
) -> ExecutionLayout {
    execution_layout_with_quit_confirmation_and_view(area, state, view, false)
}

fn execution_layout_with_quit_confirmation_and_view(
    area: Rect,
    state: &ExecutionState,
    view: ExecutionViewState,
    quit_confirmation: bool,
) -> ExecutionLayout {
    let notice = state.copy_feedback().notice().map(CopyNotice::message);
    if state.is_apply() {
        let content = prepare_selected_content(state, view);
        let status = apply_status_lines(state, Instant::now());
        return applying_layout(area, state, &content, &status, notice, quit_confirmation);
    }
    log_view_layout(
        area,
        state,
        &prepare_content(state, view),
        notice,
        quit_confirmation,
    )
}

fn log_view_layout(
    area: Rect,
    state: &ExecutionState,
    content: &PreparedContent<'_>,
    notice: Option<&str>,
    quit_confirmation: bool,
) -> ExecutionLayout {
    let panel_width = shell_layout::centered_width(area);
    let normal_footer_lines = footer_lines(state, panel_width, notice);
    let normal_required_footer_lines = required_footer_lines(state, panel_width, notice);
    let requested_height = log_view_requested_height(
        area,
        state,
        content,
        (&normal_footer_lines, &normal_required_footer_lines),
    );
    let shell_area = shell_layout::centered_area(area, requested_height);
    let footer_lines = if quit_confirmation {
        footer::pad_lines(
            footer::quit_confirmation_lines(panel_width),
            normal_footer_lines.len(),
        )
    } else {
        normal_footer_lines
    };
    let required_footer_lines = if quit_confirmation {
        footer::pad_lines(
            footer::quit_confirmation_lines(panel_width),
            normal_required_footer_lines.len(),
        )
    } else {
        normal_required_footer_lines
    };
    let shell = shell_layout::layout(shell_area, footer_lines, required_footer_lines, 1);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(STATUS_HEIGHT),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(shell.content_inner())
        .to_vec();
    let (status_area, available, separator_area) = (chunks[0], chunks[1], chunks[2]);
    let (vertical_scrollbar, horizontal_scrollbar) =
        scrollbar::reservations(content.line_count(), content.max_width(), available);
    let body = Rect::new(
        available.x,
        available.y,
        available
            .width
            .saturating_sub(u16::from(vertical_scrollbar)),
        available
            .height
            .saturating_sub(u16::from(horizontal_scrollbar)),
    );
    let (max_vertical, max_horizontal) =
        scroll_limits(content.line_count(), content.max_width(), body);
    ExecutionLayout {
        shell,
        status: status_area,
        target_panel: Rect::default(),
        target_body: Rect::default(),
        log_area: available,
        log_panel: Rect::default(),
        separator: separator_area,
        body,
        target_max_vertical: 0,
        vertical_scrollbar,
        horizontal_scrollbar,
        max_vertical,
        max_horizontal,
    }
}

fn applying_layout(
    area: Rect,
    state: &ExecutionState,
    content: &PreparedContent<'_>,
    status: &[Line<'static>],
    notice: Option<&str>,
    quit_confirmation: bool,
) -> ExecutionLayout {
    let panel_width = shell_layout::centered_width(area);
    let normal_footer_lines = apply_footer_lines(state, panel_width, notice);
    let normal_required_footer_lines = apply_required_footer_lines(state, panel_width, notice);
    let footer_lines = if quit_confirmation {
        footer::pad_lines(
            footer::quit_confirmation_lines(panel_width),
            normal_footer_lines.len(),
        )
    } else {
        normal_footer_lines
    };
    let required_footer_lines = if quit_confirmation {
        footer::pad_lines(
            footer::quit_confirmation_lines(panel_width),
            normal_required_footer_lines.len(),
        )
    } else {
        normal_required_footer_lines
    };
    let shell_area = shell_layout::max_centered_area(area);
    let shell = shell_layout::layout(shell_area, footer_lines, required_footer_lines, 4);
    let status_height =
        status_line_count(status, panel_width.saturating_sub(2)).max(APPLY_STATUS_HEIGHT);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(status_height), Constraint::Min(1)])
        .split(shell.content_inner())
        .to_vec();
    let panels = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(chunks[1])
        .to_vec();
    let target_panel = panels[0];
    let log_panel = panels[1];
    let target_inner = Block::new().borders(Borders::ALL).inner(target_panel);
    let target_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(1),
        ])
        .split(target_inner)
        .to_vec();
    let target_body = target_rows[2];
    let target_count = state.progress().targets().len();
    let target_max_vertical = layout_target_max(target_count, target_body.height);
    let log_inner = Block::new().borders(Borders::ALL).inner(log_panel);
    let (vertical_scrollbar, horizontal_scrollbar) =
        scrollbar::reservations(content.line_count(), content.max_width(), log_inner);
    let body = Rect::new(
        log_inner.x,
        log_inner.y,
        log_inner
            .width
            .saturating_sub(u16::from(vertical_scrollbar)),
        log_inner
            .height
            .saturating_sub(u16::from(horizontal_scrollbar)),
    );
    let (max_vertical, max_horizontal) =
        scroll_limits(content.line_count(), content.max_width(), body);
    ExecutionLayout {
        shell,
        status: chunks[0],
        target_panel,
        target_body,
        log_area: log_inner,
        log_panel,
        separator: Rect::default(),
        body,
        target_max_vertical,
        vertical_scrollbar,
        horizontal_scrollbar,
        max_vertical,
        max_horizontal,
    }
}

fn log_view_requested_height(
    area: Rect,
    state: &ExecutionState,
    content: &PreparedContent<'_>,
    footer_lines: (&[Line<'static>], &[Line<'static>]),
) -> u16 {
    if state.stage() != ExecutionStage::Failed {
        return shell_layout::max_centered_height(area);
    }
    let body_height = shell_layout::required_body_height(
        content.line_count(),
        content.max_width(),
        shell_layout::centered_width(area).saturating_sub(2),
    );
    let content_height = STATUS_HEIGHT
        .saturating_add(1)
        .saturating_add(body_height)
        .saturating_add(2);
    shell_layout::required_height(content_height, footer_lines.0, footer_lines.1)
}

pub(crate) fn execution_scroll_position_with_view(
    state: &ExecutionState,
    view: ExecutionViewState,
    layout: &ExecutionLayout,
) -> (usize, usize) {
    let max = layout.max_vertical();
    let current = view.vertical_offset(initial_scroll(state, view, max), max);
    (current, max)
}

pub(crate) const fn execution_target_scroll_position_with_view(
    view: ExecutionViewState,
    layout: &ExecutionLayout,
) -> (usize, usize) {
    let current = view.target_vertical_offset(0, layout.target_max_vertical());
    (current, layout.target_max_vertical())
}

pub(crate) fn execution_horizontal_scroll_position_with_view(
    view: ExecutionViewState,
    layout: &ExecutionLayout,
) -> (usize, usize) {
    let max = layout.max_horizontal();
    (view.horizontal().min(max), max)
}

fn prepare_content(state: &ExecutionState, view: ExecutionViewState) -> PreparedContent<'_> {
    let progress = state.progress();
    PreparedContent::new(
        progress.log(),
        None,
        (progress.log_index(), view.measured_log_width(None)),
        Span::raw("Waiting for Terraform output..."),
    )
}

fn prepare_selected_content(
    state: &ExecutionState,
    view: ExecutionViewState,
) -> PreparedContent<'_> {
    let progress = state.progress();
    let placeholder = Span::styled(
        if finished_apply(state) {
            "No execution output."
        } else if view.selected_target().is_some() {
            "Waiting for target output..."
        } else {
            "Waiting for Terraform output..."
        },
        theme::secondary_style(),
    );
    match view
        .selected_target()
        .and_then(|index| Some((index, progress.targets().get(index)?)))
    {
        Some((index, target)) => PreparedContent::new(
            progress.log(),
            Some(target.log_ids()),
            (target.log_index(), view.measured_log_width(Some(index))),
            placeholder,
        ),
        None => PreparedContent::new(
            progress.log(),
            None,
            (progress.log_index(), view.measured_log_width(None)),
            placeholder,
        ),
    }
}

// The log a panel shows. Its line count comes from the index the progress keeps as entries
// arrive, its width from what the view has measured, and lines are built only for the rows on
// screen.
struct PreparedContent<'a> {
    log: &'a [ExecutionLogLine],
    // Positions in `log` of a selected target's entries; `None` shows every entry.
    entries: Option<&'a [usize]>,
    index: &'a LogLineIndex,
    width: usize,
    // Shown instead of the log while it has no lines.
    placeholder: Option<Span<'static>>,
}

impl<'a> PreparedContent<'a> {
    fn new(
        log: &'a [ExecutionLogLine],
        entries: Option<&'a [usize]>,
        (index, measured): (&'a LogLineIndex, LogWidth),
        placeholder: Span<'static>,
    ) -> Self {
        Self {
            log,
            entries,
            index,
            width: measure_log_width(measured, log, entries).width,
            placeholder: (index.line_count() == 0).then_some(placeholder),
        }
    }

    const fn line_count(&self) -> usize {
        if self.placeholder.is_some() {
            1
        } else {
            self.index.line_count()
        }
    }

    fn max_width(&self) -> usize {
        self.placeholder.as_ref().map_or(self.width, |placeholder| {
            display_width(&placeholder.content)
        })
    }

    // The rows drawn into `area` from line `offset` down, each starting `horizontal` cells into
    // its line.
    fn visible_lines(&self, offset: usize, horizontal: usize, area: Rect) -> Vec<Line<'a>> {
        let (width, height) = (usize::from(area.width), usize::from(area.height));
        if let Some(placeholder) = &self.placeholder {
            return std::iter::once(Line::from(Span::styled(
                visible_cells(&placeholder.content, horizontal, width).into_owned(),
                placeholder.style,
            )))
            .skip(offset)
            .take(height)
            .collect();
        }
        let Some((first, skip)) = self.index.locate(offset) else {
            return Vec::new();
        };
        let line = |(stream, text)| log_line(stream, text, horizontal, width);
        self.entries.map_or_else(
            || {
                log_rows(&self.log[first..], skip, height)
                    .map(line)
                    .collect()
            },
            |entries| {
                log_rows(
                    entries[first..].iter().filter_map(|id| self.log.get(*id)),
                    skip,
                    height,
                )
                .map(line)
                .collect()
            },
        )
    }
}

// Continues `measured` over the entries it has not seen, measuring each line the way the
// renderer draws it. `entries` selects a target's entries from `log`, as in `PreparedContent`.
pub(super) fn measure_log_width(
    measured: LogWidth,
    log: &[ExecutionLogLine],
    entries: Option<&[usize]>,
) -> LogWidth {
    let total = entries.map_or(log.len(), <[usize]>::len);
    // A view belongs to one execution, whose log only grows, and the runtime resets the view when
    // an apply starts.
    debug_assert!(
        measured.entries <= total,
        "measured {} log entries but only {total} exist",
        measured.entries
    );
    let entry_width =
        |line: &ExecutionLogLine| line.text.lines().map(shown_width).max().unwrap_or_default();
    let width = entries.map_or_else(
        || {
            log[measured.entries..]
                .iter()
                .map(entry_width)
                .fold(measured.width, usize::max)
        },
        |entries| {
            entries[measured.entries..]
                .iter()
                .filter_map(|id| log.get(*id))
                .map(entry_width)
                .fold(measured.width, usize::max)
        },
    );
    LogWidth {
        entries: total,
        width,
    }
}

// Up to `height` rows of `log`, starting `skip` rows into it, each with the stream of the entry
// it comes from. Rows stay unformatted so a window costs formatting only for the rows it keeps.
fn log_rows<'a>(
    log: impl IntoIterator<Item = &'a ExecutionLogLine>,
    skip: usize,
    height: usize,
) -> impl Iterator<Item = (EventStream, &'a str)> {
    log.into_iter()
        .flat_map(|line| line.text.lines().map(move |text| (line.stream, text)))
        .skip(skip)
        .take(height)
}

fn log_line(stream: EventStream, text: &str, horizontal: usize, width: usize) -> Line<'_> {
    let style = if stream == EventStream::Stderr {
        theme::warning_style()
    } else {
        theme::body_style()
    };
    Line::from(Span::styled(visible_cells(text, horizontal, width), style))
}

fn target_line(
    state: &ExecutionState,
    index: usize,
    selected: Option<usize>,
    now: Instant,
    columns: &TargetColumns,
) -> Line<'static> {
    let target = &state.progress().targets()[index];
    let marker = if selected == Some(index) { "> " } else { "  " };
    let elapsed = target
        .elapsed_at(now)
        .map_or_else(|| "--".to_owned(), format_elapsed);
    let previous = target
        .previous()
        .map_or_else(|| "--".to_owned(), format_elapsed);
    let text = columns.row(
        marker,
        target.address(),
        [
            target_status_label(target.status()),
            &target_action_label(target),
            &elapsed,
            &previous,
        ],
    );
    let style = if selected == Some(index) {
        theme::accent_style().add_modifier(ratatui::style::Modifier::BOLD)
    } else {
        match target.status() {
            ExecutionTargetStatus::Failed => theme::error_style(),
            ExecutionTargetStatus::Completed => theme::success_style(),
            ExecutionTargetStatus::Incomplete | ExecutionTargetStatus::Skipped => {
                theme::warning_style()
            }
            _ => theme::body_style(),
        }
    };
    Line::from(Span::styled(text, style))
}

// Status, action and the durations keep fixed widths so every row lines up; the address gets
// the rest of the panel, but no more than its longest value needs beyond the minimum.
struct TargetColumns {
    address: usize,
    action: usize,
    show_previous: bool,
}

impl TargetColumns {
    // Marker, the gaps between columns, and the fixed-width columns.
    const fn fixed_width(action: usize, show_previous: bool) -> usize {
        let width = 2 + 2 + TARGET_STATUS_WIDTH + 1 + action + 1 + TARGET_ELAPSED_WIDTH;
        if show_previous {
            width + 2 + TARGET_PREVIOUS_WIDTH
        } else {
            width
        }
    }

    fn new(targets: &[ExecutionTargetState], width: usize, show_previous: bool) -> Self {
        let action = targets
            .iter()
            .map(|target| target_action_label(target).len())
            .fold(TARGET_ACTION_WIDTH, usize::max);
        let longest_address = targets
            .iter()
            .map(|target| display_width(target.address()))
            .fold(TARGET_ADDRESS_MIN_WIDTH, usize::max);
        let available = width
            .saturating_sub(Self::fixed_width(action, show_previous))
            .max(TARGET_ADDRESS_MIN_WIDTH);
        Self {
            address: longest_address.min(available),
            action,
            show_previous,
        }
    }

    fn row(
        &self,
        marker: &str,
        address: &str,
        [status, action, elapsed, previous]: [&str; 4],
    ) -> String {
        let address = truncate_middle(address, self.address);
        let padding = self.address.saturating_sub(display_width(&address));
        let address = format!("{address}{}", " ".repeat(padding));
        let action_width = self.action;
        let row = format!(
            "{marker}{address}  {status:<TARGET_STATUS_WIDTH$} {action:<action_width$} {elapsed:>TARGET_ELAPSED_WIDTH$}"
        );
        if self.show_previous {
            format!("{row}  {previous:>TARGET_PREVIOUS_WIDTH$}")
        } else {
            row
        }
    }
}

fn target_action_label(target: &ExecutionTargetState) -> String {
    target
        .actions()
        .iter()
        .map(plan_action_label)
        .collect::<Vec<_>>()
        .join("/")
}

const fn target_status_label(status: ExecutionTargetStatus) -> &'static str {
    match status {
        ExecutionTargetStatus::Pending => "Pending",
        ExecutionTargetStatus::Running => "Running",
        ExecutionTargetStatus::Completed => "Completed",
        ExecutionTargetStatus::Failed => "Failed",
        ExecutionTargetStatus::Skipped => "Skipped",
        ExecutionTargetStatus::Incomplete => "Incomplete",
    }
}

const fn plan_action_label(action: &PlanAction) -> &'static str {
    match action {
        PlanAction::Create => "create",
        PlanAction::Read => "read",
        PlanAction::Update => "update",
        PlanAction::Delete => "delete",
        PlanAction::NoOp => "no-op",
        PlanAction::Unknown(_) => "unknown",
    }
}

fn apply_status_lines(state: &ExecutionState, now: Instant) -> Vec<Line<'static>> {
    let progress = state.progress();
    let stage_label = match state.stage() {
        ExecutionStage::ApplySucceeded => "Apply complete",
        ExecutionStage::ApplyFailed => "Apply failed",
        ExecutionStage::ApplyInterrupted => "Apply interrupted",
        ExecutionStage::Applying if state.is_cancelling() => "Stopping...",
        ExecutionStage::Applying => "Applying...",
        _ => "Apply",
    };
    let stage_style = match state.stage() {
        ExecutionStage::ApplySucceeded => theme::success_style(),
        ExecutionStage::ApplyFailed => theme::error_style(),
        ExecutionStage::ApplyInterrupted => theme::warning_style(),
        _ => theme::body_style(),
    };
    let summary = format!(
        "    Completed: {}/{}    Elapsed: {}",
        progress.completed_count(),
        progress.targets().len(),
        format_elapsed(state.elapsed_at(now)),
    );
    let counts = format!(
        "Failed: {}    Incomplete: {}    Skipped: {}",
        progress.failed_count(),
        progress.incomplete_count(),
        progress.skipped_count(),
    );
    let warning = state.is_cancelling()
        || matches!(
            state.stage(),
            ExecutionStage::ApplyFailed | ExecutionStage::ApplyInterrupted
        );
    let mut detail = vec![Span::styled(counts, theme::secondary_style())];
    if progress.has_previous() {
        detail.push(Span::styled(
            "    Previous: local success",
            theme::secondary_style(),
        ));
    }
    let mut lines = vec![
        Line::from(vec![
            Span::styled(stage_label, stage_style),
            Span::raw(summary),
        ]),
        Line::from(detail),
    ];
    if warning {
        lines.push(Line::from(Span::styled(
            "Changes may already be applied.",
            theme::warning_style(),
        )));
    }
    lines
}

fn status_lines(state: &ExecutionState, now: Instant) -> Vec<Line<'static>> {
    let status = if state.is_cancelling() {
        Line::from("Stopping...")
    } else {
        match state.stage() {
            ExecutionStage::Reading => running_status_line("Reading plan...", state, now),
            ExecutionStage::Failed => Line::from(state.result().map_or_else(
                || "Terraform failed.".to_owned(),
                |result| format!("Terraform failed: {:?}", result.termination().status),
            )),
            ExecutionStage::Applying
            | ExecutionStage::ApplySucceeded
            | ExecutionStage::ApplyFailed
            | ExecutionStage::ApplyInterrupted => {
                unreachable!("apply stages should use the apply status")
            }
        }
    };
    vec![
        status,
        Line::from(format!("Waiting {}s", state.waiting_at(now).as_secs())),
        Line::from(format!("Elapsed {}", format_elapsed(state.elapsed_at(now)))),
    ]
}

fn running_status_line(label: &str, state: &ExecutionState, now: Instant) -> Line<'static> {
    let spinner = ['|', '/', '-', '\\']
        [usize::try_from(state.elapsed_at(now).as_millis() / 100).unwrap_or(0) % 4];
    Line::from(vec![
        Span::styled(spinner.to_string(), theme::accent_style()),
        Span::styled(format!(" {label}"), theme::body_style()),
    ])
}

const fn finished_apply(state: &ExecutionState) -> bool {
    matches!(
        state.stage(),
        ExecutionStage::ApplySucceeded
            | ExecutionStage::ApplyFailed
            | ExecutionStage::ApplyInterrupted
    )
}

fn status_paragraph(status: Vec<Line<'static>>, wrap: bool) -> Paragraph<'static> {
    let paragraph = Paragraph::new(status).style(theme::body_style());
    if wrap {
        paragraph.wrap(Wrap { trim: false })
    } else {
        paragraph
    }
}

fn status_line_count(status: &[Line<'static>], width: u16) -> u16 {
    status_paragraph(status.to_vec(), true)
        .line_count(width)
        .try_into()
        .unwrap_or(u16::MAX)
        .max(1)
}

fn footer_lines(state: &ExecutionState, width: u16, notice: Option<&str>) -> Vec<Line<'static>> {
    let items = if state.stage() == ExecutionStage::Failed {
        vec![
            footer::hint(&["q", "Ctrl-C"], "quit"),
            footer::hint(&["y"], "copy diagnostic"),
        ]
    } else {
        vec![footer::hint(&["Ctrl-C"], "cancel")]
    };
    footer::layout_with_notice(items, width, notice)
}

fn apply_footer_lines(
    state: &ExecutionState,
    width: u16,
    notice: Option<&str>,
) -> Vec<Line<'static>> {
    let items = if finished_apply(state) {
        vec![
            footer::hint(&["q", "Ctrl-C"], "quit"),
            footer::hint(&["Tab"], "focus"),
            footer::hint(&["y"], "yank result"),
        ]
    } else {
        vec![
            footer::hint(&["Ctrl-C"], "cancel"),
            footer::hint(&["Tab"], "focus"),
            footer::hint(&["End"], "follow latest"),
        ]
    };
    footer::layout_with_notice(items, width, notice)
}

fn apply_required_footer_lines(
    state: &ExecutionState,
    width: u16,
    notice: Option<&str>,
) -> Vec<Line<'static>> {
    let item = if finished_apply(state) {
        footer::hint(&["q", "Ctrl-C"], "quit")
    } else {
        footer::hint(&["Ctrl-C"], "cancel")
    };
    footer::layout_with_notice(vec![item], width, notice)
}

fn required_footer_lines(
    state: &ExecutionState,
    width: u16,
    notice: Option<&str>,
) -> Vec<Line<'static>> {
    let item = if state.stage() == ExecutionStage::Failed {
        footer::hint(&["q", "Ctrl-C"], "quit")
    } else {
        footer::hint(&["Ctrl-C"], "cancel")
    };
    footer::layout_with_notice(vec![item], width, notice)
}

fn layout_target_max(target_count: usize, height: u16) -> usize {
    target_count.saturating_sub(usize::from(height))
}

fn scroll_limits(line_count: usize, line_width: usize, body: Rect) -> (usize, usize) {
    let vertical = line_count.saturating_sub(usize::from(body.height));
    let horizontal = line_width.saturating_sub(usize::from(body.width));
    (vertical, horizontal)
}

fn initial_scroll(state: &ExecutionState, view: ExecutionViewState, max: usize) -> usize {
    if !matches!(
        state.stage(),
        ExecutionStage::Failed | ExecutionStage::ApplyFailed
    ) {
        return max;
    }

    // A selected target's log holds only its own lines, so the all-logs error line would point
    // at an unrelated line there.
    let selected = view
        .selected_target()
        .and_then(|index| state.progress().targets().get(index));
    selected
        .map_or_else(
            || {
                state
                    .result()
                    .and_then(ExecutionResult::first_error_line)
                    .or_else(|| state.progress().first_error_line())
                    .unwrap_or(max)
            },
            |target| target.first_error_line().unwrap_or(0),
        )
        .min(max)
}

fn format_elapsed(elapsed: Duration) -> String {
    format!(
        "{}.{:01}s",
        elapsed.as_secs(),
        elapsed.subsec_millis() / 100
    )
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use ratatui::{
        buffer::{Buffer, CellWidth},
        style::{Color, Modifier, Style},
    };

    use super::*;
    use crate::app::copy::{CopyResult, CopyTarget};
    use crate::app::execution::{
        ApplyStatus, Diagnostic, DiagnosticSeverity, DiagnosticSource, ExecutionAction,
        ExecutionContext, ExecutionEvent, ExecutionEventKind, ExecutionLogLine,
        ExecutionTargetSpec, ResourceAction, ResourceEvent, ResourceEventKind,
        test_support::log_event,
    };
    use crate::app::session::{self, Action, SessionState};
    use crate::ui::features::execution::ExecutionScroll;
    use crate::ui::test_support::{
        assert_shell_frame_and_footer, buffer_text, render_to_buffer, write_buffer_captures,
    };

    const SIZES: [(u16, u16); 3] = [(80, 24), (120, 40), (160, 60)];
    // The first row count a `u16` can no longer hold.
    const FIRST_BEYOND_U16: usize = u16::MAX as usize + 1;
    const APPLY_LOG: &[(&str, EventStream)] = &[
        ("terraform apply review.tfplan", EventStream::Stdout),
        (
            "terraform_data.api: Modifying... [id=api-20260920]",
            EventStream::Stdout,
        ),
        (
            "terraform_data.api: Modifications complete after 1s [id=api-20260920]",
            EventStream::Stdout,
        ),
        (
            "terraform_data.worker: Replacing... [id=worker-20260920]",
            EventStream::Stdout,
        ),
        (
            "terraform_data.worker: Destruction complete after 1s",
            EventStream::Stdout,
        ),
        (
            "terraform_data.worker: Creation complete after 1s [id=worker-20260920]",
            EventStream::Stdout,
        ),
        (
            "terraform_data.old: Destruction complete after 1s",
            EventStream::Stdout,
        ),
        (
            "terraform_data.new: Creation complete after 1s [id=new-20260920]",
            EventStream::Stdout,
        ),
        (
            "A deliberately long synthetic apply line keeps horizontal scrolling visible in the production renderer",
            EventStream::Stdout,
        ),
    ];
    const SUCCESS_LOG: &[(&str, EventStream)] = &[
        (
            "Warning: synthetic provider emitted a non-blocking diagnostic",
            EventStream::Stderr,
        ),
        ("Apply finished successfully.", EventStream::Stdout),
        ("Outputs: endpoint = synthetic", EventStream::Stdout),
        ("Apply log remains in receive order.", EventStream::Stdout),
    ];

    fn render_execution_with_view(
        frame: &mut Frame<'_>,
        state: &ExecutionState,
        view: ExecutionViewState,
        now: Instant,
    ) {
        super::render_execution_with_quit_confirmation(frame, state, view, now, false);
    }

    fn execution_layout(area: Rect, state: &ExecutionState) -> ExecutionLayout {
        execution_layout_with_view(area, state, ExecutionViewState::default())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the fixture covers the complete apply result event stream"
    )]
    fn apply_state(status: ApplyStatus) -> (ExecutionState, Instant) {
        let started_at = Instant::now();
        let finished_at = started_at + Duration::from_secs(4);
        let mut state = ExecutionState::applying_with_previous(
            started_at,
            ExecutionContext::loading("/repo/environments/production/main")
                .with_workspace("default"),
            vec![
                ExecutionTargetSpec {
                    address: "terraform_data.api".to_owned(),
                    actions: vec![PlanAction::Update],
                },
                ExecutionTargetSpec {
                    address: "terraform_data.worker".to_owned(),
                    actions: vec![PlanAction::Delete, PlanAction::Create],
                },
                ExecutionTargetSpec {
                    address: "terraform_data.old".to_owned(),
                    actions: vec![PlanAction::Delete],
                },
                ExecutionTargetSpec {
                    address: "terraform_data.new".to_owned(),
                    actions: vec![PlanAction::Create],
                },
            ],
            Vec::new(),
            &[
                Some(Duration::from_secs(11)),
                None,
                None,
                Some(Duration::from_secs(4)),
            ],
        );
        for (text, stream) in APPLY_LOG {
            state.record(ExecutionEvent {
                received_at: started_at,
                kind: log_event(*stream, (*text).to_owned()),
            });
        }
        for (address, action) in [
            ("terraform_data.api", ResourceAction::Update),
            ("terraform_data.worker", ResourceAction::Delete),
            ("terraform_data.worker", ResourceAction::Create),
            ("terraform_data.old", ResourceAction::Delete),
            ("terraform_data.new", ResourceAction::Create),
        ] {
            state.record(ExecutionEvent {
                received_at: started_at,
                kind: ExecutionEventKind::Resource(ResourceEvent {
                    address: address.to_owned(),
                    kind: ResourceEventKind::ApplyStart,
                    action: Some(action.clone()),
                    message: Some(format!("{address}: {action:?} started")),
                }),
            });
            state.record(ExecutionEvent {
                received_at: started_at + Duration::from_secs(1),
                kind: ExecutionEventKind::Resource(ResourceEvent {
                    address: address.to_owned(),
                    kind: ResourceEventKind::ApplyComplete,
                    action: Some(action),
                    message: Some(format!("{address}: apply complete")),
                }),
            });
        }
        if status == ApplyStatus::Failed {
            state.record(ExecutionEvent {
                received_at: started_at + Duration::from_secs(2),
                kind: ExecutionEventKind::Resource(ResourceEvent {
                    address: "terraform_data.api".to_owned(),
                    kind: ResourceEventKind::ApplyErrored,
                    action: Some(ResourceAction::Update),
                    message: None,
                }),
            });
            state.record(ExecutionEvent {
                received_at: started_at + Duration::from_secs(3),
                kind: ExecutionEventKind::Diagnostic(Diagnostic {
                    severity: DiagnosticSeverity::Error,
                    summary: "AccessDenied: synthetic provider rejected the request".to_owned(),
                    detail: None,
                    address: Some("terraform_data.api".to_owned()),
                    position: None,
                    source: DiagnosticSource::Terraform,
                }),
            });
        }
        if status == ApplyStatus::Succeeded {
            for (text, stream) in SUCCESS_LOG {
                state.record(ExecutionEvent {
                    received_at: started_at,
                    kind: log_event(*stream, (*text).to_owned()),
                });
            }
        }
        state.finish_apply(
            status,
            (status == ApplyStatus::Succeeded)
                .then(|| "Resources: 2 added, 2 changed, 1 destroyed.".to_owned()),
            None,
            finished_at,
        );
        (state, finished_at)
    }

    fn long_apply_state(status: ApplyStatus) -> (ExecutionState, Instant) {
        let started_at = Instant::now();
        let finished_at = started_at + Duration::from_secs(4);
        let mut state = ExecutionState::applying(
            started_at,
            ExecutionContext::loading("/repo/environments/production/main")
                .with_workspace("default"),
        );
        for index in 0..40 {
            let text = match index {
                0 => "a deliberately long synthetic apply line keeps horizontal scrolling visible after the result is complete".to_owned(),
                1 => "Warning: synthetic provider emitted a non-blocking diagnostic".to_owned(),
                3 => "Error: initial failure".to_owned(),
                39 => "tail marker".to_owned(),
                _ => format!("log line {index}"),
            };
            let stream = if index == 1 {
                EventStream::Stderr
            } else {
                EventStream::Stdout
            };
            state.record(ExecutionEvent {
                received_at: started_at,
                kind: output_event(stream, text),
            });
        }
        state.finish_apply(
            status,
            None,
            (status == ApplyStatus::Failed).then(|| "apply failed".to_owned()),
            finished_at,
        );
        (state, finished_at)
    }

    fn snapshot(name: &str, buffer: &Buffer) {
        insta::assert_snapshot!(name.to_string(), buffer_text(buffer));
        write_buffer_captures(name, buffer);
    }

    #[test]
    fn renders_apply_success_at_all_supported_sizes() {
        for &(width, height) in &SIZES {
            let (state, now) = apply_state(ApplyStatus::Succeeded);
            let buffer = render_to_buffer((width, height), |frame| {
                render_execution_with_view(frame, &state, ExecutionViewState::default(), now);
            });

            snapshot(&format!("preview_{width}x{height}_apply-success"), &buffer);
        }
    }

    #[test]
    fn renders_apply_failure_at_all_supported_sizes() {
        for &(width, height) in &SIZES {
            let (state, now) = apply_state(ApplyStatus::Failed);
            let buffer = render_to_buffer((width, height), |frame| {
                render_execution_with_view(frame, &state, ExecutionViewState::default(), now);
            });

            snapshot(&format!("preview_{width}x{height}_apply-failure"), &buffer);
        }
    }

    #[test]
    fn renders_apply_progress_stopping_and_log_view_vrt_at_all_supported_sizes() {
        // 160x60 differs from 120x40 only in border and blank space, so it has no snapshot here.
        for (width, height) in [(80, 24), (120, 40)] {
            let (state, now) = applying_state_with_content(6, 16);
            let compact = render_to_buffer((width, height), |frame| {
                render_execution_with_view(frame, &state, ExecutionViewState::default(), now);
            });
            snapshot(&format!("ux12r_{width}x{height}_apply-progress"), &compact);

            let mut stopping_state = state.clone();
            stopping_state.apply(ExecutionAction::RequestCancellation);
            let stopping = render_to_buffer((width, height), |frame| {
                render_execution_with_view(
                    frame,
                    &stopping_state,
                    ExecutionViewState::default(),
                    now,
                );
            });
            snapshot(&format!("ux12r_{width}x{height}_apply-stopping"), &stopping);

            let mut view = ExecutionViewState::default();
            view.open_logs();
            let logs = render_to_buffer((width, height), |frame| {
                render_execution_with_view(frame, &state, view, now);
            });
            snapshot(&format!("ux12r_{width}x{height}_apply-logs"), &logs);
        }
    }

    #[test]
    fn renders_plan_progress_and_failure() {
        let (running, now) = plan_state(&["reading output"]);
        let mut failed = running.clone();
        failed.fail("synthetic plan failure".to_owned(), now);

        for (name, state) in [("plan-progress", &running), ("plan-failure", &failed)] {
            let buffer = render_to_buffer((80, 24), |frame| {
                render_execution_with_view(frame, state, ExecutionViewState::default(), now);
            });
            snapshot(&format!("preview_80x24_{name}"), &buffer);
        }
    }

    #[test]
    fn apply_stopping_at_minimum_size_shows_resize_notice() {
        let (state, now) = applying_state_with_content(1, 1);
        let mut stopping_state = state;
        stopping_state.apply(ExecutionAction::RequestCancellation);
        let buffer = render_to_buffer((32, 9), |frame| {
            render_execution_with_view(frame, &stopping_state, ExecutionViewState::default(), now);
        });

        snapshot("ux12r_32x9_apply-stopping", &buffer);
        let text = buffer_text(&buffer);
        assert!(text.contains("Terminal too small."));
        assert!(!text.contains("Stopping..."));
    }

    fn find_text_cell<'a>(buffer: &'a Buffer, area: Rect, text: &str) -> &'a ratatui::buffer::Cell {
        for y in area.y..area.bottom() {
            let symbols = (area.x..area.right())
                .map(|x| buffer.cell((x, y)).expect("execution cell").symbol())
                .collect::<Vec<_>>();
            let Some(start) = (0..symbols.len()).find(|&start| {
                symbols[start..]
                    .iter()
                    .copied()
                    .collect::<String>()
                    .starts_with(text)
            }) else {
                continue;
            };
            return buffer
                .cell((area.x + u16::try_from(start).expect("execution offset"), y))
                .expect("execution cell");
        }
        panic!("text should be visible: {text}");
    }

    fn applying_state_with_content(line_count: u16, line_width: u16) -> (ExecutionState, Instant) {
        let text = "x".repeat(usize::from(line_width));
        applying_state_with_lines(vec![text; usize::from(line_count)])
    }

    fn applying_state_with_lines(lines: Vec<String>) -> (ExecutionState, Instant) {
        let now = Instant::now();
        let mut state = ExecutionState::applying(now, ExecutionContext::loading("/repo"));
        for text in lines {
            state.record(ExecutionEvent {
                received_at: now,
                kind: log_event(EventStream::Stdout, text),
            });
        }
        (state, now)
    }

    fn plan_state(lines: &[&str]) -> (ExecutionState, Instant) {
        let started_at = Instant::now();
        let mut state = ExecutionState::with_context(
            started_at,
            ExecutionContext::loading("/repo/environments/production/main")
                .with_workspace("default"),
        );
        for text in lines {
            state.record(ExecutionEvent {
                received_at: started_at,
                kind: output_event(EventStream::Stdout, (*text).to_owned()),
            });
        }
        (state, started_at + Duration::from_secs(2))
    }

    // Terraform reports errors as error diagnostics, which mark the first error line.
    fn output_event(stream: EventStream, text: String) -> ExecutionEventKind {
        if text.starts_with("Error:") {
            ExecutionEventKind::Diagnostic(Diagnostic {
                severity: DiagnosticSeverity::Error,
                summary: text,
                detail: None,
                address: None,
                position: None,
                source: DiagnosticSource::Terraform,
            })
        } else {
            log_event(stream, text)
        }
    }

    mod targets {
        use super::*;

        const LONG_ADDRESS: &str = "module.synthetic_platform.module.regional_workers[\"region-a\"].terraform_data.worker_pool_configuration";

        fn long_address_state() -> (ExecutionState, Instant) {
            let started_at = Instant::now();
            let mut state = ExecutionState::applying_with_previous(
                started_at,
                ExecutionContext::loading("/repo/environments/production/main")
                    .with_workspace("default"),
                vec![
                    ExecutionTargetSpec {
                        address: "terraform_data.api".to_owned(),
                        actions: vec![PlanAction::Update],
                    },
                    ExecutionTargetSpec {
                        address: LONG_ADDRESS.to_owned(),
                        actions: vec![PlanAction::Delete, PlanAction::Create],
                    },
                    ExecutionTargetSpec {
                        address: "terraform_data.new".to_owned(),
                        actions: vec![PlanAction::Create],
                    },
                ],
                Vec::new(),
                &[Some(Duration::from_secs(11)), None, None],
            );
            for (address, action) in [
                ("terraform_data.api", ResourceAction::Update),
                (LONG_ADDRESS, ResourceAction::Delete),
            ] {
                state.record(ExecutionEvent {
                    received_at: started_at,
                    kind: ExecutionEventKind::Resource(ResourceEvent {
                        address: address.to_owned(),
                        kind: ResourceEventKind::ApplyStart,
                        action: Some(action),
                        message: None,
                    }),
                });
            }
            state.record(ExecutionEvent {
                received_at: started_at + Duration::from_secs(1),
                kind: ExecutionEventKind::Resource(ResourceEvent {
                    address: "terraform_data.api".to_owned(),
                    kind: ResourceEventKind::ApplyComplete,
                    action: Some(ResourceAction::Update),
                    message: None,
                }),
            });
            (state, started_at + Duration::from_secs(2))
        }

        // Rows of the target table, from its header to the last target, as characters.
        fn table_rows(buffer: &Buffer, target_count: usize) -> Vec<Vec<char>> {
            let text = buffer_text(buffer);
            let lines = text.lines().collect::<Vec<_>>();
            let header = lines
                .iter()
                .position(|line| line.contains("  Resource "))
                .expect("target header should be visible");
            lines[header..=header + target_count]
                .iter()
                .map(|line| line.chars().collect())
                .collect()
        }

        fn column(row: &[char], name: &str) -> usize {
            let row = row.iter().collect::<String>();
            let byte = row.find(name).expect("column header should be visible");
            row[..byte].chars().count()
        }

        #[test]
        fn renders_long_addresses_at_all_supported_sizes() {
            for &(width, height) in &SIZES {
                let (state, now) = long_address_state();
                let buffer = render_to_buffer((width, height), |frame| {
                    render_execution_with_view(frame, &state, ExecutionViewState::default(), now);
                });

                snapshot(
                    &format!("preview_{width}x{height}_apply-long-address"),
                    &buffer,
                );
            }
        }

        #[test]
        fn target_columns_line_up_and_addresses_shrink_only_when_the_panel_is_narrow() {
            let mut long_address_texts = Vec::new();
            for (long_addresses, fixture, target_count) in [
                (false, apply_state(ApplyStatus::Succeeded), 4),
                (true, long_address_state(), 3),
            ] {
                let (state, now) = fixture;
                for &(width, height) in &SIZES {
                    let buffer = render_to_buffer((width, height), |frame| {
                        render_execution_with_view(
                            frame,
                            &state,
                            ExecutionViewState::default(),
                            now,
                        );
                    });
                    if long_addresses {
                        long_address_texts.push(buffer_text(&buffer));
                    }
                    let rows = table_rows(&buffer, target_count);
                    let header = &rows[0];
                    let status = column(header, "Status");
                    let action = column(header, "Action");
                    let elapsed_end = column(header, "Elapsed") + "Elapsed".len();
                    let previous_end = column(header, "Previous") + "Previous".len();

                    for row in &rows[1..] {
                        let row_text = row.iter().collect::<String>();
                        assert_eq!(row[status - 1], ' ', "{width}: {row_text}");
                        assert_ne!(row[status], ' ', "{width}: {row_text}");
                        assert_eq!(row[action - 1], ' ', "{width}: {row_text}");
                        assert_ne!(row[action], ' ', "{width}: {row_text}");
                        for end in [elapsed_end, previous_end] {
                            assert_ne!(row[end - 1], ' ', "{width}: {row_text}");
                            assert!(matches!(row[end], ' ' | '│'), "{width}: {row_text}");
                        }
                    }
                }
            }

            // Only the widest panel has room for the whole address.
            for text in &long_address_texts[..2] {
                assert!(!text.contains(LONG_ADDRESS), "{text}");
                let row = text
                    .lines()
                    .find(|line| line.contains("module.synthe"))
                    .expect("long address row should be visible");
                assert!(row.contains("...") && row.contains("onfiguration"), "{row}");
            }
            assert!(
                long_address_texts[2].contains(LONG_ADDRESS),
                "{}",
                long_address_texts[2]
            );
            assert!(long_address_texts[2].contains("terraform_data.api"));
        }

        #[test]
        fn wide_character_addresses_fit_whole_and_keep_the_columns_aligned() {
            let wide_address = "terraform_data.x[\"ｶﾞ\"]";
            let state = ExecutionState::applying_with_previous(
                Instant::now(),
                ExecutionContext::loading("/repo"),
                vec![
                    ExecutionTargetSpec {
                        address: "terraform_data.api".to_owned(),
                        actions: vec![PlanAction::Update],
                    },
                    ExecutionTargetSpec {
                        address: wide_address.to_owned(),
                        actions: vec![PlanAction::Create],
                    },
                ],
                Vec::new(),
                &[None, None],
            );
            let area = Rect::new(0, 0, 160, 60);
            let body = execution_layout(area, &state).target_body();
            let buffer = render_to_buffer((area.width, area.height), |frame| {
                render_execution_with_view(
                    frame,
                    &state,
                    ExecutionViewState::default(),
                    Instant::now(),
                );
            });
            // Cells of a row from `x`, with the cell a wide character covers left out.
            let cells = |x: u16, y: u16| {
                let mut text = String::new();
                let mut x = x;
                while x < body.right() {
                    let symbol = buffer[(x, y)].symbol();
                    text.push_str(symbol);
                    x += u16::try_from(display_width(symbol).max(1)).expect("cell width");
                }
                text
            };
            let header = cells(body.x, body.y - 1);
            let status_x = body.x
                + u16::try_from(header.find("Status").expect("status header")).expect("status");

            assert!(cells(body.x, body.y + 1).starts_with(&format!("  {wide_address}")));
            for y in [body.y, body.y + 1] {
                assert_eq!(buffer[(status_x - 1, y)].symbol(), " ");
                assert!(
                    cells(status_x, y).starts_with("Pending "),
                    "{}",
                    cells(body.x, y)
                );
            }
        }

        #[test]
        fn fixed_target_columns_fit_every_label() {
            for status in [
                ExecutionTargetStatus::Pending,
                ExecutionTargetStatus::Running,
                ExecutionTargetStatus::Completed,
                ExecutionTargetStatus::Failed,
                ExecutionTargetStatus::Skipped,
                ExecutionTargetStatus::Incomplete,
            ] {
                assert!(target_status_label(status).len() <= TARGET_STATUS_WIDTH);
            }
            for actions in [
                vec![PlanAction::Delete, PlanAction::Create],
                vec![PlanAction::Create, PlanAction::Delete],
                vec![PlanAction::NoOp],
                vec![PlanAction::Unknown("forget".to_owned())],
            ] {
                let label = actions
                    .iter()
                    .map(plan_action_label)
                    .collect::<Vec<_>>()
                    .join("/");
                assert!(label.len() <= TARGET_ACTION_WIDTH, "{label}");
            }
        }
    }

    mod layout {
        use super::*;

        fn execution_layout_with_quit_confirmation(
            area: Rect,
            state: &ExecutionState,
            quit_confirmation: bool,
        ) -> ExecutionLayout {
            super::execution_layout_with_quit_confirmation_and_view(
                area,
                state,
                ExecutionViewState::default(),
                quit_confirmation,
            )
        }

        fn assert_text_uses_style(buffer: &Buffer, text: &str, color: Color, modifier: Modifier) {
            let area = buffer.area();
            for y in area.y..area.bottom() {
                let symbols = (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).expect("execution cell").symbol())
                    .collect::<Vec<_>>();
                let Some(start) = (0..symbols.len()).find(|&start| {
                    symbols[start..]
                        .iter()
                        .copied()
                        .collect::<String>()
                        .starts_with(text)
                }) else {
                    continue;
                };
                for offset in 0..text.chars().count() {
                    let cell = buffer
                        .cell((
                            area.x + u16::try_from(start + offset).expect("execution offset"),
                            y,
                        ))
                        .expect("execution cell");
                    assert_eq!(cell.fg, color, "{text}");
                    assert!(cell.modifier.contains(modifier), "{text}");
                }
                return;
            }
            panic!("text should be visible: {text}");
        }

        fn execution_buffer_at(
            area: Rect,
            state: &ExecutionState,
            now: Instant,
            vertical: usize,
            horizontal: usize,
        ) -> (ExecutionLayout, Buffer) {
            let mut view = ExecutionViewState::default();
            view.open_logs();
            let layout = execution_layout_with_view(area, state, view);
            let mut current_vertical = 0;
            view.apply_scroll(
                ExecutionScroll::Top,
                current_vertical,
                layout.max_vertical(),
                layout.body().height,
            );
            for _ in 0..vertical {
                view.apply_scroll(
                    ExecutionScroll::Down,
                    current_vertical,
                    layout.max_vertical(),
                    layout.body().height,
                );
                current_vertical = current_vertical
                    .saturating_add(1)
                    .min(layout.max_vertical());
            }
            let mut current_horizontal = 0;
            view.apply_horizontal_scroll(
                ExecutionScroll::LeftEdge,
                current_horizontal,
                layout.max_horizontal(),
                current_vertical,
            );
            for _ in 0..horizontal {
                view.apply_horizontal_scroll(
                    ExecutionScroll::Right,
                    current_horizontal,
                    layout.max_horizontal(),
                    current_vertical,
                );
                current_horizontal = current_horizontal
                    .saturating_add(1)
                    .min(layout.max_horizontal());
            }
            let buffer = render_to_buffer((area.width, area.height), |frame| {
                render_execution_with_view(frame, state, view, now);
            });
            (layout, buffer)
        }

        fn assert_scrollbar_positions(
            buffer: &Buffer,
            layout: &ExecutionLayout,
            vertical: usize,
            horizontal: usize,
        ) {
            let body = layout.body();
            if layout.vertical_scrollbar() {
                let height = body.height + u16::from(layout.horizontal_scrollbar());
                let symbols = (body.y..body.y + height)
                    .map(|y| {
                        buffer
                            .cell((body.x + body.width, y))
                            .expect("vertical cell")
                            .symbol()
                            .to_owned()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(symbols.first().map(String::as_str), Some("▲"));
                assert_thumb_endpoints(
                    &symbols[1..symbols.len() - 1],
                    "┃",
                    vertical,
                    layout.max_vertical(),
                );
            }
            if layout.horizontal_scrollbar() {
                let width = body.width + u16::from(layout.vertical_scrollbar());
                let symbols = (body.x..body.x + width)
                    .map(|x| {
                        buffer
                            .cell((x, body.y + body.height))
                            .expect("horizontal cell")
                            .symbol()
                            .to_owned()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(symbols.first().map(String::as_str), Some("◀︎"));
                let track_end = if symbols.last().is_some_and(|symbol| symbol == "▶︎") {
                    symbols.len() - 1
                } else {
                    symbols.len()
                };
                assert_thumb_endpoints(
                    &symbols[1..track_end],
                    "═",
                    horizontal,
                    layout.max_horizontal(),
                );
            }
        }

        fn assert_thumb_endpoints(
            track: &[String],
            thumb_symbol: &str,
            position: usize,
            max_position: usize,
        ) {
            let thumb_start = track
                .iter()
                .position(|symbol| symbol == thumb_symbol)
                .expect("scrollbar should contain a thumb");
            let thumb_end = track
                .iter()
                .rposition(|symbol| symbol == thumb_symbol)
                .expect("scrollbar should contain a thumb");
            if position == 0 {
                assert_eq!(thumb_start, 0);
            } else {
                assert!(thumb_start > 0);
            }
            if position == max_position {
                assert_eq!(thumb_end, track.len() - 1);
            } else {
                assert!(thumb_end < track.len() - 1);
            }
        }

        #[test]
        fn quit_confirmation_replaces_the_result_footer_and_has_a_narrow_notice() {
            let (state, now) = apply_state(ApplyStatus::Succeeded);
            let buffer = render_to_buffer((80, 24), |frame| {
                render_execution_with_quit_confirmation(
                    frame,
                    &state,
                    ExecutionViewState::default(),
                    now,
                    true,
                );
            });
            let text = buffer_text(&buffer);
            assert!(text.contains("Quit Terraleph?   [Enter] Quit   [Esc] Cancel"));
            assert!(!text.contains("q/Ctrl-C quit"));
            snapshot("preview_80x24_quit-confirmation", &buffer);

            let narrow = render_to_buffer((32, 9), |frame| {
                render_execution_with_quit_confirmation(
                    frame,
                    &state,
                    ExecutionViewState::default(),
                    now,
                    true,
                );
            });
            assert!(buffer_text(&narrow).contains("Quit? Enter exit / Esc cancel"));
        }

        #[test]
        fn narrow_quit_confirmation_keeps_its_prompt_while_a_copy_notice_is_active() {
            let (mut state, now) = apply_state(ApplyStatus::Succeeded);
            state
                .copy_feedback_mut()
                .record(CopyResult::SentToTerminal, now, true);
            let render_text = |quit_confirmation| {
                buffer_text(&render_to_buffer((40, 24), |frame| {
                    render_execution_with_quit_confirmation(
                        frame,
                        &state,
                        ExecutionViewState::default(),
                        now,
                        quit_confirmation,
                    );
                }))
            };

            let copied = render_text(false);
            let confirmation = render_text(true);

            assert!(copied.contains("Sent to terminal clipboard."), "{copied}");
            assert!(
                confirmation.contains("Quit? [Enter] quit [Esc] cancel"),
                "{confirmation}"
            );
            assert!(
                !confirmation.contains("Sent to terminal clipboard."),
                "{confirmation}"
            );
        }

        #[test]
        fn quit_confirmation_preserves_the_execution_body_and_scroll_limits() {
            let (state, _) = apply_state(ApplyStatus::Succeeded);
            let area = Rect::new(0, 0, 50, 24);
            let normal = execution_layout(area, &state);
            let waiting = execution_layout_with_quit_confirmation(area, &state, true);

            assert_eq!(waiting.body(), normal.body());
            assert_eq!(waiting.max_vertical(), normal.max_vertical());
            assert_eq!(waiting.max_horizontal(), normal.max_horizontal());
        }

        #[test]
        fn production_execution_render_draws_shell_scrollbars_and_stream_colors() {
            let (state, now) = long_apply_state(ApplyStatus::Succeeded);
            let area = Rect::new(0, 0, 80, 24);
            let layout = execution_layout(area, &state);
            let buffer = render_to_buffer((area.width, area.height), |frame| {
                render_execution_with_view(frame, &state, ExecutionViewState::default(), now);
            });
            let mut top_view = ExecutionViewState::default();
            top_view.apply_scroll(
                ExecutionScroll::Top,
                0,
                layout.max_vertical(),
                layout.body().height,
            );
            let top_buffer = render_to_buffer((area.width, area.height), |frame| {
                render_execution_with_view(frame, &state, top_view, now);
            });

            assert_shell_frame_and_footer(
                &buffer,
                layout.shell.content(),
                layout.shell.footer(),
                "y yank result",
            );
            let text = buffer_text(&buffer);
            assert!(text.contains("Apply result"));
            assert!(text.contains("Apply complete"));
            assert!(layout.vertical_scrollbar());
            assert!(layout.horizontal_scrollbar());
            let body = layout.body();
            let vertical_x = body.x.saturating_add(body.width);
            let horizontal_y = body.y.saturating_add(body.height);
            let horizontal_end_x = vertical_x;
            assert_eq!(buffer[(vertical_x, body.y)].symbol(), "▲");
            assert_eq!(
                buffer[(vertical_x, body.y)].fg,
                Color::Rgb(0xc0, 0xb8, 0xb0)
            );
            assert_eq!(buffer[(body.x, horizontal_y)].symbol(), "◀︎");
            assert_eq!(
                buffer[(body.x, horizontal_y)].fg,
                Color::Rgb(0x50, 0x52, 0x5e)
            );
            assert_eq!(buffer[(horizontal_end_x, horizontal_y)].symbol(), "▶︎");
            assert_eq!(
                buffer[(horizontal_end_x, horizontal_y)].fg,
                Color::Rgb(0xc0, 0xb8, 0xb0)
            );
            assert_text_uses_style(
                &top_buffer,
                "Warning: synthetic provider emitted a non-blocking diagnostic",
                Color::Rgb(0xeb, 0xcb, 0x8b),
                Modifier::BOLD,
            );
        }

        #[test]
        fn production_execution_scrollbars_reach_offsets_after_resize_and_single_overflow() {
            let (state, now) = long_apply_state(ApplyStatus::Succeeded);
            let mut previous_body = None;
            for area in [Rect::new(0, 0, 80, 24), Rect::new(0, 0, 88, 24)] {
                let layout = execution_layout(area, &state);
                assert!(layout.vertical_scrollbar());
                assert!(layout.horizontal_scrollbar());
                assert!(layout.max_vertical() > 1);
                assert!(layout.max_horizontal() > 1);
                assert_ne!(previous_body, Some(layout.body()));
                previous_body = Some(layout.body());

                for (vertical, horizontal) in [
                    (0, 0),
                    (layout.max_vertical() / 2, layout.max_horizontal() / 2),
                    (layout.max_vertical(), layout.max_horizontal()),
                ] {
                    let (layout, buffer) =
                        execution_buffer_at(area, &state, now, vertical, horizontal);
                    assert_scrollbar_positions(&buffer, &layout, vertical, horizontal);
                }
            }

            let area = Rect::new(0, 0, 80, 40);
            let (base_state, _) = applying_state_with_content(1, 1);
            let mut logs_view = ExecutionViewState::default();
            logs_view.open_logs();
            let available = execution_layout_with_view(area, &base_state, logs_view).log_area();

            let (vertical_state, vertical_now) = applying_state_with_content(
                available.height.saturating_add(1),
                available.width.saturating_sub(1),
            );
            let (vertical_layout, vertical_buffer) =
                execution_buffer_at(area, &vertical_state, vertical_now, 1, 0);
            assert_eq!(vertical_layout.max_vertical(), 1);
            assert!(!vertical_layout.horizontal_scrollbar());
            assert_scrollbar_positions(&vertical_buffer, &vertical_layout, 1, 0);

            let (horizontal_state, horizontal_now) = applying_state_with_content(
                available.height.saturating_sub(1),
                available.width.saturating_add(1),
            );
            let (horizontal_layout, horizontal_buffer) =
                execution_buffer_at(area, &horizontal_state, horizontal_now, 0, 1);
            assert_eq!(horizontal_layout.max_horizontal(), 1);
            assert!(!horizontal_layout.vertical_scrollbar());
            assert_scrollbar_positions(&horizontal_buffer, &horizontal_layout, 0, 1);
        }

        #[test]
        fn apply_log_scrollbars_stay_inside_the_panel_at_reserved_width_boundaries() {
            struct ReservationCase {
                name: &'static str,
                extra_lines: i32,
                width_delta: i32,
                fill: char,
                bars: (bool, bool),
            }

            let area = Rect::new(0, 0, 80, 24);
            let (base_state, _) = applying_state_with_content(1, 1);
            let mut logs_view = ExecutionViewState::default();
            logs_view.open_logs();
            let available = execution_layout_with_view(area, &base_state, logs_view).log_area();

            for case in [
                ReservationCase {
                    name: "none_equal",
                    extra_lines: 0,
                    width_delta: 0,
                    fill: 'x',
                    bars: (false, false),
                },
                ReservationCase {
                    name: "horizontal_only_wider",
                    extra_lines: -1,
                    width_delta: 1,
                    fill: 'x',
                    bars: (false, true),
                },
                ReservationCase {
                    name: "horizontal_takes_the_last_row",
                    extra_lines: 0,
                    width_delta: 1,
                    fill: 'x',
                    bars: (true, true),
                },
                ReservationCase {
                    name: "vertical_only_narrower",
                    extra_lines: 1,
                    width_delta: -1,
                    fill: 'x',
                    bars: (true, false),
                },
                ReservationCase {
                    name: "both_equal",
                    extra_lines: 1,
                    width_delta: 0,
                    fill: 'x',
                    bars: (true, true),
                },
                ReservationCase {
                    name: "both_equal_fullwidth",
                    extra_lines: 1,
                    width_delta: 0,
                    fill: 'あ',
                    bars: (true, true),
                },
            ] {
                let line_count = usize::try_from(i32::from(available.height) + case.extra_lines)
                    .expect("line count");
                let line_width = usize::try_from(i32::from(available.width) + case.width_delta)
                    .expect("line width");
                let mut lines = vec![log_line("", line_width, case.fill); line_count - 1];
                lines.push(log_line("tail", line_width, case.fill));
                let (state, now) = applying_state_with_lines(lines);

                let layout = execution_layout_with_view(area, &state, logs_view);
                let buffer = render_to_buffer((area.width, area.height), |frame| {
                    render_execution_with_view(frame, &state, logs_view, now);
                });

                assert_eq!(layout.log_area(), available, "case: {}", case.name);
                assert_eq!(
                    (layout.vertical_scrollbar(), layout.horizontal_scrollbar()),
                    case.bars,
                    "case: {}",
                    case.name
                );
                assert_panel_border(&buffer, layout.log_panel(), case.name);
                assert_log_bar_ends(&buffer, &layout, case.name);
                let body = layout.body();
                let tail_row = body.y
                    + u16::try_from(line_count - 1 - layout.max_vertical()).expect("tail row");
                let tail = (body.x..body.x + 4)
                    .map(|x| buffer[(x, tail_row)].symbol())
                    .collect::<String>();
                assert_eq!(tail, "tail", "case: {}", case.name);
            }
        }

        fn assert_log_bar_ends(buffer: &Buffer, layout: &ExecutionLayout, name: &str) {
            let body = layout.body();
            let (vertical, horizontal) =
                (layout.vertical_scrollbar(), layout.horizontal_scrollbar());
            if vertical {
                assert_eq!(buffer[(body.right(), body.y)].symbol(), "▲", "case: {name}");
            }
            if vertical && !horizontal {
                let end = buffer[(body.right(), body.bottom() - 1)].symbol();
                assert_eq!(end, "▼", "case: {name}");
            }
            if horizontal {
                assert_eq!(
                    buffer[(body.x, body.bottom())].symbol(),
                    "◀︎",
                    "case: {name}"
                );
                let end_x = body.right() - u16::from(!vertical);
                assert_eq!(buffer[(end_x, body.bottom())].symbol(), "▶︎", "case: {name}");
            }
        }

        fn log_line(prefix: &str, width: usize, fill: char) -> String {
            let fill_width = if fill.is_ascii() { 1 } else { 2 };
            let remaining = width - prefix.len();
            let mut line = prefix.to_owned();
            line.extend(std::iter::repeat_n(fill, remaining / fill_width));
            line.extend(std::iter::repeat_n('x', remaining % fill_width));
            line
        }

        fn assert_panel_border(buffer: &Buffer, panel: Rect, name: &str) {
            let row = |y: u16| {
                (panel.x..panel.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            };
            let horizontal = "─".repeat(usize::from(panel.width - 2));
            let title = "Logs: All logs";
            assert_eq!(
                row(panel.y),
                format!(
                    "┌{title}{}┐",
                    "─".repeat(usize::from(panel.width - 2) - title.len())
                ),
                "case: {name}"
            );
            assert_eq!(
                row(panel.bottom() - 1),
                format!("└{horizontal}┘"),
                "case: {name}"
            );
            for y in panel.y + 1..panel.bottom() - 1 {
                assert_eq!(buffer[(panel.x, y)].symbol(), "│", "case: {name}");
                assert_eq!(buffer[(panel.right() - 1, y)].symbol(), "│", "case: {name}");
            }
        }
    }

    mod large_log {
        use super::*;
        use crate::ui::features::execution::ExecutionTargetMove;

        const TARGET_EVERY: usize = 10;
        // Leaves a window's worth of entries after the first position beyond the `u16` range, and
        // makes the target's last entry fall beyond it too.
        const ENTRY_COUNT: usize = (FIRST_BEYOND_U16 + 100).next_multiple_of(TARGET_EVERY);
        const TARGET: &str = "terraform_data.bulk";

        fn entry_text(entry: usize) -> String {
            if entry.is_multiple_of(TARGET_EVERY) {
                format!("target line {entry:06}")
            } else {
                format!("log line {entry:06}")
            }
        }

        // Every tenth entry is the target's progress message; the rest is unbound output.
        fn large_apply_state() -> (ExecutionState, Instant) {
            let started_at = Instant::now();
            let mut state = ExecutionState::applying_with_previous(
                started_at,
                ExecutionContext::loading("/repo"),
                vec![ExecutionTargetSpec {
                    address: TARGET.to_owned(),
                    actions: vec![PlanAction::Update],
                }],
                Vec::new(),
                &[None],
            );
            for entry in 0..ENTRY_COUNT {
                let kind = if entry.is_multiple_of(TARGET_EVERY) {
                    ExecutionEventKind::Resource(ResourceEvent {
                        address: TARGET.to_owned(),
                        kind: ResourceEventKind::ApplyProgress,
                        action: Some(ResourceAction::Update),
                        message: Some(entry_text(entry)),
                    })
                } else {
                    log_event(EventStream::Stdout, entry_text(entry))
                };
                state.record(ExecutionEvent {
                    received_at: started_at,
                    kind,
                });
            }
            (state, started_at)
        }

        fn body_rows(
            area: Rect,
            state: &ExecutionState,
            view: ExecutionViewState,
            now: Instant,
        ) -> Vec<String> {
            let body = execution_layout_with_view(area, state, view).body();
            let buffer = render_to_buffer((area.width, area.height), |frame| {
                render_execution_with_view(frame, state, view, now);
            });
            (body.y..body.bottom())
                .map(|y| {
                    (body.x..body.right())
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                        .trim_end()
                        .to_owned()
                })
                .collect()
        }

        fn expected_rows(lines: impl Iterator<Item = usize>) -> Vec<String> {
            lines.map(entry_text).collect()
        }

        #[test]
        fn all_logs_follow_the_tail_and_scroll_past_the_u16_range() {
            let (state, now) = large_apply_state();
            let area = Rect::new(0, 0, 80, 24);
            let mut view = ExecutionViewState::default();
            view.open_logs();
            let layout = execution_layout_with_view(area, &state, view);
            let height = usize::from(layout.body().height);
            let max = layout.max_vertical();
            assert_eq!(max, ENTRY_COUNT - height);

            assert_eq!(
                body_rows(area, &state, view, now),
                expected_rows(max..ENTRY_COUNT)
            );

            view.apply_scroll(ExecutionScroll::Top, max, max, layout.body().height);
            assert_eq!(body_rows(area, &state, view, now), expected_rows(0..height));
            view.apply_scroll(ExecutionScroll::PageDown, 0, max, layout.body().height);
            assert_eq!(
                body_rows(area, &state, view, now),
                expected_rows(height..height * 2)
            );
            let after_first_beyond_u16 = FIRST_BEYOND_U16 + 1;
            assert!(max > after_first_beyond_u16);
            view.apply_scroll(
                ExecutionScroll::Down,
                FIRST_BEYOND_U16,
                max,
                layout.body().height,
            );
            assert_eq!(
                execution_scroll_position_with_view(&state, view, &layout),
                (after_first_beyond_u16, max)
            );
            assert_eq!(
                body_rows(area, &state, view, now),
                expected_rows(after_first_beyond_u16..after_first_beyond_u16 + height)
            );
            view.apply_scroll(ExecutionScroll::Up, max, max, layout.body().height);
            assert!(!view.follows_latest());
            assert_eq!(
                body_rows(area, &state, view, now),
                expected_rows(max - 1..ENTRY_COUNT - 1)
            );

            view.end();
            assert_eq!(
                body_rows(area, &state, view, now),
                expected_rows(max..ENTRY_COUNT)
            );
        }

        #[test]
        fn selected_target_shows_only_its_lines_across_the_whole_log() {
            let (state, now) = large_apply_state();
            let area = Rect::new(0, 0, 80, 24);
            let mut view = ExecutionViewState::default();
            view.open_logs();
            view.select_target(ExecutionTargetMove::Next, &[0]);
            let layout = execution_layout_with_view(area, &state, view);
            let height = usize::from(layout.body().height);
            let target_lines = ENTRY_COUNT / TARGET_EVERY;
            let max = layout.max_vertical();
            assert_eq!(max, target_lines - height);
            assert!((target_lines - 1) * TARGET_EVERY > usize::from(u16::MAX));
            let target_rows = |range: std::ops::Range<usize>| {
                expected_rows(range.map(|line| line * TARGET_EVERY))
            };

            assert_eq!(
                body_rows(area, &state, view, now),
                target_rows(max..target_lines)
            );
            view.apply_scroll(ExecutionScroll::Top, max, max, layout.body().height);
            assert_eq!(body_rows(area, &state, view, now), target_rows(0..height));
            view.apply_scroll(ExecutionScroll::Down, 5_000, max, layout.body().height);
            assert_eq!(
                body_rows(area, &state, view, now),
                target_rows(5_001..5_001 + height)
            );
            view.end();
            assert_eq!(
                body_rows(area, &state, view, now),
                target_rows(max..target_lines)
            );
        }

        #[test]
        fn scrolling_can_start_inside_a_multi_line_entry() {
            let started_at = Instant::now();
            let mut state = ExecutionState::applying_with_previous(
                started_at,
                ExecutionContext::loading("/repo"),
                vec![ExecutionTargetSpec {
                    address: TARGET.to_owned(),
                    actions: vec![PlanAction::Update],
                }],
                Vec::new(),
                &[None],
            );
            let mut lines = Vec::new();
            for entry in 0..40 {
                let text = format!("entry {entry} first\nentry {entry} second");
                lines.extend(text.lines().map(str::to_owned));
                state.record(ExecutionEvent {
                    received_at: started_at,
                    kind: ExecutionEventKind::Diagnostic(Diagnostic {
                        severity: DiagnosticSeverity::Warning,
                        summary: format!("entry {entry} first"),
                        detail: Some(format!("entry {entry} second")),
                        address: Some(TARGET.to_owned()),
                        position: None,
                        source: DiagnosticSource::Terraform,
                    }),
                });
            }
            let area = Rect::new(0, 0, 80, 24);
            for selected in [false, true] {
                let mut view = ExecutionViewState::default();
                view.open_logs();
                if selected {
                    view.select_target(ExecutionTargetMove::Next, &[0]);
                }
                let layout = execution_layout_with_view(area, &state, view);
                let height = usize::from(layout.body().height);
                view.apply_scroll(
                    ExecutionScroll::Down,
                    6,
                    layout.max_vertical(),
                    layout.body().height,
                );

                assert_eq!(
                    body_rows(area, &state, view, started_at),
                    lines[7..7 + height],
                    "selected: {selected}"
                );
            }
        }
    }

    mod appended_log {
        use rstest::rstest;

        use super::*;
        use crate::ui::display_text::DisplayColumns;
        use crate::ui::features::execution::ExecutionTargetMove;

        fn record_log(state: &mut ExecutionState, now: Instant, text: &str) {
            state.record(ExecutionEvent {
                received_at: now,
                kind: log_event(EventStream::Stdout, text.to_owned()),
            });
        }

        fn logs_view() -> ExecutionViewState {
            let mut view = ExecutionViewState::default();
            view.open_logs();
            view
        }

        fn body_row(buffer: &Buffer, body: Rect, y: u16) -> String {
            (body.x..body.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        }

        // Scrolled to the right edge, the end of the widest line meets the body's last column.
        fn assert_right_edge(state: &ExecutionState, view: ExecutionViewState, tail: &str) {
            let area = Rect::new(0, 0, 80, 24);
            let layout = execution_layout_with_view(area, state, view);
            let mut view = view;
            let (vertical, _) = execution_scroll_position_with_view(state, view, &layout);
            view.apply_horizontal_scroll(
                ExecutionScroll::RightEdge,
                0,
                layout.max_horizontal(),
                vertical,
            );
            let buffer = render_to_buffer((area.width, area.height), |frame| {
                render_execution_with_view(frame, state, view, Instant::now());
            });
            let body = layout.body();
            let tail_width = u16::try_from(tail.len()).expect("tail width");
            let found = (body.y..body.bottom()).any(|y| {
                (body.right() - tail_width..body.right())
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    == tail
            });
            assert!(found, "{}", buffer_text(&buffer));
        }

        #[test]
        fn halfwidth_voiced_katakana_scrolls_exactly_to_the_right_edge() {
            let (mut state, now) = applying_state_with_lines(vec!["short".to_owned()]);
            record_log(&mut state, now, &format!("{}END", "ｶﾞ".repeat(60)));
            record_log(&mut state, now, "after");
            // Each `ｶﾞ` takes two cells, as the renderer draws it.
            let width = 60 * 2 + 3;

            let unmeasured = logs_view();
            let mut measured = logs_view();
            measured.measure_log(state.progress());
            let area = Rect::new(0, 0, 80, 24);
            for view in [unmeasured, measured] {
                let layout = execution_layout_with_view(area, &state, view);
                assert_eq!(
                    layout.max_horizontal(),
                    width - usize::from(layout.body().width)
                );
                assert_right_edge(&state, view, "END");
            }
        }

        #[test]
        fn a_line_wider_than_u16_cells_scrolls_to_its_right_end() {
            let (mut state, now) = applying_state_with_lines(vec!["short".to_owned()]);
            record_log(&mut state, now, &format!("{}END", "x".repeat(70_000)));
            let view = logs_view();
            let layout = execution_layout_with_view(Rect::new(0, 0, 80, 24), &state, view);

            assert_eq!(
                layout.max_horizontal(),
                70_003 - usize::from(layout.body().width)
            );
            assert!(layout.max_horizontal() > usize::from(u16::MAX));
            assert_right_edge(&state, view, "END");
        }

        #[test]
        fn a_wide_character_cut_by_the_left_edge_leaves_a_blank_in_the_line_style() {
            let (mut state, now) = applying_state_with_lines(vec!["x".repeat(200)]);
            state.record(ExecutionEvent {
                received_at: now,
                kind: log_event(EventStream::Stderr, "ｶﾞabc".to_owned()),
            });
            let area = Rect::new(0, 0, 80, 24);
            let mut view = logs_view();
            let layout = execution_layout_with_view(area, &state, view);
            let (vertical, _) = execution_scroll_position_with_view(&state, view, &layout);
            view.apply_horizontal_scroll(
                ExecutionScroll::Right,
                0,
                layout.max_horizontal(),
                vertical,
            );
            let buffer = render_to_buffer((area.width, area.height), |frame| {
                render_execution_with_view(frame, &state, view, now);
            });

            let body = layout.body();
            let y = (body.y..body.bottom())
                .find(|y| body_row(&buffer, body, *y) == " abc")
                .unwrap_or_else(|| panic!("{}", buffer_text(&buffer)));
            let blank = &buffer[(body.x, y)];
            assert_eq!(blank.style(), buffer[(body.x + 1, y)].style());
            assert_eq!(Some(blank.fg), theme::warning_style().fg);
        }

        // The row the cutter produced before it stopped at the right edge: the whole shown line
        // from `offset` cells in, with a wide character cut by the left edge left blank.
        fn whole_remainder(text: &str, offset: usize) -> String {
            let shown = DisplayColumns::default().show(text);
            let mut skipped = 0;
            let mut kept = String::new();
            for grapheme in Line::from(shown.as_ref()).styled_graphemes(Style::default()) {
                if skipped < offset {
                    skipped += usize::from(grapheme.symbol.cell_width());
                    kept.extend(std::iter::repeat_n(' ', skipped.saturating_sub(offset)));
                } else {
                    kept.push_str(grapheme.symbol);
                }
            }
            kept
        }

        fn draw_row(text: &str, width: u16) -> Buffer {
            render_to_buffer((width, 1), |frame| {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(text, theme::warning_style())))
                        .style(theme::body_style()),
                    frame.area(),
                );
            })
        }

        // A line of `prefix`, `unit` repeated, then `suffix`, at least `cells` cells wide.
        fn line_past(prefix: &str, unit: &str, suffix: &str, cells: usize) -> String {
            let build = |count: usize| format!("{prefix}{}{suffix}", unit.repeat(count));
            let width = |text: String| {
                let log = [ExecutionLogLine {
                    stream: EventStream::Stdout,
                    text,
                }];
                measure_log_width(LogWidth::default(), &log, None).width
            };
            let mut count = 1;
            loop {
                let drawn = width(build(count));
                if drawn >= cells {
                    return build(count);
                }
                count = (count * cells).div_ceil(drawn).max(count + 1);
            }
        }

        #[test]
        fn long_lines_scrolled_right_draw_only_what_fits_as_their_whole_remainder_would() {
            const MAX_OFFSET: usize = 40_001;
            const MAX_WIDTH: u16 = 80;
            // A few cells beyond the widest window's right edge, so it still has to be cut there.
            const PAST_RIGHT_EDGE: usize = 8;
            let reach = MAX_OFFSET + usize::from(MAX_WIDTH) + PAST_RIGHT_EDGE;
            let lines = [
                line_past("", "x", "", reach),
                "aｶﾞ全角b\t😀".repeat(5_000),
                line_past("", "全", "END", reach),
                line_past(
                    "",
                    "\u{1b}[1m全\u{1b}[0m\r\u{301}\u{600}\t🇯🇵\t\u{7f}ｶﾞ\t",
                    "",
                    reach,
                ),
                line_past(&format!("{}\t", "x".repeat(79)), "\ty", "", reach),
            ];
            for text in &lines {
                for width in [1_u16, 2, 3, 7, MAX_WIDTH] {
                    for offset in [0, 1, 2, 3, 5, MAX_OFFSET] {
                        let visible = visible_cells(text, offset, usize::from(width));
                        let case = format!("width {width}, offset {offset}");

                        assert_eq!(
                            draw_row(&visible, width),
                            draw_row(&whole_remainder(text, offset), width),
                            "{case}"
                        );
                        // `Paragraph` clips a line it draws from the left edge as it is.
                        if matches!(visible, Cow::Owned(_)) {
                            assert!(display_width(&visible) <= usize::from(width), "{case}");
                        }
                    }
                }
            }
        }

        #[rstest]
        #[case::voiced_halfwidth_kana("ｶﾞｷﾞ", 4)]
        #[case::arabic_ligature("لا", 2)]
        #[case::tab("a\tb", 9)]
        #[case::escape("abc\x1b[0m", 8)]
        fn measured_widths_match_the_drawn_cells(#[case] text: &str, #[case] drawn: usize) {
            // The marker lands on the first cell after the drawn text.
            let buffer = render_to_buffer((20, 1), |frame| {
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::raw(visible_cells(text, 0, 20)),
                        Span::raw("|"),
                    ])),
                    frame.area(),
                );
            });
            let marker = (0..20)
                .position(|x| buffer[(x, 0)].symbol() == "|")
                .expect("marker should be drawn");
            let log = [ExecutionLogLine {
                stream: EventStream::Stdout,
                text: text.to_owned(),
            }];

            assert_eq!(marker, drawn);
            assert_eq!(
                measure_log_width(LogWidth::default(), &log, None).width,
                drawn
            );
        }

        #[test]
        fn tabs_and_control_characters_are_shown_and_scroll_to_the_right_edge() {
            let (mut state, now) = applying_state_with_lines(vec!["short".to_owned()]);
            for text in [
                "null_resource.build (local-exec): \tgo build\t# compile",
                "null_resource.build (local-exec):  10%\r 50%\r100%",
                "null_resource.build (local-exec): \u{1b}[32mok\u{1b}[0m",
                &format!("{}\tEND", "x".repeat(90)),
            ] {
                record_log(&mut state, now, text);
            }
            let area = Rect::new(0, 0, 80, 40);
            let view = logs_view();
            let layout = execution_layout_with_view(area, &state, view);
            let buffer = render_to_buffer((area.width, area.height), |frame| {
                render_execution_with_view(frame, &state, view, now);
            });
            let body = layout.body();
            let rows = (body.y..body.bottom())
                .map(|y| body_row(&buffer, body, y))
                .collect::<Vec<_>>();
            for shown in [
                "null_resource.build (local-exec):       go build        # compile",
                "null_resource.build (local-exec):  10%^M 50%^M100%",
                "null_resource.build (local-exec): ^[[32mok^[[0m",
            ] {
                assert!(rows.iter().any(|row| row == shown), "{rows:#?}");
            }
            // The tab after 90 cells stops at column 96.
            assert_eq!(layout.max_horizontal(), 99 - usize::from(body.width));
            assert_right_edge(&state, view, "     END");

            state.finish_apply(ApplyStatus::Succeeded, None, None, now);
            let copied = state
                .copy_effect(CopyTarget::Execution)
                .expect("completed apply should be copyable");
            assert!(copied.text().contains("\tgo build\t# compile\n"));
            assert!(copied.text().contains(" 10%\r 50%\r100%\n"));
            assert!(copied.text().contains("\u{1b}[32mok\u{1b}[0m\n"));
        }

        #[test]
        fn measuring_continues_over_new_entries_for_all_logs_and_the_selected_target() {
            let started_at = Instant::now();
            let mut state = ExecutionState::applying_with_previous(
                started_at,
                ExecutionContext::loading("/repo"),
                vec![ExecutionTargetSpec {
                    address: "terraform_data.api".to_owned(),
                    actions: vec![PlanAction::Update],
                }],
                Vec::new(),
                &[None],
            );
            let mut view = logs_view();
            view.select_target(ExecutionTargetMove::Next, &[0]);
            let target_message = |text: &str| {
                ExecutionEventKind::Resource(ResourceEvent {
                    address: "terraform_data.api".to_owned(),
                    kind: ResourceEventKind::ApplyProgress,
                    action: Some(ResourceAction::Update),
                    message: Some(text.to_owned()),
                })
            };
            for text in ["short", "wider target line"] {
                state.record(ExecutionEvent {
                    received_at: started_at,
                    kind: target_message(text),
                });
                record_log(&mut state, started_at, "unbound");
                view.measure_log(state.progress());
            }
            record_log(&mut state, started_at, "a much wider unbound log line");
            view.measure_log(state.progress());

            assert_eq!(
                view.measured_log_width(None),
                LogWidth {
                    entries: 5,
                    width: "a much wider unbound log line".len(),
                }
            );
            assert_eq!(
                view.measured_log_width(Some(0)),
                LogWidth {
                    entries: 2,
                    width: "wider target line".len(),
                }
            );
        }

        #[test]
        fn appended_lines_show_while_following_and_a_manual_position_stays_put() {
            let lines = (0..40).map(|line| format!("line {line}")).collect();
            let (mut state, now) = applying_state_with_lines(lines);
            let area = Rect::new(0, 0, 80, 24);
            let mut following = logs_view();
            following.apply_scroll(ExecutionScroll::Top, 0, 0, 1);
            following.end();
            let layout = execution_layout_with_view(area, &state, following);
            let mut manual = logs_view();
            manual.apply_scroll(
                ExecutionScroll::Top,
                layout.max_vertical(),
                layout.max_vertical(),
                layout.body().height,
            );
            manual.apply_scroll(
                ExecutionScroll::PageDown,
                0,
                layout.max_vertical(),
                layout.body().height,
            );
            let render_rows = |state: &ExecutionState, view: ExecutionViewState| {
                let body = execution_layout_with_view(area, state, view).body();
                let buffer = render_to_buffer((area.width, area.height), |frame| {
                    render_execution_with_view(frame, state, view, now);
                });
                (body.y..body.bottom())
                    .map(|y| body_row(&buffer, body, y))
                    .collect::<Vec<_>>()
            };
            let manual_before = render_rows(&state, manual);
            assert_eq!(
                render_rows(&state, following).last().map(String::as_str),
                Some("line 39")
            );

            for line in 40..43 {
                record_log(&mut state, now, &format!("line {line}"));
                following.measure_log(state.progress());
                manual.measure_log(state.progress());
            }

            let following_after = render_rows(&state, following);
            assert_eq!(
                following_after[following_after.len() - 3..],
                ["line 40", "line 41", "line 42"]
            );
            assert_eq!(render_rows(&state, manual), manual_before);
            assert!(!manual_before.contains(&"line 39".to_owned()));
        }

        #[test]
        fn a_log_window_yields_only_its_raw_rows_across_multi_line_entries() {
            const LINE_COUNT: usize = FIRST_BEYOND_U16 + 10;
            let entry = |stream, text: String| ExecutionLogLine { stream, text };
            let many_lines = (0..LINE_COUNT)
                .map(|line| format!("line {line:06}"))
                .collect::<Vec<_>>()
                .join("\n");
            let log = [
                entry(EventStream::Stdout, "first\nsecond".to_owned()),
                entry(EventStream::Stderr, "third".to_owned()),
                entry(EventStream::Stdout, String::new()),
                entry(EventStream::Stdout, many_lines),
            ];
            let window = |log: &[ExecutionLogLine], skip, height| {
                log_rows(log, skip, height)
                    .map(|(stream, text)| (stream, text.to_owned()))
                    .collect::<Vec<_>>()
            };

            assert_eq!(
                window(&log, 1, 3),
                [
                    (EventStream::Stdout, "second".to_owned()),
                    (EventStream::Stderr, "third".to_owned()),
                    (EventStream::Stdout, "line 000000".to_owned()),
                ]
            );
            assert_eq!(
                window(&log[3..], LINE_COUNT - 10, 20),
                (LINE_COUNT - 10..LINE_COUNT)
                    .map(|line| (EventStream::Stdout, format!("line {line:06}")))
                    .collect::<Vec<_>>()
            );
        }
    }

    mod scroll {
        use super::*;
        use crate::ui::features::execution::ExecutionTargetMove;

        #[test]
        fn reopening_apply_logs_starts_at_the_newest_line() {
            let started_at = Instant::now();
            let mut state =
                ExecutionState::applying(started_at, ExecutionContext::loading("/repo"));
            for text in ["first", "second", "tail"] {
                state.record(ExecutionEvent {
                    received_at: started_at,
                    kind: log_event(EventStream::Stdout, text.to_owned()),
                });
            }

            let mut view = ExecutionViewState::default();
            view.open_logs();
            view.apply_scroll(ExecutionScroll::Top, 0, 2, 1);
            view.close_logs();
            view.open_logs();

            assert!(view.logs_open());
            assert!(view.follows_latest());
            let layout = execution_layout_with_view(Rect::new(0, 0, 80, 24), &state, view);
            assert_eq!(
                view.vertical_offset(0, layout.max_vertical()),
                layout.max_vertical()
            );
        }

        #[test]
        fn cancelling_apply_keeps_the_warning_and_cancel_action_in_both_views() {
            let started_at = Instant::now();
            let mut state =
                ExecutionState::applying(started_at, ExecutionContext::loading("/repo"));
            state.record(ExecutionEvent {
                received_at: started_at,
                kind: log_event(EventStream::Stdout, "Applying saved plan...".to_owned()),
            });
            state.apply(ExecutionAction::RequestCancellation);

            let compact = render_to_buffer((80, 24), |frame| {
                render_execution_with_view(
                    frame,
                    &state,
                    ExecutionViewState::default(),
                    started_at,
                );
            });
            let mut logs_view = ExecutionViewState::default();
            logs_view.open_logs();
            let logs = render_to_buffer((80, 24), |frame| {
                render_execution_with_view(frame, &state, logs_view, started_at);
            });

            for buffer in [&compact, &logs] {
                let text = buffer_text(buffer);
                assert!(text.contains("Stopping..."));
                assert!(text.contains("Changes may already be applied."));
                assert!(text.contains("Ctrl-C cancel"));
                assert!(text.contains("Tab focus"));
            }
        }

        #[test]
        fn initial_execution_position_depends_on_the_completed_result() {
            struct InitialPositionCase {
                name: &'static str,
                status: ApplyStatus,
                expected_marker: &'static str,
                tail_is_visible: bool,
            }

            for case in [
                InitialPositionCase {
                    name: "success_follows_tail",
                    status: ApplyStatus::Succeeded,
                    expected_marker: "tail marker",
                    tail_is_visible: true,
                },
                InitialPositionCase {
                    name: "failure_starts_at_first_error",
                    status: ApplyStatus::Failed,
                    expected_marker: "Error: initial failure",
                    tail_is_visible: false,
                },
                InitialPositionCase {
                    name: "interrupted_follows_tail",
                    status: ApplyStatus::Interrupted,
                    expected_marker: "tail marker",
                    tail_is_visible: true,
                },
            ] {
                let (state, now) = long_apply_state(case.status);
                let buffer = render_to_buffer((80, 24), |frame| {
                    render_execution_with_view(frame, &state, ExecutionViewState::default(), now);
                });
                let text = buffer_text(&buffer);

                assert!(text.contains(case.expected_marker), "case: {}", case.name);
                assert_eq!(
                    text.contains("tail marker"),
                    case.tail_is_visible,
                    "case: {}",
                    case.name
                );
            }
        }

        // Target a fails first, late in all logs; b fails at its own third line; c has no error.
        fn failed_apply_with_target_errors() -> (ExecutionState, Instant) {
            let started_at = Instant::now();
            let mut state = ExecutionState::applying_with_previous(
                started_at,
                ExecutionContext::loading("/repo"),
                ["terraform_data.a", "terraform_data.b", "terraform_data.c"]
                    .into_iter()
                    .map(|address| ExecutionTargetSpec {
                        address: address.to_owned(),
                        actions: vec![PlanAction::Update],
                    })
                    .collect(),
                Vec::new(),
                &[None, None, None],
            );
            let mut record = |severity, summary: String, address: &str| {
                state.record(ExecutionEvent {
                    received_at: started_at,
                    kind: ExecutionEventKind::Diagnostic(Diagnostic {
                        severity,
                        summary,
                        detail: None,
                        address: Some(address.to_owned()),
                        position: None,
                        source: DiagnosticSource::Terraform,
                    }),
                });
            };
            for line in 0..30 {
                record(
                    DiagnosticSeverity::Warning,
                    format!("a line {line}"),
                    "terraform_data.a",
                );
            }
            record(
                DiagnosticSeverity::Error,
                "a failure".to_owned(),
                "terraform_data.a",
            );
            for line in 0..2 {
                record(
                    DiagnosticSeverity::Warning,
                    format!("b line {line}"),
                    "terraform_data.b",
                );
            }
            record(
                DiagnosticSeverity::Error,
                "b failure".to_owned(),
                "terraform_data.b",
            );
            for line in 2..40 {
                record(
                    DiagnosticSeverity::Warning,
                    format!("b line {line}"),
                    "terraform_data.b",
                );
            }
            for line in 0..40 {
                record(
                    DiagnosticSeverity::Warning,
                    format!("c line {line}"),
                    "terraform_data.c",
                );
            }
            let finished_at = started_at + Duration::from_secs(1);
            state.finish_apply(
                ApplyStatus::Failed,
                None,
                Some("apply failed".to_owned()),
                finished_at,
            );
            (state, finished_at)
        }

        #[test]
        fn selected_target_starts_at_its_own_first_error_after_a_failed_apply() {
            struct SelectedTargetCase {
                name: &'static str,
                target: usize,
                expected_offset: usize,
                first_row: &'static str,
            }

            let (state, finished_at) = failed_apply_with_target_errors();
            let area = Rect::new(0, 0, 80, 24);

            for case in [
                SelectedTargetCase {
                    name: "target_with_an_error",
                    target: 1,
                    expected_offset: 2,
                    first_row: "b failure",
                },
                SelectedTargetCase {
                    name: "target_without_an_error",
                    target: 2,
                    expected_offset: 0,
                    first_row: "c line 0",
                },
            ] {
                let mut view = ExecutionViewState::default();
                view.open_logs();
                view.select_target(ExecutionTargetMove::Next, &[case.target]);
                let layout = execution_layout_with_view(area, &state, view);
                let body = layout.body();
                let buffer = render_to_buffer((area.width, area.height), |frame| {
                    render_execution_with_view(frame, &state, view, finished_at);
                });
                let first_row = (body.x..body.x + body.width)
                    .map(|x| buffer[(x, body.y)].symbol())
                    .collect::<String>();

                assert_eq!(
                    execution_scroll_position_with_view(&state, view, &layout).0,
                    case.expected_offset,
                    "case: {}",
                    case.name
                );
                assert_eq!(first_row.trim_end(), case.first_row, "case: {}", case.name);
            }
        }

        #[test]
        fn end_uses_the_log_tail_after_a_failed_apply() {
            let (state, now) = long_apply_state(ApplyStatus::Failed);
            let mut view = ExecutionViewState::default();
            view.end();
            let buffer = render_to_buffer((80, 24), |frame| {
                render_execution_with_view(frame, &state, view, now);
            });

            assert!(buffer_text(&buffer).contains("tail marker"));
        }
    }

    mod result {
        use super::*;

        #[test]
        fn production_execution_failure_render_draws_diagnostic_color() {
            let (state, now) = apply_state(ApplyStatus::Failed);
            let buffer = render_to_buffer((80, 24), |frame| {
                render_execution_with_view(frame, &state, ExecutionViewState::default(), now);
            });

            assert!(
                buffer_text(&buffer)
                    .contains("AccessDenied: synthetic provider rejected the request")
            );
            let diagnostic = "AccessDenied: synthetic provider rejected the request";
            let area = buffer.area();
            for y in area.y..area.bottom() {
                let symbols = (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).expect("diagnostic cell").symbol())
                    .collect::<Vec<_>>();
                let Some(start) = (0..symbols.len()).find(|&start| {
                    symbols[start..]
                        .iter()
                        .copied()
                        .collect::<String>()
                        .starts_with(diagnostic)
                }) else {
                    continue;
                };
                for offset in 0..diagnostic.chars().count() {
                    let cell = buffer
                        .cell((
                            area.x + u16::try_from(start + offset).expect("diagnostic offset"),
                            y,
                        ))
                        .expect("diagnostic cell");
                    assert_eq!(cell.fg, Color::Rgb(0xeb, 0xcb, 0x8b));
                    assert!(cell.modifier.contains(Modifier::BOLD));
                }
                return;
            }
            panic!("diagnostic row should be visible");
        }

        #[test]
        fn completed_apply_statuses_use_their_result_styles() {
            struct StatusCase {
                name: &'static str,
                status: ApplyStatus,
                headline: &'static str,
                headline_color: Color,
                warning: Option<&'static str>,
            }

            for case in [
                StatusCase {
                    name: "success_status",
                    status: ApplyStatus::Succeeded,
                    headline: "Apply complete",
                    headline_color: Color::Rgb(0xa3, 0xbe, 0x8c),
                    warning: None,
                },
                StatusCase {
                    name: "failure_status",
                    status: ApplyStatus::Failed,
                    headline: "Apply failed",
                    headline_color: Color::Rgb(0xbf, 0x61, 0x6a),
                    warning: Some("Changes may already be applied."),
                },
                StatusCase {
                    name: "interrupted_status",
                    status: ApplyStatus::Interrupted,
                    headline: "Apply interrupted",
                    headline_color: Color::Rgb(0xeb, 0xcb, 0x8b),
                    warning: Some("Changes may already be applied."),
                },
            ] {
                let (state, now) = apply_state(case.status);
                let area = Rect::new(0, 0, 80, 24);
                let layout = execution_layout(area, &state);
                let buffer = render_to_buffer((area.width, area.height), |frame| {
                    render_execution_with_view(frame, &state, ExecutionViewState::default(), now);
                });
                let headline = find_text_cell(&buffer, layout.status(), case.headline);

                assert_eq!(headline.fg, case.headline_color, "case: {}", case.name);
                assert!(
                    headline.modifier.contains(Modifier::BOLD),
                    "case: {}",
                    case.name
                );
                if let Some(warning) = case.warning {
                    let warning_cell = find_text_cell(&buffer, layout.status(), warning);
                    assert_eq!(
                        warning_cell.fg,
                        Color::Rgb(0xeb, 0xcb, 0x8b),
                        "case: {}",
                        case.name
                    );
                    assert!(
                        warning_cell.modifier.contains(Modifier::BOLD),
                        "case: {}",
                        case.name
                    );
                }
            }
        }

        #[test]
        fn terraform_summary_stays_once_in_the_log_and_visible_with_elapsed_when_narrow() {
            let now = Instant::now();
            let summary = "Apply complete! Resources: 1 added, 0 changed, 0 destroyed.";
            let mut state = ExecutionState::applying(now, ExecutionContext::loading("/project"));
            state.record(ExecutionEvent {
                received_at: now,
                kind: log_event(EventStream::Stdout, summary.to_owned()),
            });
            state.finish_apply(
                ApplyStatus::Succeeded,
                Some(summary.to_owned()),
                None,
                now + Duration::from_secs(1),
            );

            let buffer = render_to_buffer((80, 24), |frame| {
                render_execution_with_view(
                    frame,
                    &state,
                    ExecutionViewState::default(),
                    now + Duration::from_secs(1),
                );
            });

            assert_eq!(buffer_text(&buffer).matches(summary).count(), 1);

            let narrow = buffer_text(&render_to_buffer((40, 24), |frame| {
                render_execution_with_view(
                    frame,
                    &state,
                    ExecutionViewState::default(),
                    now + Duration::from_secs(1),
                );
            }));

            assert!(narrow.contains("Elapsed: 1.0s"), "{narrow}");
            assert!(narrow.contains("Apply complete! Resources"), "{narrow}");
        }

        #[test]
        fn completed_apply_without_log_shows_a_distinct_empty_output_message() {
            let started_at = Instant::now();
            let mut state = ExecutionState::applying(
                started_at,
                ExecutionContext::loading("/repo/environments/production/main"),
            );
            state.finish_apply(
                ApplyStatus::Succeeded,
                None,
                None,
                started_at + Duration::from_secs(1),
            );

            let buffer = render_to_buffer((80, 24), |frame| {
                render_execution_with_view(
                    frame,
                    &state,
                    ExecutionViewState::default(),
                    started_at + Duration::from_secs(1),
                );
            });
            let text = buffer_text(&buffer);

            assert!(text.contains("Apply result"));
            assert!(text.contains("Apply complete"));
            assert!(text.contains("No execution output."));
            assert!(!text.contains("Waiting for Terraform output..."));
        }

        #[test]
        fn completed_apply_wraps_the_fixed_warning_before_the_log_separator() {
            let (state, now) = apply_state(ApplyStatus::Failed);
            let area = Rect::new(0, 0, 32, 24);
            let layout = execution_layout(area, &state);
            let mut view = ExecutionViewState::default();
            view.apply_scroll(
                ExecutionScroll::Top,
                0,
                layout.max_vertical(),
                layout.body().height,
            );
            let buffer = render_to_buffer((area.width, area.height), |frame| {
                render_execution_with_view(frame, &state, view, now);
            });
            let text = buffer_text(&buffer);

            assert!(layout.body().height > 0);
            assert!(layout.status().height >= 3);
            assert!(text.contains("Apply result"));
            assert!(text.contains("Changes may already be"));
            assert!(layout.log_area().y > layout.status().y);
        }
    }

    mod plan_execution {
        use super::*;

        fn long_plan_lines() -> Vec<String> {
            (0..40)
                .map(|index| match index {
                    3 => "Error: initial failure".to_owned(),
                    39 => "tail marker".to_owned(),
                    _ => format!("log line {index}"),
                })
                .collect()
        }

        fn render_text(
            (width, height): (u16, u16),
            state: &ExecutionState,
            view: ExecutionViewState,
            now: Instant,
            quit_confirmation: bool,
        ) -> String {
            buffer_text(&render_to_buffer((width, height), |frame| {
                render_execution_with_quit_confirmation(frame, state, view, now, quit_confirmation);
            }))
        }

        #[test]
        fn failed_plan_starts_at_the_first_error_until_end_is_pressed() {
            let lines = long_plan_lines();
            let lines = lines.iter().map(String::as_str).collect::<Vec<_>>();
            let (mut state, now) = plan_state(&lines);
            state.fail("synthetic plan failure".to_owned(), now);

            let initial = render_text((80, 24), &state, ExecutionViewState::default(), now, false);
            let mut view = ExecutionViewState::default();
            view.end();
            let end = render_text((80, 24), &state, view, now, false);

            assert!(initial.contains("Error: initial failure"));
            assert!(!initial.contains("tail marker"));
            assert!(end.contains("tail marker"));
        }

        #[test]
        fn narrow_terminal_notice_depends_on_the_stage_and_quit_confirmation() {
            struct NoticeCase {
                name: &'static str,
                failed: bool,
                quit_confirmation: bool,
                expected: &'static str,
            }

            for case in [
                NoticeCase {
                    name: "running",
                    failed: false,
                    quit_confirmation: false,
                    expected: "Terminal too small. Resize or press Ctrl-C to cancel.",
                },
                NoticeCase {
                    name: "failed",
                    failed: true,
                    quit_confirmation: false,
                    expected: "Terminal too small. Resize or press q to quit.",
                },
                NoticeCase {
                    name: "quit_confirmation",
                    failed: false,
                    quit_confirmation: true,
                    expected: "Quit? Enter exit / Esc cancel",
                },
            ] {
                let (mut state, now) = plan_state(&["output"]);
                if case.failed {
                    state.fail("synthetic plan failure".to_owned(), now);
                }

                let text = render_text(
                    (MIN_WIDTH - 1, MIN_HEIGHT),
                    &state,
                    ExecutionViewState::default(),
                    now,
                    case.quit_confirmation,
                );

                assert_eq!(
                    text.split_whitespace().collect::<Vec<_>>().join(" "),
                    case.expected,
                    "case: {}",
                    case.name
                );
            }
        }

        #[test]
        fn quit_confirmation_and_copy_notice_replace_the_failed_footer_in_place() {
            let (mut state, now) = plan_state(&["output"]);
            state.fail("synthetic plan failure".to_owned(), now);
            let area = Rect::new(0, 0, 80, 24);
            let normal = execution_layout(area, &state);
            let waiting = execution_layout_with_quit_confirmation_and_view(
                area,
                &state,
                ExecutionViewState::default(),
                true,
            );

            let confirmation =
                render_text((80, 24), &state, ExecutionViewState::default(), now, true);
            let mut session = SessionState::new(state);
            session::update(
                &mut session,
                Action::CopyCompleted {
                    target: CopyTarget::Diagnostic,
                    result: CopyResult::Written,
                },
                now,
            );
            let copied = render_text(
                (80, 24),
                session.execution().expect("execution should be visible"),
                ExecutionViewState::default(),
                now,
                false,
            );

            assert_eq!(waiting.body(), normal.body());
            assert_eq!(waiting.max_vertical(), normal.max_vertical());
            assert!(confirmation.contains("Quit Terraleph?   [Enter] Quit   [Esc] Cancel"));
            assert!(!confirmation.contains("q/Ctrl-C quit"));
            assert!(copied.contains("Copied."));
            assert!(copied.contains("q/Ctrl-C quit"));
        }

        #[test]
        fn narrow_quit_confirmation_keeps_its_prompt_while_a_copy_notice_is_active() {
            let (mut state, now) = plan_state(&["output"]);
            state.fail("synthetic plan failure".to_owned(), now);
            state
                .copy_feedback_mut()
                .record(CopyResult::SentToTerminal, now, false);

            let copied = render_text((40, 24), &state, ExecutionViewState::default(), now, false);
            let confirmation =
                render_text((40, 24), &state, ExecutionViewState::default(), now, true);

            assert!(copied.contains("Sent to terminal clipboard."), "{copied}");
            assert!(
                confirmation.contains("Quit? [Enter] quit [Esc] cancel"),
                "{confirmation}"
            );
            assert!(
                !confirmation.contains("Sent to terminal clipboard."),
                "{confirmation}"
            );
        }
    }

    mod progress {
        use super::*;

        #[test]
        fn running_apply_keeps_the_compact_frame_fixed_as_logs_and_state_change() {
            for &(width, height) in &SIZES {
                let (empty_state, _) = applying_state_with_content(0, 0);
                let (short_state, _) = applying_state_with_content(1, 1);
                let (long_state, _) = applying_state_with_content(40, 1);
                let mut stopping_state = short_state.clone();
                stopping_state.apply(ExecutionAction::RequestCancellation);
                let area = Rect::new(0, 0, width, height);
                let empty_layout = execution_layout(area, &empty_state);
                let short_layout = execution_layout(area, &short_state);
                let long_layout = execution_layout(area, &long_state);
                let stopping_layout = execution_layout(area, &stopping_state);

                assert_eq!(empty_layout.shell.content(), short_layout.shell.content());
                assert_eq!(short_layout.shell.content(), long_layout.shell.content());
                assert_eq!(
                    short_layout.shell.content(),
                    stopping_layout.shell.content()
                );
                assert_eq!(empty_layout.shell.footer(), short_layout.shell.footer());
                assert_eq!(short_layout.shell.footer(), long_layout.shell.footer());
                assert_eq!(short_layout.shell.footer(), stopping_layout.shell.footer());
                assert_eq!(short_layout.status(), long_layout.status());
                assert!(stopping_layout.status().height >= short_layout.status().height);
                assert!(short_layout.shell.footer().bottom() > short_layout.shell.header().y);

                let long_buffer = render_to_buffer((width, height), |frame| {
                    render_execution_with_view(
                        frame,
                        &long_state,
                        ExecutionViewState::default(),
                        Instant::now(),
                    );
                });
                let long_text = buffer_text(&long_buffer);
                assert!(long_text.contains("Applying"));
                assert!(long_text.contains('x'));
                assert!(!long_text.contains("Waiting for Terraform output..."));

                let stopping_buffer = render_to_buffer((width, height), |frame| {
                    render_execution_with_view(
                        frame,
                        &stopping_state,
                        ExecutionViewState::default(),
                        Instant::now(),
                    );
                });
                let stopping_text = buffer_text(&stopping_buffer);
                assert!(stopping_text.contains("Stopping..."));
                assert!(stopping_text.contains("Changes may already be applied."));
            }
        }

        #[test]
        fn running_status_keeps_fixed_height_at_the_narrowest_width() {
            let started_at = Instant::now();
            let now = started_at + Duration::from_secs(10_000);
            let state =
                ExecutionState::with_context(started_at, ExecutionContext::loading("/project"));
            let area = Rect::new(0, 0, 32, 24);
            let layout = execution_layout(area, &state);
            let buffer = render_to_buffer((area.width, area.height), |frame| {
                render_execution_with_view(frame, &state, ExecutionViewState::default(), now);
            });

            assert_eq!(layout.status().height, STATUS_HEIGHT);
            assert_eq!(layout.log_area().y, layout.status().bottom());
            assert_eq!(layout.separator().y, layout.log_area().bottom());
            let text = buffer_text(&buffer);
            assert!(text.contains("Elapsed 10000.0s"), "{text}");
        }

        #[test]
        fn running_status_cycles_the_ascii_spinner() {
            let started_at = Instant::now();
            let state =
                ExecutionState::with_context(started_at, ExecutionContext::loading("/project"));
            let frames = ["|", "/", "-", "\\"];

            for (index, frame) in frames.into_iter().enumerate() {
                let status = status_lines(
                    &state,
                    started_at + Duration::from_millis(u64::try_from(index).unwrap() * 100),
                );
                assert_eq!(status[0].to_string(), format!("{frame} Reading plan..."));
            }
        }

        #[test]
        fn append_only_log_is_rendered_in_receive_order() {
            let now = Instant::now();
            let mut state =
                ExecutionState::with_context(now, ExecutionContext::loading("/project"));
            for (stream, text) in [
                (EventStream::Stdout, "first"),
                (EventStream::Stderr, "second"),
                (EventStream::Stdout, "third"),
            ] {
                state.record(ExecutionEvent {
                    received_at: now,
                    kind: log_event(stream, text.to_owned()),
                });
            }

            assert_eq!(
                prepare_content(&state, ExecutionViewState::default())
                    .visible_lines(0, 0, Rect::new(0, 0, 80, 24))
                    .iter()
                    .map(Line::to_string)
                    .collect::<Vec<_>>(),
                vec!["first", "second", "third"]
            );
        }
    }

    mod copy {
        use super::*;

        #[test]
        fn production_execution_copy_flash_uses_accent_background_then_restores_log_style() {
            let started_at = Instant::now();
            let mut state =
                ExecutionState::applying(started_at, ExecutionContext::loading("/repo"));
            state.record(ExecutionEvent {
                received_at: started_at,
                kind: log_event(
                    EventStream::Stdout,
                    "terraform apply review.tfplan".to_owned(),
                ),
            });
            state.record(ExecutionEvent {
                received_at: started_at,
                kind: log_event(EventStream::Stdout, "apply output".to_owned()),
            });
            let mut session = SessionState::new(state);
            let mut view = ExecutionViewState::default();
            view.open_logs();
            let before = render_to_buffer((80, 24), |frame| {
                render_execution_with_view(
                    frame,
                    session.execution().expect("execution should be visible"),
                    view,
                    started_at,
                );
            });
            session::update(
                &mut session,
                Action::CopyCompleted {
                    target: CopyTarget::Execution,
                    result: CopyResult::Written,
                },
                started_at,
            );
            let state = session.execution().expect("execution should be visible");
            let flash = render_to_buffer((80, 24), |frame| {
                render_execution_with_view(frame, state, view, started_at);
            });
            let after = render_to_buffer((80, 24), |frame| {
                render_execution_with_view(
                    frame,
                    state,
                    view,
                    started_at + Duration::from_millis(201),
                );
            });

            let body = execution_layout_with_view(Rect::new(0, 0, 80, 24), state, view).body();
            let flash_cell = find_text_cell(&flash, body, "terraform apply review.tfplan");
            assert_eq!(flash_cell.fg, Color::Rgb(0x11, 0x14, 0x19));
            assert_eq!(flash_cell.bg, Color::Rgb(0xf4, 0x9e, 0x4c));
            let before_cell = find_text_cell(&before, body, "terraform apply review.tfplan");
            let after_cell = find_text_cell(&after, body, "terraform apply review.tfplan");
            assert_eq!(after_cell, before_cell);
        }

        #[test]
        fn completed_apply_statuses_keep_full_log_order_for_render_and_copy() {
            struct ApplyCase {
                name: &'static str,
                status: ApplyStatus,
                expected_headline: &'static str,
                expects_warning: bool,
            }

            for case in [
                ApplyCase {
                    name: "succeeded",
                    status: ApplyStatus::Succeeded,
                    expected_headline: "Apply complete.",
                    expects_warning: false,
                },
                ApplyCase {
                    name: "failed",
                    status: ApplyStatus::Failed,
                    expected_headline: "Apply failed.",
                    expects_warning: true,
                },
                ApplyCase {
                    name: "interrupted",
                    status: ApplyStatus::Interrupted,
                    expected_headline: "Apply interrupted.",
                    expects_warning: true,
                },
            ] {
                let now = Instant::now();
                let mut state =
                    ExecutionState::applying(now, ExecutionContext::loading("/project"));
                for (stream, text) in [
                    (EventStream::Stdout, "first"),
                    (EventStream::Stderr, "second"),
                    (EventStream::Stdout, "third"),
                ] {
                    state.record(ExecutionEvent {
                        received_at: now,
                        kind: log_event(stream, text.to_owned()),
                    });
                }
                state.finish_apply(case.status, None, None, now + Duration::from_secs(1));
                let area = Rect::new(0, 0, 80, 24);
                let body = execution_layout(area, &state).body();
                let buffer = render_to_buffer((area.width, area.height), |frame| {
                    render_execution_with_view(
                        frame,
                        &state,
                        ExecutionViewState::default(),
                        now + Duration::from_secs(1),
                    );
                });

                let rendered_log = (body.y..body.bottom())
                    .map(|y| {
                        (body.x..body.right())
                            .map(|x| buffer[(x, y)].symbol())
                            .collect::<String>()
                            .trim_end()
                            .to_owned()
                    })
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>();
                assert_eq!(
                    rendered_log,
                    ["first", "second", "third"],
                    "case: {}",
                    case.name
                );
                let copied = state
                    .copy_effect(CopyTarget::Execution)
                    .expect("completed apply should be copyable")
                    .text()
                    .to_owned();
                assert!(
                    copied.starts_with(case.expected_headline),
                    "case: {}",
                    case.name
                );
                assert!(copied.contains("Completed: 0/0"), "case: {}", case.name);
                assert!(copied.contains("Elapsed: 1.0s"), "case: {}", case.name);
                assert_eq!(
                    case.expects_warning,
                    copied.contains("Changes may already be applied."),
                    "case: {}",
                    case.name
                );
                assert!(
                    copied.ends_with("first\nsecond\nthird"),
                    "case: {}",
                    case.name
                );
            }
        }
    }
}
