use super::{EnvironmentDialog, EnvironmentView, overview::matrix::MatrixSelectedItem, sidebar};
use crate::{
    app::{
        environments::{EnvironmentPlan, EnvironmentSession, EnvironmentState},
        session::ReviewSessionState,
    },
    ui::{
        features::{
            overview::{
                matrix,
                relations::{self, RelationGraphTitle, RelationGraphView},
            },
            plan_review,
        },
        primitives::molecules::{dialog_scroll::DialogScroll, help_dialog},
        shell::{environments, environments::EnvironmentPane, footer},
        theme,
    },
};
use ratatui::{
    Frame,
    layout::{Rect, Size},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::{fmt::Write, time::Instant};

impl EnvironmentView {
    pub(super) fn overview_page_size(&self, size: Size, state: &EnvironmentSession) -> usize {
        if self.selection.raw.is_some() {
            return 1;
        }
        let area = Rect::new(0, 0, size.width, size.height);
        let sidebar_visible = self.sidebar_visible(size.width);
        let layout = environments::overview_layout(
            area,
            self.sidebar_width,
            sidebar_visible,
            self.maximized_for_width(size.width),
            !sidebar_visible && self.maximized_for_width(size.width).is_none(),
            true,
        );
        let content = self.matrix_content_layout(pane_inner(layout.matrix), state);
        let legend_height = if content.matrix.width < 50 { 2 } else { 1 };
        usize::from(content.matrix.height.saturating_sub(2 + legend_height + 1)).max(1)
    }

    pub(crate) fn render(&mut self, frame: &mut Frame<'_>, state: &EnvironmentSession) {
        let area = frame.area();
        self.initialize(Size::new(area.width, area.height), state);
        self.sync(state);
        frame.render_widget(Block::new().style(theme::overview_background_style()), area);
        let layout = if self.selection.raw.is_some() {
            environments::overview_layout(area, self.sidebar_width, false, None, false, false)
        } else {
            let sidebar_visible = self.sidebar_visible(area.width);
            environments::overview_layout(
                area,
                self.sidebar_width,
                sidebar_visible,
                self.maximized_for_width(area.width),
                !sidebar_visible && self.maximized_for_width(area.width).is_none(),
                true,
            )
        };
        environments::render_header(frame, layout.header, state, &self.selection);
        if let Some(index) = self.selection.raw
            && let Some(review) = state.plans()[index].review()
        {
            let body = review_body(area, layout.header);
            if self.confirming_quit {
                plan_review::render_environment_with_quit_confirmation(
                    frame,
                    body,
                    review,
                    &mut self.reviews[index],
                    Instant::now(),
                    state.acquiring(),
                );
            } else {
                plan_review::render_environment(
                    frame,
                    body,
                    review,
                    &mut self.reviews[index],
                    Instant::now(),
                );
            }
        } else {
            self.render_overview(frame, &layout, state);
        }
        if self.confirming_quit && state.acquiring() {
            render_message_dialog(
                frame,
                area,
                "Stop acquiring environment plans?\nEnter stop and quit   Esc continue",
                &DialogScroll::default(),
            );
        } else if !self.confirming_quit
            && let Some(dialog) = &self.dialog
        {
            match dialog {
                EnvironmentDialog::Help => render_help_dialog(
                    frame,
                    area,
                    &self.dialog_scroll,
                    self.sidebar_enabled,
                    self.sidebar_enabled && area.width >= 90,
                    &matrix_pane_name(self, state),
                ),
                EnvironmentDialog::Message(text) => {
                    render_message_dialog(frame, area, text, &self.dialog_scroll);
                }
            }
        }
    }

    /// Draws the environment review behind the apply confirmation. The session no longer
    /// exposes a raw review while confirming, so the caller passes the confirming state.
    pub(crate) fn render_apply_confirmation(
        &mut self,
        frame: &mut Frame<'_>,
        state: &EnvironmentSession,
        index: usize,
        confirmation: &ReviewSessionState,
        confirmation_view: &plan_review::ApplyConfirmationViewState,
    ) {
        let area = frame.area();
        self.initialize(Size::new(area.width, area.height), state);
        self.sync(state);
        frame.render_widget(Block::new().style(theme::overview_background_style()), area);
        let layout =
            environments::overview_layout(area, self.sidebar_width, false, None, false, false);
        environments::render_header(frame, layout.header, state, &self.selection);
        plan_review::render_environment(
            frame,
            review_body(area, layout.header),
            confirmation,
            &mut self.reviews[index],
            Instant::now(),
        );
        plan_review::render_apply_confirmation_dialog(frame, confirmation, confirmation_view);
    }

    fn render_overview(
        &mut self,
        frame: &mut Frame<'_>,
        layout: &environments::EnvironmentLayout,
        state: &EnvironmentSession,
    ) {
        if layout.environments.width > 0 && layout.environments.height > 0 {
            sidebar::render(
                frame,
                layout.environments,
                state.plans(),
                self.selection.column,
                &self.compared_environments(state.plans().len()),
                self.active_pane(layout.header.width) == EnvironmentPane::Environments,
            );
        }
        if layout.summary.height > 0 {
            render_environment_summary(frame, layout.summary, state, self.selection.column);
        }
        if layout.matrix.width > 0 && layout.matrix.height > 0 {
            self.render_matrix_panel(
                frame,
                layout.matrix,
                state,
                self.active_pane(layout.header.width) == EnvironmentPane::Matrix,
            );
        }
        if layout.relations.width > 0 && layout.relations.height > 0 {
            self.render_relations_panel(
                frame,
                layout.relations,
                state,
                self.active_pane(layout.header.width) == EnvironmentPane::Relations,
            );
        }
        let matrix_state = if self.matrix.searching() {
            MatrixFooterState::Searching
        } else if self.matrix.filter().is_empty() {
            MatrixFooterState::Unfiltered
        } else {
            MatrixFooterState::Filtered
        };
        let focus = self.active_pane(layout.header.width);
        let matrix_selection = self.matrix.selected_item(self.selection.column);
        let enter_action = matches!(matrix_selection, Some(MatrixSelectedItem::Resource { .. }))
            .then_some(MatrixEnterAction::OpenRow);
        let footer_lines = if self.confirming_quit {
            if state.acquiring() {
                vec![Line::default()]
            } else {
                footer::quit_confirmation_lines(layout.footer.width, None)
            }
        } else {
            overview_footer(OverviewFooterContext {
                width: layout.footer.width,
                focus,
                matrix: matrix_state,
                expanded: self
                    .dialog
                    .is_none()
                    .then(|| self.matrix.selected_expanded())
                    .flatten(),
                comparison_toggle_available: self.dialog.is_none(),
                enter_action,
                selected: state.plans().get(self.selection.column),
                maximized: self.maximized.is_some(),
                environment_navigation: if self.sidebar_enabled {
                    EnvironmentNavigation::Multiple {
                        sidebar_available: layout.header.width >= 90,
                    }
                } else {
                    EnvironmentNavigation::Single
                },
                resize_guidance: layout.body.height < 3
                    || (focus == EnvironmentPane::Matrix
                        && (layout.matrix.width < 3 || layout.matrix.height < 3))
                    || (focus == EnvironmentPane::Relations
                        && (layout.relations.width < 3 || layout.relations.height < 3)),
            })
        };
        frame.render_widget(
            Paragraph::new(footer_lines).style(theme::overview_text_style()),
            layout.footer,
        );
    }

    fn render_matrix_panel(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        state: &EnvironmentSession,
        focused: bool,
    ) {
        let title = if area.height < 3 || area.width < 3 {
            "Resize terminal".to_owned()
        } else {
            matrix_title(self, state, area.width)
        };
        let block = pane_block(focused, &title, overview_pane_border_style(focused));
        let inner = block.inner(area);
        frame.render_widget(block, area);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        let layout = self.matrix_content_layout(inner, state);
        if layout.context_height > 0 {
            frame.render_widget(
                Paragraph::new(layout.context)
                    .wrap(Wrap { trim: false })
                    .style(theme::overview_muted_style()),
                Rect::new(inner.x, inner.y, inner.width, layout.context_height),
            );
        }
        if layout.detail_height > 0 {
            frame.render_widget(
                Paragraph::new(layout.detail)
                    .wrap(Wrap { trim: false })
                    .style(theme::overview_warning_style()),
                Rect::new(
                    inner.x,
                    inner.y.saturating_add(layout.context_height),
                    inner.width,
                    layout.detail_height,
                ),
            );
        }
        let show_same_change_toggle = focused
            && !self.confirming_quit
            && self.dialog.is_none()
            && !self.matrix.searching()
            && matches!(
                self.matrix.selected_item(self.selection.column),
                Some(MatrixSelectedItem::SameChanges)
            );
        matrix::render(
            frame,
            layout.matrix,
            state,
            &mut self.matrix,
            show_same_change_toggle,
        );
    }

    fn render_relations_panel(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        state: &EnvironmentSession,
        focused: bool,
    ) {
        let environment = state
            .plans()
            .get(self.selection.column)
            .map(environments::name);
        let scope = if environment.is_none() {
            "environment unavailable".to_owned()
        } else if self
            .compared_environments(state.plans().len())
            .contains(&self.selection.column)
        {
            self.selected_row_state_in_environment()
                .zip(environment.as_deref())
                .map_or_else(
                    || "whole env".to_owned(),
                    |(row, environment)| format!("whole env · {}", row.scope_note(environment)),
                )
        } else {
            "not compared".to_owned()
        };
        let title = RelationGraphTitle {
            environment: environment.as_deref(),
            scope: &scope,
        };
        let relation = self
            .environment_relations
            .as_ref()
            .and_then(|overview| overview.relations.get(&self.selection.column));
        if let Some(graph) = relation.and_then(|relation| relation.graph.as_ref()) {
            let scroll = self.relation_scrolls[self.selection.column];
            let rendered_scroll = relations::render(
                frame,
                area,
                graph,
                &RelationGraphView {
                    title,
                    selected_node: self.selected_relation_node(state),
                    focused,
                    maximized: self.maximized_for_width(area.width)
                        == Some(EnvironmentPane::Relations),
                    scroll,
                },
            );
            self.relation_scrolls[self.selection.column] = rendered_scroll;
        } else {
            let status = state.plans().get(self.selection.column).map_or_else(
                || "No environment is selected.".to_owned(),
                relations_status,
            );
            render_relations_status(frame, area, title, focused, &status);
        }
    }

    fn matrix_content_layout(&self, area: Rect, state: &EnvironmentSession) -> MatrixContentLayout {
        let context = overview_context(self, state);
        let detail = state
            .plans()
            .get(self.selection.column)
            .map(overview_detail)
            .unwrap_or_default();
        let (context_height, detail_height) = section_heights(area, &context, &detail);
        let y = area.y.saturating_add(context_height + detail_height);
        let matrix = Rect::new(area.x, y, area.width, area.bottom().saturating_sub(y));
        MatrixContentLayout {
            context,
            context_height,
            detail,
            detail_height,
            matrix,
        }
    }
}

const fn review_body(area: Rect, header: Rect) -> Rect {
    Rect::new(
        area.x,
        header.bottom(),
        area.width,
        area.bottom().saturating_sub(header.bottom()),
    )
}

fn render_message_dialog(frame: &mut Frame<'_>, area: Rect, text: &str, scroll: &DialogScroll) {
    let widget = Paragraph::new(text).wrap(Wrap { trim: false });
    let max = widget
        .line_count(area.width.max(1))
        .saturating_sub(usize::from(area.height));
    frame.render_widget(Clear, area);
    frame.render_widget(
        widget.scroll((
            scroll.clamp_for_render(u16::try_from(max).unwrap_or(u16::MAX)),
            0,
        )),
        area,
    );
}

fn render_environment_summary(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &EnvironmentSession,
    selected: usize,
) {
    let all = state
        .plans()
        .iter()
        .map(environment_summary_line)
        .collect::<Vec<_>>();
    let mut line = Line::default();
    for (index, part) in all.iter().enumerate() {
        if index > 0 {
            line.push_span(Span::styled("  │  ", theme::overview_muted_style()));
        }
        line.extend(part.spans.clone());
    }
    if line.width() > usize::from(area.width) {
        line = state
            .plans()
            .get(selected)
            .map_or_else(Line::default, environment_summary_line);
    }
    frame.render_widget(
        Paragraph::new(line).style(theme::overview_text_style()),
        area,
    );
}

fn environment_summary_line(plan: &EnvironmentPlan) -> Line<'static> {
    let mut line = Line::from(vec![
        Span::styled(environments::name(plan), theme::overview_text_style()),
        Span::styled(" ", theme::overview_muted_style()),
        environments::status_marker(plan.state()),
        Span::styled(
            environments::status(plan),
            environments::status_style(plan.state()),
        ),
    ]);
    if let Some(review) = plan.review() {
        let review = review.review();
        let counts = review.summary();
        for (count, label, style) in [
            (counts.creates, "+", theme::overview_total_add_style()),
            (counts.updates, "~", theme::overview_total_update_style()),
            (counts.deletes, "-", theme::overview_total_destroy_style()),
        ] {
            if count > 0 {
                line.push_span(Span::styled(format!(" {label}{count}"), style));
            }
        }
        if counts.replaces > 0 {
            line.push_span(Span::styled(
                format!(" {} replace", counts.replaces),
                theme::overview_total_replace_style(),
            ));
        }
        if !review.has_changes() {
            line.push_span(Span::styled(" No changes", theme::overview_muted_style()));
        }
    }
    line
}

