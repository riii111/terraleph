use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};

use crate::{
    app::{
        environments::{EnvironmentPlan, EnvironmentSession, EnvironmentState},
        execution::{PreparationStage, ToolVersion, directory_display_name},
    },
    ui::{primitives::atoms::ready_mark, shell::context::truncate_middle, theme},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EnvironmentPane {
    Environments,
    Matrix,
    Relations,
}

pub(super) fn status_marker(state: &EnvironmentState) -> Span<'static> {
    match state {
        EnvironmentState::Ready { .. } => ready_mark::render(),
        EnvironmentState::Error => Span::styled("✗ ", theme::overview_total_destroy_style()),
        EnvironmentState::Unselected
        | EnvironmentState::Pending
        | EnvironmentState::Running
        | EnvironmentState::ExcludedHcp => Span::styled("  ", theme::overview_muted_style()),
    }
}

pub(super) fn status_style(state: &EnvironmentState) -> ratatui::style::Style {
    match state {
        EnvironmentState::Unselected | EnvironmentState::Pending | EnvironmentState::Running => {
            theme::overview_muted_style()
        }
        EnvironmentState::Ready { .. } => theme::overview_text_style(),
        EnvironmentState::Error => theme::overview_total_destroy_style(),
        EnvironmentState::ExcludedHcp => theme::overview_warning_style(),
    }
}

#[derive(Default)]
pub(super) struct EnvironmentSelection {
    pub(super) column: usize,
    pub(super) raw: Option<usize>,
}

pub(super) struct EnvironmentLayout {
    pub(super) header: Rect,
    pub(super) summary: Rect,
    pub(super) body: Rect,
    pub(super) footer: Rect,
    pub(super) environments: Rect,
    pub(super) matrix: Rect,
    pub(super) relations: Rect,
}

impl EnvironmentSelection {
    pub(super) fn active(&self) -> usize {
        self.raw.unwrap_or(self.column)
    }
}

/// The matrix takes at most four tenths of the right column and only the rows it needs, so the
/// rows a short matrix does not use go to the relations.
pub(super) fn overview_layout(
    area: Rect,
    sidebar_width: u16,
    sidebar_visible: bool,
    maximized: Option<EnvironmentPane>,
    matrix_rows: impl FnOnce(u16) -> usize,
) -> EnvironmentLayout {
    let show_summary = !sidebar_visible && maximized.is_none();
    let header = Rect::new(area.x, area.y, area.width, area.height.min(1));
    let footer_height = if area.height < 6 {
        1
    } else {
        area.height.saturating_sub(header.height).min(2)
    };
    let footer = Rect::new(
        area.x,
        area.bottom().saturating_sub(footer_height),
        area.width,
        footer_height,
    );
    let summary_height =
        u16::from(show_summary && area.height >= 6).min(footer.y.saturating_sub(header.bottom()));
    let summary = Rect::new(area.x, header.bottom(), area.width, summary_height);
    let body = Rect::new(
        area.x,
        summary.bottom(),
        area.width,
        footer.y.saturating_sub(summary.bottom()),
    );
    let mut environments = Rect::default();
    let mut matrix = Rect::default();
    let mut relations = Rect::default();

    match maximized {
        Some(EnvironmentPane::Environments) => environments = body,
        Some(EnvironmentPane::Matrix) => matrix = body,
        Some(EnvironmentPane::Relations) => relations = body,
        None => {
            let right = if sidebar_visible {
                let width = sidebar_width.min(body.width);
                environments = Rect::new(body.x, body.y, width, body.height);
                Rect::new(
                    body.x.saturating_add(width),
                    body.y,
                    body.width.saturating_sub(width),
                    body.height,
                )
            } else {
                body
            };
            let needed = u16::try_from(matrix_rows(right.width)).unwrap_or(u16::MAX);
            let matrix_height = (right.height.saturating_mul(4) / 10).min(needed);
            matrix = Rect::new(right.x, right.y, right.width, matrix_height);
            relations = Rect::new(
                right.x,
                right.y.saturating_add(matrix_height),
                right.width,
                right.height.saturating_sub(matrix_height),
            );
        }
    }

    EnvironmentLayout {
        header,
        summary,
        body,
        footer,
        environments,
        matrix,
        relations,
    }
}

const REVIEW_GAP: u16 = 1;

/// The full plan keeps the environment list where the overview draws it, so the plan starts near
/// where the overview panes start.
pub(super) struct ReviewLayout {
    pub(super) header: Rect,
    pub(super) environments: Rect,
    pub(super) plan: Rect,
}

