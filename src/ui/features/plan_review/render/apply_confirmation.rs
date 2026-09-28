use std::time::Instant;

use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use crate::app::{
    execution::{ExecutionContext, ExecutionContextValue},
    review::PlanReview,
    session::ReviewSessionState,
};
use crate::ui::features::plan_review::{
    ApplyConfirmationViewState, ConfirmationOverlay, PlanReviewViewState,
};
use crate::ui::primitives::molecules::{
    context_dialog, dialog_scroll::DialogScroll, help_dialog, terminal_notice,
};
use crate::ui::shell::{context, footer, header, layout as shell_layout};
use crate::ui::theme;

use super::render_with_quit_confirmation;

pub(super) const CONFIRMATION_MAX_WIDTH: u16 = 80;
const CONFIRMATION_HEADER_HEIGHT: u16 = 2;
const CONFIRMATION_NOTICE: &str = "Terminal too small. Resize or press Esc to go back.";

pub(crate) struct ApplyConfirmationLayout {
    header: Rect,
    notice: Rect,
    frame: Rect,
    footer: Rect,
    inner: Rect,
    input: Rect,
    status: Rect,
    prefix: Rect,
    scroll: Rect,
    suffix: Rect,
    prefix_lines: Vec<Line<'static>>,
    scroll_lines: Vec<Line<'static>>,
    suffix_lines: Vec<Line<'static>>,
    max_vertical: u16,
    renderable: bool,
}

impl ApplyConfirmationLayout {
    pub(crate) const fn header(&self) -> Rect {
        self.header
    }

    pub(crate) const fn notice(&self) -> Rect {
        self.notice
    }

    pub(crate) const fn frame(&self) -> Rect {
        self.frame
    }

    pub(crate) const fn footer(&self) -> Rect {
        self.footer
    }

    pub(crate) const fn inner(&self) -> Rect {
        self.inner
    }

    pub(crate) const fn input(&self) -> Rect {
        self.input
    }

    pub(crate) const fn status(&self) -> Rect {
        self.status
    }

    pub(crate) const fn prefix(&self) -> Rect {
        self.prefix
    }

    pub(crate) const fn scroll(&self) -> Rect {
        self.scroll
    }

    pub(crate) const fn suffix(&self) -> Rect {
        self.suffix
    }

    pub(crate) fn prefix_lines(&self) -> &[Line<'static>] {
        &self.prefix_lines
    }

    pub(crate) fn scroll_lines(&self) -> &[Line<'static>] {
        &self.scroll_lines
    }

    pub(crate) fn suffix_lines(&self) -> &[Line<'static>] {
        &self.suffix_lines
    }

    pub(crate) const fn max_vertical(&self) -> u16 {
        self.max_vertical
    }

    pub(crate) const fn renderable(&self) -> bool {
        self.renderable
    }
}

pub(crate) fn render_apply_confirmation(
    frame: &mut Frame<'_>,
    state: &ReviewSessionState,
    view: &ApplyConfirmationViewState,
) {
    render_with_quit_confirmation(
        frame,
        state,
        &PlanReviewViewState::default(),
        Instant::now(),
        false,
    );
    render_apply_confirmation_dialog(frame, state, view, None);
}