fn relations_status(plan: &EnvironmentPlan) -> String {
    match plan.state() {
        EnvironmentState::Pending => {
            "Plan pending; relations will appear after acquisition.".to_owned()
        }
        EnvironmentState::Running => {
            "Plan running; relations will appear after acquisition.".to_owned()
        }
        EnvironmentState::Error => format!("Plan failed: {}", plan.diagnostic().text()),
        EnvironmentState::ExcludedHcp => {
            "Plan excluded because HCP performs the execution.".to_owned()
        }
        EnvironmentState::Ready { .. } => "Relations are unavailable for this plan.".to_owned(),
    }
}

fn render_relations_status(
    frame: &mut Frame<'_>,
    area: Rect,
    title: RelationGraphTitle<'_>,
    focused: bool,
    status: &str,
) {
    let block = Block::new()
        .borders(Borders::ALL)
        .title(relations::title_line(title, focused))
        .border_style(overview_pane_border_style(focused))
        .style(theme::overview_background_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width > 0 && inner.height > 0 {
        frame.render_widget(
            Paragraph::new(status)
                .wrap(Wrap { trim: false })
                .style(theme::overview_muted_style()),
            inner,
        );
    }
}

struct MatrixContentLayout {
    context: String,
    context_height: u16,
    detail: String,
    detail_height: u16,
    matrix: Rect,
}

fn pane_block(focused: bool, title: &str, border_style: Style) -> Block<'static> {
    let mark = if focused { "* " } else { "  " };
    let (pane_name, context) = title.split_once(" · ").unwrap_or((title, ""));
    let mut title_spans = vec![
        Span::styled(
            mark,
            if focused {
                Style::default().fg(Color::Cyan).bg(Color::Reset)
            } else {
                theme::overview_muted_style()
            },
        ),
        Span::styled(pane_name.to_owned(), theme::overview_pane_title_style()),
    ];
    if !context.is_empty() {
        title_spans.push(Span::styled(
            format!(" · {context}"),
            theme::overview_header_muted_style(),
        ));
    }
    Block::new()
        .borders(Borders::ALL)
        .title(Line::from(title_spans))
        .border_style(border_style)
        .style(theme::overview_background_style())
}

