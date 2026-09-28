use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::Paragraph,
};

use super::context::truncate_middle;
use crate::{
    app::{
        environments::{EnvironmentPlan, EnvironmentSession, EnvironmentState},
        execution::{ExecutionContextValue, directory_display_name},
        session::SessionState,
    },
    ui::theme,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvironmentPane {
    Environments,
    Matrix,
    Relations,
}

pub(crate) fn status_marker(state: &EnvironmentState) -> Span<'static> {
    let (symbol, style) = match state {
        EnvironmentState::Ready { .. } => ("✓", theme::overview_total_add_style()),
        EnvironmentState::Error => ("✗", theme::overview_total_destroy_style()),
        EnvironmentState::Pending | EnvironmentState::Running | EnvironmentState::ExcludedHcp => {
            ("", theme::overview_muted_style())
        }
    };
    fixed_status_marker(symbol, style)
}

pub(crate) fn ready_status_marker() -> Span<'static> {
    fixed_status_marker("✓", theme::overview_total_add_style())
}

fn fixed_status_marker(symbol: &str, style: ratatui::style::Style) -> Span<'static> {
    const WIDTH: usize = 2;
    let padding = WIDTH.saturating_sub(Line::from(symbol).width());
    Span::styled(format!("{symbol}{}", " ".repeat(padding)), style)
}

pub(crate) fn status_style(state: &EnvironmentState) -> ratatui::style::Style {
    match state {
        EnvironmentState::Pending | EnvironmentState::Running => theme::overview_muted_style(),
        EnvironmentState::Ready { .. } => theme::overview_text_style(),
        EnvironmentState::Error => theme::overview_total_destroy_style(),
        EnvironmentState::ExcludedHcp => theme::overview_warning_style(),
    }
}

#[derive(Default)]
pub(crate) struct EnvironmentSelection {
    pub(crate) column: usize,
    pub(crate) raw: Option<usize>,
}

pub(crate) struct EnvironmentLayout {
    pub(crate) header: Rect,
    pub(crate) summary: Rect,
    pub(crate) body: Rect,
    pub(crate) footer: Rect,
    pub(crate) environments: Rect,
    pub(crate) matrix: Rect,
    pub(crate) relations: Rect,
}

impl EnvironmentSelection {
    pub(crate) fn active(&self) -> usize {
        self.raw.unwrap_or(self.column)
    }
}

pub(crate) fn overview_layout(
    area: Rect,
    sidebar_width: u16,
    sidebar_visible: bool,
    maximized: Option<EnvironmentPane>,
    show_summary: bool,
    show_relations: bool,
) -> EnvironmentLayout {
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
            if show_relations {
                let matrix_height = right.height.saturating_mul(4) / 10;
                matrix = Rect::new(right.x, right.y, right.width, matrix_height);
                relations = Rect::new(
                    right.x,
                    right.y.saturating_add(matrix_height),
                    right.width,
                    right.height.saturating_sub(matrix_height),
                );
            } else {
                matrix = right;
            }
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

pub(crate) fn sidebar_width(plans: &[EnvironmentPlan]) -> u16 {
    let widest_row = plans
        .iter()
        .map(|plan| {
            Line::from(name(plan))
                .width()
                .saturating_add(if plan.is_production() { 6 } else { 0 })
                .saturating_add(4)
        })
        .max()
        .unwrap_or(0)
        .clamp(24, 41);
    u16::try_from(widest_row).unwrap_or(41)
}

pub(crate) fn render_header(
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
    let review = plan
        .review()
        .or_else(|| plan.session().and_then(SessionState::apply_confirmation));
    let tool = review.map_or_else(
        || plan.tool.display_name().to_owned(),
        |review| {
            let review = review.review();
            match review.context().tool_version() {
                ExecutionContextValue::Known(version) => {
                    format!("{} {version}", plan.tool.display_name())
                }
                ExecutionContextValue::Loading => plan.tool.display_name().to_owned(),
            }
        },
    );
    let tool_width = Line::from(tool.as_str())
        .width()
        .min(usize::from(area.width));
    let title_width = usize::from(area.width).saturating_sub(tool_width.saturating_add(1));
    let title = fit_end(&title, title_width);
    let gap =
        usize::from(area.width).saturating_sub(Line::from(title.as_str()).width() + tool_width);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(title, theme::overview_header_style()),
            Span::styled(" ".repeat(gap), theme::overview_header_style()),
            Span::styled(tool, theme::overview_text_style()),
        ]))
        .style(theme::overview_header_style()),
        area,
    );
}

pub(crate) fn name(plan: &EnvironmentPlan) -> String {
    plan.display_name()
}

pub(crate) fn context(plan: &EnvironmentPlan) -> String {
    format!(
        "{}   ws:{}\nDirectory: {}",
        plan.tool.display_name(),
        plan.workspace().unwrap_or("unavailable"),
        plan.directory().display()
    )
}

pub(crate) const fn status(plan: &EnvironmentPlan) -> &'static str {
    match plan.state() {
        EnvironmentState::Pending => "Pending",
        EnvironmentState::Running => "Running",
        EnvironmentState::Ready { .. } => "Ready",
        EnvironmentState::Error => "Error",
        EnvironmentState::ExcludedHcp => "Excluded: HCP execution",
    }
}

fn fit_end(value: &str, width: usize) -> String {
    truncate_middle(value, width)
}