/// Dims whatever the caller already drew and places the dialog over it, so each
/// navigation mode keeps its own screen behind the confirmation. A caller that
/// already drew its own header passes that area; the compact review header would
/// otherwise overwrite the content below it.
pub(crate) fn render_apply_confirmation_dialog(
    frame: &mut Frame<'_>,
    state: &ReviewSessionState,
    view: &ApplyConfirmationViewState,
    drawn_header: Option<Rect>,
) {
    let area = frame.area();
    dim_background(frame);
    let layout = apply_confirmation_layout(area, state);
    let header_area = drawn_header.unwrap_or_else(|| layout.header());
    if drawn_header.is_none() && header_area.height > 0 {
        header::render_review(frame, header_area, state.review());
    }
    if !layout.renderable() {
        terminal_notice::render_wrapped(frame, layout.notice(), CONFIRMATION_NOTICE);
        if let Some(overlay) = view.overlay() {
            render_confirmation_overlay(
                frame,
                area,
                state.review(),
                overlay,
                view.overlay_scroll(),
            );
        }
        return;
    }

    frame.render_widget(Clear, layout.frame());
    let block_inner =
        shell_layout::render_content_block_line(frame, layout.frame(), Line::default());
    let inner = padded_confirmation_inner(block_inner);
    debug_assert_eq!(inner, layout.inner());
    let info_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(1),
    );
    debug_assert_eq!(
        info_area,
        Rect::new(
            layout.prefix().x,
            layout.prefix().y,
            layout.prefix().width,
            layout.prefix().height + layout.scroll().height + layout.suffix().height
        )
    );
    frame.render_widget(
        Paragraph::new(layout.prefix_lines().to_owned())
            .style(theme::body_style())
            .wrap(Wrap { trim: false }),
        layout.prefix(),
    );
    frame.render_widget(
        Paragraph::new(layout.scroll_lines().to_owned())
            .style(theme::body_style())
            .wrap(Wrap { trim: false })
            .scroll((view.scroll().min(layout.max_vertical()), 0)),
        layout.scroll(),
    );
    frame.render_widget(
        Paragraph::new(layout.suffix_lines().to_owned())
            .style(theme::body_style())
            .wrap(Wrap { trim: false }),
        layout.suffix(),
    );
    frame.render_widget(
        Paragraph::new(confirmation_input_line(view))
            .style(theme::body_style())
            .scroll((0, confirmation_input_scroll(view, layout.input().width))),
        layout.input(),
    );
    let expected = state.review().confirmation_input();
    frame.render_widget(
        Paragraph::new(confirmation_status_line(view, &expected)),
        layout.status(),
    );
    let footer_lines = footer::layout(
        confirmation_footer_items(view.input() == expected),
        layout.frame().width,
    );
    footer::render(frame, layout.footer(), &footer_lines, None);
    clear_dim(frame, header_area);
    clear_dim(frame, layout.frame());
    clear_dim(frame, layout.footer());
    if let Some(overlay) = view.overlay() {
        render_confirmation_overlay(frame, area, state.review(), overlay, view.overlay_scroll());
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the confirmation layout keeps content and safety constraints together"
)]
pub(crate) fn apply_confirmation_layout(
    area: Rect,
    state: &ReviewSessionState,
) -> ApplyConfirmationLayout {
    let panel = shell_layout::max_centered_area(area);
    let header_height = panel.height.min(CONFIRMATION_HEADER_HEIGHT);
    let header = Rect::new(panel.x, panel.y, panel.width, header_height);
    let available = Rect::new(
        panel.x,
        panel.y.saturating_add(header_height),
        panel.width,
        panel.height.saturating_sub(header_height),
    );
    let frame_width = panel.width.min(CONFIRMATION_MAX_WIDTH);
    let footer_items = confirmation_footer_items(true);
    let footer_lines = footer::layout(footer_items.clone(), frame_width);
    let footer_required_width = footer_items.iter().map(Line::width).sum::<usize>()
        + footer_items.len().saturating_sub(1) * 3;
    let footer_fits = footer_lines.len() == 1
        && footer_lines
            .first()
            .is_some_and(|line| line.width() == footer_required_width);
    let inner_width = frame_width.saturating_sub(4);
    let ConfirmationSections {
        prefix: prefix_lines,
        scroll: scroll_lines,
        suffix: suffix_lines,
    } = confirmation_sections(state);
    let body = Paragraph::new(
        [
            prefix_lines.as_slice(),
            scroll_lines.as_slice(),
            suffix_lines.as_slice(),
        ]
        .concat(),
    )
    .wrap(Wrap { trim: false });
    let body_height = body.line_count(inner_width).saturating_add(1);
    let natural_frame_height = u16::try_from(body_height)
        .unwrap_or(u16::MAX)
        .saturating_add(4);
    let max_frame_height = available.height.saturating_sub(1);
    let frame_height = natural_frame_height.min(max_frame_height);
    let group_height = frame_height.saturating_add(1);
    let renderable =
        inner_width > 0 && footer_fits && group_height <= available.height && frame_width >= 5;
    let frame_x = panel.x + panel.width.saturating_sub(frame_width) / 2;
    let group_y = available.y + available.height.saturating_sub(group_height) / 2;
    let frame = Rect::new(frame_x, group_y, frame_width, frame_height);
    let footer = Rect::new(frame.x, frame.bottom(), frame.width, 1);
    let inner = padded_confirmation_inner(Block::new().borders(Borders::ALL).inner(frame));
    let info_height = inner.height.saturating_sub(1);
    let input = Rect::new(
        inner.x,
        inner.y.saturating_add(info_height),
        inner.width,
        u16::from(inner.height > 0),
    );
    // The mismatch message takes the bottom padding row, so a rejected Enter
    // never shifts the dialog layout or its scroll limit.
    let status = Rect::new(inner.x, input.bottom(), inner.width, input.height);
    let prefix_height = u16::try_from(
        Paragraph::new(prefix_lines.clone())
            .wrap(Wrap { trim: false })
            .line_count(inner_width),
    )
    .unwrap_or(u16::MAX);
    let suffix_height = u16::try_from(
        Paragraph::new(suffix_lines.clone())
            .wrap(Wrap { trim: false })
            .line_count(inner_width),
    )
    .unwrap_or(u16::MAX);
    let fixed_height = prefix_height.saturating_add(suffix_height);
    let scroll_height = info_height.saturating_sub(fixed_height);
    let prefix = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        prefix_height.min(info_height),
    );
    let suffix_y = inner.y + info_height.saturating_sub(suffix_height);
    let suffix = Rect::new(
        inner.x,
        suffix_y,
        inner.width,
        suffix_height.min(info_height),
    );
    let scroll = Rect::new(
        inner.x,
        inner.y.saturating_add(prefix.height),
        inner.width,
        scroll_height,
    );
    let renderable = renderable
        && usize::from(info_height) >= usize::from(fixed_height)
        && (scroll_lines.is_empty() || scroll_height > 0);
    let max_vertical = u16::try_from(
        Paragraph::new(scroll_lines.clone())
            .wrap(Wrap { trim: false })
            .line_count(inner_width)
            .saturating_sub(usize::from(scroll_height)),
    )
    .unwrap_or(u16::MAX);
    ApplyConfirmationLayout {
        header,
        notice: available,
        frame,
        footer,
        inner,
        input,
        status,
        prefix,
        scroll,
        suffix,
        prefix_lines,
        scroll_lines,
        suffix_lines,
        max_vertical,
        renderable,
    }
}