fn pane_inner(area: Rect) -> Rect {
    pane_block(false, "", overview_pane_border_style(false)).inner(area)
}

fn overview_pane_border_style(focused: bool) -> Style {
    Style::default()
        .fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        })
        .bg(Color::Reset)
}

fn overview_context(view: &EnvironmentView, state: &EnvironmentSession) -> String {
    if let Some(notice) = &view.notice {
        return notice.clone();
    }
    if view.matrix.searching() || view.matrix.filtered() {
        return format!("Filter: /{}   (display only)", view.matrix.filter());
    }
    if !view
        .compared_environments(state.plans().len())
        .contains(&view.selection.column)
    {
        return "Selected environment is excluded from the comparison.".to_owned();
    }
    String::new()
}

fn matrix_title(view: &EnvironmentView, state: &EnvironmentSession, width: u16) -> String {
    let compared = view.compared_environments(state.plans().len());
    let mut title = format!("[2] {}", matrix_pane_name(view, state));
    if compared.len() < state.plans().len() {
        if width < 50 {
            let _ = write!(
                title,
                " · Filtered {}/{}",
                compared.len(),
                state.plans().len()
            );
        } else {
            let _ = write!(
                title,
                " · Filtered {}/{} envs",
                compared.len(),
                state.plans().len()
            );
        }
    }
    let ready = compared
        .iter()
        .filter(|index| {
            state
                .plans()
                .get(**index)
                .is_some_and(|plan| matches!(plan.state(), EnvironmentState::Ready { .. }))
        })
        .count();
    if ready < compared.len() {
        let _ = write!(title, " · Ready {ready}/{}", compared.len());
    }
    title
}

