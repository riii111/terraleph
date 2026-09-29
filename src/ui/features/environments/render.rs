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
        primitives::{
            atoms::focus_mark,
            molecules::{dialog_scroll::DialogScroll, help_dialog},
        },
        shell::{environments, environments::EnvironmentPane, footer},
        theme,
    },
};
use ratatui::{
    Frame,
    layout::{Rect, Size},
    style::Style,
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::{fmt::Write, time::Instant};

impl EnvironmentView {
    pub(super) fn overview_page_size(&self, size: Size, state: &EnvironmentSession) -> usize {
        if self.selection.raw.is_some() {
            return 1;
        }
        let layout = self.overview_layout(Rect::new(0, 0, size.width, size.height));
        let content = self.matrix_content_layout(pane_inner(layout.matrix), state);
        let legend_height = if content.matrix.width < 50 { 2 } else { 1 };
        usize::from(content.matrix.height.saturating_sub(2 + legend_height + 1)).max(1)
    }

    pub(crate) fn render(&mut self, frame: &mut Frame<'_>, state: &EnvironmentSession) {
        let area = frame.area();
        self.initialize(Size::new(area.width, area.height), state);
        self.sync(state);
        frame.render_widget(Block::new().style(theme::overview_text_style()), area);
        if let Some(index) = self.selection.raw
            && let Some(review) = state.plans()[index].review()
        {
            let header = header_row(area);
            environments::render_header(frame, header, state, &self.selection);
            let body = review_body(area, header);
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
            let layout = self.overview_layout(area);
            environments::render_header(frame, layout.header, state, &self.selection);
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
        now: Instant,
    ) {
        let area = frame.area();
        self.initialize(Size::new(area.width, area.height), state);
        self.sync(state);
        frame.render_widget(Block::new().style(theme::overview_text_style()), area);
        let header = header_row(area);
        environments::render_header(frame, header, state, &self.selection);
        plan_review::render_environment(
            frame,
            review_body(area, header),
            confirmation,
            &mut self.reviews[index],
            now,
        );
        plan_review::render_apply_confirmation_dialog(
            frame,
            confirmation,
            confirmation_view,
            Some(header),
            now,
        );
    }

    pub(super) fn overview_layout(&self, area: Rect) -> environments::EnvironmentLayout {
        environments::overview_layout(
            area,
            self.sidebar_width,
            self.sidebar_visible(area.width),
            self.maximized_for_width(area.width),
        )
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
        let focus = self.active_pane(layout.header.width);
        let row_selected = matches!(
            self.matrix.selected_item(self.selection.column),
            Some(MatrixSelectedItem::Resource { .. })
        );
        let footer_lines = if self.confirming_quit {
            if state.acquiring() {
                vec![Line::default()]
            } else {
                footer::quit_confirmation_lines(layout.footer.width)
            }
        } else {
            overview_footer(OverviewFooterContext {
                width: layout.footer.width,
                focus,
                searching: self.matrix.searching(),
                expanded: self
                    .dialog
                    .is_none()
                    .then(|| self.matrix.selected_expanded())
                    .flatten(),
                comparison_toggle_available: self.dialog.is_none(),
                row_selected,
                selected: state.plans().get(self.selection.column),
                maximized: self.maximized.is_some(),
                multiple: self.sidebar_enabled,
                sidebar_available: self.sidebar_enabled && layout.header.width >= 90,
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
        let block = pane_block(focused, &title, theme::relation_frame_style(focused));
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
            .map(EnvironmentPlan::display_name);
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

const fn header_row(area: Rect) -> Rect {
    Rect::new(
        area.x,
        area.y,
        area.width,
        if area.height > 0 { 1 } else { 0 },
    )
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
        Span::styled(plan.display_name(), theme::overview_text_style()),
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
        .border_style(theme::relation_frame_style(focused))
        .style(theme::overview_text_style());
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
    let (pane_name, context) = title.split_once(" · ").unwrap_or((title, ""));
    let mut title_spans = vec![
        focus_mark::render(focused),
        Span::styled(pane_name.to_owned(), theme::overview_pane_title_style()),
    ];
    if !context.is_empty() {
        title_spans.push(Span::styled(
            format!(" · {context}"),
            theme::overview_muted_style(),
        ));
    }
    Block::new()
        .borders(Borders::ALL)
        .title(Line::from(title_spans))
        .border_style(border_style)
        .style(theme::overview_text_style())
}

fn pane_inner(area: Rect) -> Rect {
    pane_block(false, "", theme::relation_frame_style(false)).inner(area)
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
        format!("Changes · {}", state.plans()[compared[0]].display_name())
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
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent footer condition from the environment view"
)]
struct OverviewFooterContext<'a> {
    width: u16,
    focus: environments::EnvironmentPane,
    searching: bool,
    expanded: Option<bool>,
    comparison_toggle_available: bool,
    // The Same change summary has no Enter action; Space alone expands it.
    row_selected: bool,
    selected: Option<&'a EnvironmentPlan>,
    maximized: bool,
    multiple: bool,
    sidebar_available: bool,
    resize_guidance: bool,
}

fn overview_footer(context: OverviewFooterContext<'_>) -> Vec<Line<'static>> {
    let OverviewFooterContext {
        width,
        focus,
        searching,
        expanded,
        comparison_toggle_available,
        row_selected,
        selected,
        maximized,
        multiple,
        sidebar_available,
        resize_guidance,
    } = context;
    if resize_guidance {
        return resize_guidance_footer(width);
    }
    if searching {
        return footer::layout(
            vec![
                footer::overview_hint(&["Enter"], "confirm"),
                footer::overview_hint(&["Esc"], "cancel"),
            ],
            width,
        );
    }
    let compact = width < 45;
    let mut items = Vec::new();
    match focus {
        EnvironmentPane::Environments => {
            items.push((100, footer::overview_hint(&["Enter"], "open plan")));
            if comparison_toggle_available {
                items.push((90, footer::overview_hint(&["Space"], "toggle")));
            }
            if !compact {
                items.push((55, footer::overview_hint(&["o"], "only")));
                items.push((55, footer::overview_hint(&["a"], "all")));
            }
        }
        EnvironmentPane::Matrix => {
            if row_selected {
                let label = if compact {
                    "open row"
                } else {
                    "open selected row"
                };
                items.push((100, footer::overview_hint(&["Enter"], label)));
            }
            if !compact {
                items.push((75, footer::overview_hint(&["v"], "full plan")));
                items.push((65, footer::overview_hint(&["/"], "filter")));
            }
            if let Some(expanded) = expanded {
                let label = match (expanded, compact) {
                    (true, true) => "collapse",
                    (true, false) => "collapse selected",
                    (false, true) => "expand",
                    (false, false) => "expand selected",
                };
                items.push((90, footer::overview_hint(&["Space"], label)));
            }
        }
        // In [3] Enter and v both open the plan from the top, so only Enter is listed.
        EnvironmentPane::Relations => {
            items.push((100, footer::overview_hint(&["Enter"], "open plan")));
        }
    }
    if selected.is_some_and(|plan| matches!(plan.state(), EnvironmentState::Error)) {
        items.push((95, footer::overview_hint(&["r"], "retry")));
    }
    if compact && focus == EnvironmentPane::Matrix {
        items.push((75, footer::overview_hint(&["v"], "full plan")));
    }
    if multiple {
        items.push((70, footer::overview_hint(&["[", "]"], "env")));
    }
    if sidebar_available && !maximized {
        items.push((45, footer::overview_hint(&["b"], "toggle envs")));
    }
    if focus != EnvironmentPane::Environments {
        items.push((
            50,
            if maximized {
                footer::overview_hint(&["f", "Esc"], "restore")
            } else {
                footer::overview_hint(&["f"], "maximize")
            },
        ));
    }
    items.push((110, footer::overview_hint(&["?"], "help")));
    items.push((120, footer::overview_hint(&["q"], "quit")));
    footer::layout_prioritized(items, width)
}

fn resize_guidance_footer(width: u16) -> Vec<Line<'static>> {
    footer::layout_prioritized(
        vec![
            (
                80,
                Line::from(Span::styled(
                    "Resize terminal to view pane content",
                    theme::overview_text_style(),
                )),
            ),
            (110, footer::overview_hint(&["?"], "help")),
            (120, footer::overview_hint(&["q"], "quit")),
        ],
        width,
    )
}