const fn padded_confirmation_inner(inner: Rect) -> Rect {
    Rect::new(
        inner.x.saturating_add(1),
        inner.y.saturating_add(1),
        inner.width.saturating_sub(2),
        inner.height.saturating_sub(2),
    )
}

struct ConfirmationSections {
    prefix: Vec<Line<'static>>,
    scroll: Vec<Line<'static>>,
    suffix: Vec<Line<'static>>,
}

fn confirmation_footer_items(input_matches: bool) -> Vec<Line<'static>> {
    let apply = if input_matches {
        footer::hint(&["Enter"], "apply")
    } else {
        footer::disabled_hint(&["Enter"], "apply")
    };
    vec![
        apply,
        footer::hint(&["Esc"], "back"),
        footer::hint(&["?"], "help"),
    ]
}

fn confirmation_sections(state: &ReviewSessionState) -> ConfirmationSections {
    let review = state.review();
    let counts = review.summary();
    let context = review.context();
    let target = match context.display_name() {
        ExecutionContextValue::Known(name) => {
            let suffix = context.is_production().is_some_and(|production| production);
            if suffix {
                format!("{name} [PROD]")
            } else {
                name.clone()
            }
        }
        ExecutionContextValue::Loading => "loading...".to_owned(),
    };
    let mut prefix = vec![Line::from("Apply this reviewed plan?")];
    prefix.extend(confirmation_warning(review));
    prefix.push(Line::default());
    prefix.extend([
        Line::from(vec![
            Span::styled("Target: ", theme::secondary_style()),
            Span::styled(target, theme::body_style()),
        ]),
        Line::from(vec![
            Span::styled("Workspace: ", theme::secondary_style()),
            Span::styled(state.review().workspace().to_owned(), theme::body_style()),
        ]),
        Line::from(vec![
            Span::styled("Directory: ", theme::secondary_style()),
            Span::styled(
                context::relative_directory(context.cwd_path(), context.launch_root_path()),
                theme::body_style(),
            ),
        ]),
        Line::from(vec![
            Span::styled("Tool: ", theme::secondary_style()),
            Span::styled(tool_version(context), theme::body_style()),
        ]),
        Line::from(format!(
            "Plan: +{} add  ~{} update  {} replace  -{} destroy",
            counts.creates, counts.updates, counts.replaces, counts.deletes,
        )),
    ]);
    let mut scroll = Vec::new();
    append_variable_sources(&mut scroll, context);
    append_destructive_resources(&mut scroll, review);
    if !state.review().search_query().is_empty() {
        scroll.push(Line::from(Span::styled(
            "Filter changes display only. Apply uses all changes.",
            theme::secondary_style(),
        )));
    }
    let suffix = vec![
        Line::default(),
        confirmation_instruction(&review.confirmation_input()),
    ];
    ConfirmationSections {
        prefix,
        scroll,
        suffix,
    }
}