fn matrix_pane_name(view: &EnvironmentView, state: &EnvironmentSession) -> String {
    let compared = view.compared_environments(state.plans().len());
    if compared.len() == 1 {
        format!(
            "Changes · {}",
            environments::name(&state.plans()[compared[0]])
        )
    } else {
        "Compare".to_owned()
    }
}

fn overview_detail(plan: &EnvironmentPlan) -> String {
    if matches!(plan.state(), EnvironmentState::Error) {
        return plan.diagnostic().text().to_owned();
    }
    let Some(review) = plan.review().map(ReviewSessionState::review) else {
        return String::new();
    };
    let mut notes = Vec::new();
    let count = review.nonstandard_changes();
    let outputs = review.changed_outputs() > 0;
    if count > 0 || outputs {
        let detail = if count > 0 && outputs {
            format!("{count} other change(s) and output changes")
        } else if count > 0 {
            format!("{count} other change(s)")
        } else {
            "output changes".to_owned()
        };
        notes.push(format!("Other changes: {detail}. v opens the full plan."));
    }
    let drift = review.noted_drift();
    if drift > 0 {
        notes.push(format!("Drift detected in {drift} resource(s)."));
    }
    notes.join(" ")
}

fn section_heights(area: Rect, context: &str, detail: &str) -> (u16, u16) {
    let context_height = u16::try_from(
        Paragraph::new(context)
            .wrap(Wrap { trim: false })
            .line_count(area.width.max(1)),
    )
    .unwrap_or(u16::MAX)
    .min(area.height.saturating_sub(5));
    let detail_height = u16::try_from(
        Paragraph::new(detail)
            .wrap(Wrap { trim: false })
            .line_count(area.width.max(1)),
    )
    .unwrap_or(u16::MAX)
    .min(3)
    .min(area.height.saturating_sub(context_height + 5));
    (context_height, detail_height)
}