pub(super) fn review_layout(area: Rect, sidebar_width: u16, sidebar_visible: bool) -> ReviewLayout {
    let header = Rect::new(area.x, area.y, area.width, area.height.min(1));
    let body = Rect::new(
        area.x,
        header.bottom(),
        area.width,
        area.bottom().saturating_sub(header.bottom()),
    );
    if !sidebar_visible {
        return ReviewLayout {
            header,
            environments: Rect::default(),
            plan: body,
        };
    }
    let width = sidebar_width.min(body.width);
    let plan_x = width.saturating_add(REVIEW_GAP).min(body.width);
    ReviewLayout {
        header,
        environments: Rect::new(body.x, body.y, width, body.height),
        plan: Rect::new(
            body.x.saturating_add(plan_x),
            body.y,
            body.width.saturating_sub(plan_x),
            body.height,
        ),
    }
}

pub(super) fn sidebar_width(plans: &[EnvironmentPlan]) -> u16 {
    let widest_row = plans
        .iter()
        .map(|plan| {
            Line::from(plan.display_name())
                .width()
                .saturating_add(if plan.is_production() { 6 } else { 0 })
                .saturating_add(4)
        })
        .max()
        .unwrap_or(0)
        .clamp(24, 41);
    u16::try_from(widest_row).unwrap_or(41)
}

pub(super) fn render_header(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &EnvironmentSession,
    selection: &EnvironmentSelection,
) {
    let Some(plan) = state.plans().get(selection.active()) else {
        return;
    };
    let title = format!(
        "terraleph ▸ {}",
        directory_display_name(state.exploration_root().unwrap_or_else(|| plan.directory()))
    );
    // The confirmation hides the raw review but keeps the same plan context.
    let tool = plan.plan_review().map_or_else(
        || plan.tool.display_name().to_owned(),
        |review| match review.context().tool_version() {
            ToolVersion::Known(version) => {
                format!("{} {version}", plan.tool.display_name())
            }
            ToolVersion::Loading | ToolVersion::Unavailable => plan.tool.display_name().to_owned(),
        },
    );
    let tool_width = Line::from(tool.as_str())
        .width()
        .min(usize::from(area.width));
    let title_width = usize::from(area.width).saturating_sub(tool_width.saturating_add(1));
    let title = truncate_middle(&title, title_width);
    let gap =
        usize::from(area.width).saturating_sub(Line::from(title.as_str()).width() + tool_width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(title, theme::overview_text_style()),
            Span::styled(" ".repeat(gap), theme::overview_text_style()),
            Span::styled(tool, theme::overview_text_style()),
        ]))
        .style(theme::overview_text_style()),
        area,
    );
}

pub(super) fn context(plan: &EnvironmentPlan) -> String {
    format!(
        "{}   ws:{}\nDirectory: {}",
        plan.tool.display_name(),
        plan.workspace().unwrap_or("not determined"),
        plan.directory().display()
    )
}

pub(super) const fn status(plan: &EnvironmentPlan) -> &'static str {
    match plan.state() {
        EnvironmentState::Unselected => "Not planned",
        EnvironmentState::Pending => "Pending",
        EnvironmentState::Running => match plan.preparation().stage() {
            Some(stage) => stage.title(),
            None => "Running",
        },
        EnvironmentState::Ready { .. } => "Ready",
        EnvironmentState::Error => "Error",
        EnvironmentState::ExcludedHcp => "Excluded: HCP execution",
    }
}

pub(super) fn preparation_detail(plan: &EnvironmentPlan) -> String {
    let preparation = plan.preparation();
    let mut lines = Vec::new();
    if preparation.stage() == Some(PreparationStage::Initializing)
        && let Some(reason) = preparation.initialization()
    {
        lines.push(format!("Initializing because {}.", reason.message()));
    }
    if let Some(change) = preparation.lock_file() {
        lines.push(change.message().to_owned());
    }
    if let Some(output) = preparation.latest_output() {
        lines.push(output.to_owned());
    }
    lines.join("\n")
}

// The failed step decides the next action; the diagnostic after it carries the tool's cause.
pub(super) fn failure_detail(plan: &EnvironmentPlan) -> String {
    let guidance = match plan.preparation().stage() {
        Some(PreparationStage::Initializing) => {
            "Init failed, so the plan did not run. Fix the cause, such as credentials or backend \
             access, then retry. Run init yourself when it needs -migrate-state, -reconfigure, or \
             -upgrade."
        }
        Some(PreparationStage::Planning) => {
            "Plan failed. Fix the cause, such as missing credentials, then retry."
        }
        Some(PreparationStage::Reading) | None => "Fix the cause, then retry.",
    };
    format!("{guidance}\n{}", plan.diagnostic().text())
}