// States why the confirmation text is the target name instead of "yes", so
// the stricter confirmation is never unexplained.
fn confirmation_warning(review: &PlanReview) -> Option<Line<'static>> {
    let destructive = review.has_destructive_changes();
    let production = review.context().is_production() == Some(true);
    let warning = match (destructive, production) {
        (true, true) => "! Warning: this plan destroys or replaces resources in production.",
        (true, false) => "! Warning: this plan destroys or replaces resources.",
        (false, true) => "! Warning: this target looks like production.",
        (false, false) => return None,
    };
    Some(Line::from(Span::styled(warning, theme::warning_style())))
}

fn confirmation_instruction(expected: &str) -> Line<'static> {
    let text = theme::body_style().add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled("To confirm, type ", text),
        Span::styled(
            format!("\"{expected}\""),
            theme::accent_style().add_modifier(Modifier::BOLD),
        ),
        Span::styled(" below.", text),
    ])
}

fn confirmation_status_line(view: &ApplyConfirmationViewState, expected: &str) -> Line<'static> {
    if !view.rejected() {
        return Line::default();
    }
    let message = if view.input() == "yes" && expected != "yes" {
        format!("Type \"{expected}\", not \"yes\".")
    } else {
        format!("Does not match \"{expected}\".")
    };
    Line::from(Span::styled(message, theme::error_style()))
}

fn append_variable_sources(lines: &mut Vec<Line<'static>>, context: &ExecutionContext) {
    let sources = context.variable_sources();
    if sources.automatic_files().is_empty()
        && sources.explicit_files().is_empty()
        && !sources.has_var_argument()
        && sources.environment_variables().is_empty()
    {
        return;
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        "Variable sources:",
        theme::secondary_style(),
    )));
    for path in sources.automatic_files() {
        lines.push(Line::from(format!("  auto: {}", source_name(path))));
    }
    for path in sources.explicit_files() {
        lines.push(Line::from(format!("  -var-file: {}", source_name(path))));
    }
    if sources.has_var_argument() {
        lines.push(Line::from("  -var: provided"));
    }
    if !sources.environment_variables().is_empty() {
        lines.push(Line::from(format!(
            "  TF_VAR_*: {} provided",
            sources.environment_variables().len()
        )));
    }
}