fn render_help_dialog(
    frame: &mut Frame<'_>,
    area: Rect,
    scroll: &DialogScroll,
    sidebar_enabled: bool,
    sidebar_available: bool,
    matrix_name: &str,
) {
    help_dialog::render(
        frame,
        area,
        "Help",
        &overview_help_sections(sidebar_enabled, sidebar_available, matrix_name),
        scroll,
    );
}

fn overview_help_sections(
    sidebar_enabled: bool,
    sidebar_available: bool,
    matrix_name: &str,
) -> Vec<help_dialog::HelpSection> {
    let mut current_actions = vec![help_dialog::HelpAction::new(
        "↑ / ↓ / j / k",
        if sidebar_available {
            "select environments in [1], rows in [2], or scroll [3]"
        } else {
            "select rows in [2] or scroll [3]"
        },
    )];
    current_actions.push(help_dialog::HelpAction::new(
        "Space",
        if sidebar_available {
            "toggle an environment in [1]; a group or summary in [2]"
        } else {
            "toggle a group or the Same change summary in [2]"
        },
    ));
    if sidebar_available {
        current_actions.push(help_dialog::HelpAction::new(
            "o / a",
            "compare the selected / all environments in [1]",
        ));
    }
    if sidebar_enabled {
        current_actions.push(help_dialog::HelpAction::new(
            "[ / ]",
            "select the previous or next environment",
        ));
    }
    current_actions.push(if sidebar_available {
        help_dialog::HelpAction::new(
            "1 / 2 / 3",
            format!("focus Envs / {matrix_name} / Relations; 1 opens Envs"),
        )
    } else {
        help_dialog::HelpAction::new("2 / 3", format!("focus {matrix_name} / Relations"))
    });
    if sidebar_available {
        current_actions.push(help_dialog::HelpAction::new("b", "toggle the Envs sidebar"));
    }
    current_actions.extend([
        help_dialog::HelpAction::new(
            "f",
            format!("maximize or restore [2] {matrix_name} / [3] Relations"),
        ),
        help_dialog::HelpAction::new(
            "Enter",
            if sidebar_available {
                "[2] opens the selected source; [1] or [3] the plan top"
            } else {
                "[2] opens the selected source; [3] the plan top"
            },
        ),
        help_dialog::HelpAction::new("/", "filter [2] addresses; [3] still shows everything"),
        help_dialog::HelpAction::new("r", "retry the selected Error environment"),
    ]);
    let current_title = if sidebar_enabled {
        "Current: Multi-environment Overview"
    } else {
        "Current: Overview"
    };
    vec![
        help_dialog::HelpSection::new(current_title, current_actions),
        other_overview_help(sidebar_available),
        matrix_legend_help(),
        comparison_help(),
        environment_status_help(sidebar_enabled),
        relations::help_section(),
    ]
}

fn environment_status_help(multiple: bool) -> help_dialog::HelpSection {
    let mut actions = vec![help_dialog::HelpAction::new(
        "✓ Ready",
        "plan acquired; not a judgment of apply safety",
    )];
    if multiple {
        actions.extend([
            help_dialog::HelpAction::new("✗ Error", "acquisition failed; r retries the plan"),
            help_dialog::HelpAction::new("Pending / Running", "plan acquisition is incomplete"),
            help_dialog::HelpAction::new("Excluded", "HCP performs the plan execution"),
        ]);
    }
    help_dialog::HelpSection::new("Plan status", actions)
}

fn other_overview_help(sidebar_available: bool) -> help_dialog::HelpSection {
    help_dialog::HelpSection::new(
        "Other",
        vec![
            help_dialog::HelpAction::new("← / → / h / l", "scroll columns in [2] or [3]"),
            help_dialog::HelpAction::new("PgUp / PgDn", "move rows in [2] or scroll [3]"),
            help_dialog::HelpAction::new(
                "Home / End / g / G",
                if sidebar_available {
                    "first/last item in [1] or [2]; scroll [3] to an edge"
                } else {
                    "first/last row in [2]; scroll [3] to an edge"
                },
            ),
            help_dialog::HelpAction::new("v", "open the full plan from the top"),
            help_dialog::HelpAction::new("y", "copy the selected environment's plan"),
            help_dialog::HelpAction::new("c", "show environment context"),
            help_dialog::HelpAction::new("?", "show or close this help"),
            help_dialog::HelpAction::new("q", "quit; confirms first while acquiring"),
        ],
    )
}