fn append_destructive_resources(lines: &mut Vec<Line<'static>>, review: &PlanReview) {
    let destroy = review.destructive_addresses().collect::<Vec<_>>();
    let replace = review.replacement_addresses().collect::<Vec<_>>();
    if destroy.is_empty() && replace.is_empty() {
        return;
    }
    lines.push(Line::default());
    for (label, addresses, style) in [
        ("Destroy", destroy, theme::error_style()),
        ("Replace", replace, theme::warning_style()),
    ] {
        if addresses.is_empty() {
            continue;
        }
        lines.push(Line::from(Span::styled(format!("{label}:"), style)));
        lines.extend(
            addresses
                .into_iter()
                .map(|address| Line::from(Span::styled(format!("  {address}"), style))),
        );
    }
}

fn source_name(path: &std::path::Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn tool_version(context: &ExecutionContext) -> String {
    let version = match context.tool_version() {
        ExecutionContextValue::Known(version) => version.as_str(),
        ExecutionContextValue::Loading => "loading...",
    };
    format!("{} {version}", context.tool_name())
}

fn dim_background(frame: &mut Frame<'_>) {
    for cell in &mut frame.buffer_mut().content {
        cell.set_style(cell.style().add_modifier(Modifier::DIM));
    }
}

fn clear_dim(frame: &mut Frame<'_>, area: Rect) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.modifier.remove(Modifier::DIM);
            }
        }
    }
}

fn confirmation_input_line(view: &ApplyConfirmationViewState) -> Line<'static> {
    let cursor = view.cursor().min(view.input().len());
    Line::from(vec![
        Span::styled("> ", theme::body_style()),
        Span::styled(view.input()[..cursor].to_owned(), theme::body_style()),
        Span::styled("|", theme::accent_style()),
        Span::styled(view.input()[cursor..].to_owned(), theme::body_style()),
    ])
}

pub(super) fn confirmation_input_scroll(view: &ApplyConfirmationViewState, width: u16) -> u16 {
    let width = usize::from(width);
    let cursor = view.cursor().min(view.input().len());
    let cursor_width = 2 + Line::from(view.input()[..cursor].to_owned()).width();
    u16::try_from(cursor_width.saturating_sub(width.saturating_sub(1))).unwrap_or(u16::MAX)
}

fn render_confirmation_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    review: &PlanReview,
    overlay: ConfirmationOverlay,
    scroll: &DialogScroll,
) {
    match overlay {
        ConfirmationOverlay::Help => help_dialog::render(
            frame,
            area,
            "Apply help",
            &[
                help_dialog::HelpSection::new(
                    "Navigation",
                    vec![
                        help_dialog::HelpAction::new("↑ / ↓ / PgUp / PgDn", "scroll confirmation"),
                        help_dialog::HelpAction::new("← / →", "move in confirmation input"),
                        help_dialog::HelpAction::new("Home / End", "move to input start or end"),
                    ],
                ),
                help_dialog::HelpSection::new(
                    "Input",
                    vec![
                        help_dialog::HelpAction::new("Type", "enter the confirmation text"),
                        help_dialog::HelpAction::new("Backspace", "delete before the cursor"),
                    ],
                ),
                help_dialog::HelpSection::new(
                    "Context",
                    vec![help_dialog::HelpAction::new(
                        "Tab",
                        "show execution context",
                    )],
                ),
                help_dialog::HelpSection::new(
                    "Apply",
                    vec![
                        help_dialog::HelpAction::new("Enter", "confirm apply"),
                        help_dialog::HelpAction::new("Esc", "return to plan review"),
                    ],
                ),
            ],
            scroll,
        ),
        ConfirmationOverlay::Context => {
            context_dialog::render(frame, area, review.context(), scroll);
        }
    }
}