fn matrix_legend_help() -> help_dialog::HelpSection {
    help_dialog::HelpSection::new(
        "Matrix legend",
        vec![
            help_dialog::HelpAction::new("+ / ~ / -", "create / update / delete"),
            help_dialog::HelpAction::new("+/- / -/+", "replace (create→delete / delete→create)"),
            help_dialog::HelpAction::new("blank", "resource absent from this environment"),
            help_dialog::HelpAction::new(".", "resource present, with no change"),
            help_dialog::HelpAction::new("?", "plan unavailable; action unknown"),
            help_dialog::HelpAction::new(
                "[unknown values]",
                "known changes match; unknown values may still differ",
            ),
        ],
    )
}

fn comparison_help() -> help_dialog::HelpSection {
    help_dialog::HelpSection::new(
        "Comparison",
        vec![
            help_dialog::HelpAction::new(
                "Same changes",
                "same in Ready plans; unknown values may differ",
            ),
            help_dialog::HelpAction::new(
                "N patterns",
                "counts [2] rows, not resources; group sizes may vary",
            ),
            help_dialog::HelpAction::new(
                "Excluded",
                "environments remain selectable and are not retried",
            ),
            help_dialog::HelpAction::new("Scope", "only Ready plans are compared"),
            help_dialog::HelpAction::new(
                "only in / not in",
                "why column: present in only some Ready plans",
            ),
        ],
    )
}

#[derive(Clone, Copy)]
enum MatrixFooterState {
    Searching,
    Filtered,
    Unfiltered,
}

// The Same change summary has no Enter action; Space alone expands it, keeping one key per action.
#[derive(Clone, Copy)]
enum MatrixEnterAction {
    OpenRow,
}

impl MatrixEnterAction {
    const fn label(self, compact: bool) -> &'static str {
        match (self, compact) {
            (Self::OpenRow, true) => "open row",
            (Self::OpenRow, false) => "open selected row",
        }
    }
}

#[derive(Clone, Copy)]
enum EnvironmentNavigation {
    Single,
    Multiple { sidebar_available: bool },
}

impl EnvironmentNavigation {
    const fn is_multiple(self) -> bool {
        matches!(self, Self::Multiple { .. })
    }

    const fn sidebar_available(self) -> bool {
        matches!(
            self,
            Self::Multiple {
                sidebar_available: true
            }
        )
    }
}

#[derive(Clone, Copy)]
struct OverviewFooterContext<'a> {
    width: u16,
    focus: environments::EnvironmentPane,
    matrix: MatrixFooterState,
    expanded: Option<bool>,
    comparison_toggle_available: bool,
    enter_action: Option<MatrixEnterAction>,
    selected: Option<&'a EnvironmentPlan>,
    maximized: bool,
    environment_navigation: EnvironmentNavigation,
    resize_guidance: bool,
}

fn overview_footer(context: OverviewFooterContext<'_>) -> Vec<Line<'static>> {
    let OverviewFooterContext {
        width,
        focus,
        matrix,
        expanded,
        comparison_toggle_available,
        enter_action,
        selected,
        maximized,
        environment_navigation,
        resize_guidance,
    } = context;
    if resize_guidance {
        return footer::layout_prioritized(
            vec![
                (
                    80,
                    Line::from(Span::styled(
                        "Resize terminal to view pane content",
                        theme::overview_footer_text_style(),
                    )),
                ),
                (110, overview_footer_hint(&["?"], "help")),
                (120, overview_footer_hint(&["q"], "quit")),
            ],
            width,
        );
    }
    if matches!(matrix, MatrixFooterState::Searching) {
        return footer::layout(
            vec![
                overview_footer_hint(&["Enter"], "confirm"),
                overview_footer_hint(&["Esc"], "cancel"),
            ],
            width,
        );
    }
    if width < 45 {
        return compact_overview_footer(context);
    }
    let mut items = Vec::new();
    if focus == environments::EnvironmentPane::Environments {
        items.push((100, overview_footer_hint(&["Enter"], "open plan")));
        if comparison_toggle_available {
            items.push((90, overview_footer_hint(&["Space"], "toggle")));
        }
        items.push((55, overview_footer_hint(&["o"], "only")));
        items.push((55, overview_footer_hint(&["a"], "all")));
    } else if focus == environments::EnvironmentPane::Matrix {
        if let Some(action) = enter_action {
            items.push((100, overview_footer_hint(&["Enter"], action.label(false))));
        }
        items.push((75, overview_footer_hint(&["v"], "full plan")));
        items.push((65, overview_footer_hint(&["/"], "filter")));
        if let Some(expanded) = expanded {
            items.push((
                90,
                overview_footer_hint(
                    &["Space"],
                    if expanded {
                        "collapse selected"
                    } else {
                        "expand selected"
                    },
                ),
            ));
        }
    } else {
        // In [3] Enter and v both open the plan from the top, so only Enter is listed.
        items.push((100, overview_footer_hint(&["Enter"], "open plan")));
    }
    if selected.is_some_and(|plan| matches!(plan.state(), EnvironmentState::Error)) {
        items.push((95, overview_footer_hint(&["r"], "retry")));
    }
    items.extend(overview_common_footer_items(
        focus,
        maximized,
        environment_navigation,
    ));
    footer::layout_prioritized(items, width)
}

fn overview_common_footer_items(
    focus: environments::EnvironmentPane,
    maximized: bool,
    environment_navigation: EnvironmentNavigation,
) -> Vec<(u8, Line<'static>)> {
    let sidebar_available = environment_navigation.sidebar_available();
    let mut items = Vec::new();
    if environment_navigation.is_multiple() {
        items.push((70, overview_footer_hint(&["[", "]"], "env")));
    }
    if sidebar_available && !maximized {
        items.push((45, overview_footer_hint(&["b"], "toggle envs")));
    }
    if focus != environments::EnvironmentPane::Environments {
        items.push((
            50,
            if maximized {
                overview_footer_hint(&["f", "Esc"], "restore")
            } else {
                overview_footer_hint(&["f"], "maximize")
            },
        ));
    }
    items.push((110, overview_footer_hint(&["?"], "help")));
    items.push((120, overview_footer_hint(&["q"], "quit")));
    items
}

fn compact_overview_footer(context: OverviewFooterContext<'_>) -> Vec<Line<'static>> {
    let OverviewFooterContext {
        width,
        focus,
        expanded,
        comparison_toggle_available,
        enter_action,
        selected,
        maximized,
        environment_navigation,
        ..
    } = context;
    let mut items = Vec::new();
    if focus == environments::EnvironmentPane::Environments {
        items.push((100, overview_footer_hint(&["Enter"], "open plan")));
        if comparison_toggle_available {
            items.push((90, overview_footer_hint(&["Space"], "toggle")));
        }
    } else if focus == environments::EnvironmentPane::Matrix {
        if let Some(action) = enter_action {
            items.push((100, overview_footer_hint(&["Enter"], action.label(true))));
        }
        if let Some(expanded) = expanded {
            items.push((
                90,
                overview_footer_hint(&["Space"], if expanded { "collapse" } else { "expand" }),
            ));
        }
    } else {
        items.push((100, overview_footer_hint(&["Enter"], "open plan")));
    }
    if selected.is_some_and(|plan| matches!(plan.state(), EnvironmentState::Error)) {
        items.push((95, overview_footer_hint(&["r"], "retry")));
    }
    if focus == environments::EnvironmentPane::Matrix {
        items.push((75, overview_footer_hint(&["v"], "full plan")));
    }
    items.extend(overview_common_footer_items(
        focus,
        maximized,
        environment_navigation,
    ));
    footer::layout_prioritized(items, width)
}

fn overview_footer_hint(keys: &[&'static str], description: &'static str) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("/", theme::overview_footer_separator_style()));
        }
        spans.push(Span::styled(*key, theme::overview_footer_key_style()));
    }
    spans.push(Span::styled(
        format!(" {description}"),
        theme::overview_footer_text_style(),
    ));
    Line::from(spans)
}
