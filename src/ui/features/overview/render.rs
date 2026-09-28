use std::time::Instant;

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::app::{
    copy::CopyNotice, execution::ExecutionContextValue, review::PlanReview,
    session::ReviewSessionState,
};
use crate::ui::{
    primitives::{
        atoms::{scrollbar, separator},
        molecules::{context_dialog, help_dialog, terminal_notice},
    },
    shell::{context, environments, footer, header, layout as shell_layout},
    theme,
};

use super::{
    OverviewContent, OverviewOverlay, OverviewPane, OverviewViewState,
    relations::{self, RelationGraphTitle, RelationGraphView},
};

const MIN_WIDTH: u16 = 40;
const MIN_HEIGHT: u16 = 16;

pub(crate) struct OverviewLayout {
    shell: shell_layout::ShellLayout,
    status: Rect,
    separator: Rect,
    changes: Rect,
    relations: Rect,
    changes_body: Rect,
    max_vertical: u16,
}

struct PreparedOverview {
    layout: OverviewLayout,
    lines: Vec<Line<'static>>,
}

impl OverviewLayout {
    pub(crate) const fn status(&self) -> Rect {
        self.status
    }

    pub(crate) const fn separator(&self) -> Rect {
        self.separator
    }

    pub(crate) const fn changes_body(&self) -> Rect {
        self.changes_body
    }

    pub(crate) const fn relations(&self) -> Rect {
        self.relations
    }

    pub(crate) const fn max_vertical(&self) -> u16 {
        self.max_vertical
    }
}

pub(crate) fn layout(
    area: Rect,
    state: &ReviewSessionState,
    view: &OverviewViewState,
    content: &OverviewContent,
) -> OverviewLayout {
    prepare(area, state, view, content, false).layout
}

pub(crate) fn reconcile_view(area: Rect, state: &ReviewSessionState, view: &mut OverviewViewState) {
    let content = OverviewContent::project(state, view.filter(), view.expanded());
    let max_vertical = layout(area, state, view, &content).max_vertical();
    view.reconcile(max_vertical, content.rows.len());
}

fn prepare(
    area: Rect,
    state: &ReviewSessionState,
    view: &OverviewViewState,
    content: &OverviewContent,
    quit_confirmation: bool,
) -> PreparedOverview {
    let footer_message = state.copy_feedback().notice().map(CopyNotice::message);
    let available_footer_width = footer::available_width(area.width, footer_message);
    let full_footer =
        footer::layout_prioritized(footer_items(view, content), available_footer_width);
    let required_footer =
        footer::layout_prioritized(required_footer_items(view), available_footer_width);
    let (full_footer, required_footer) = if quit_confirmation {
        let lines = footer::quit_confirmation_lines(area.width, footer_message);
        (
            footer::pad_lines(lines.clone(), full_footer.len()),
            footer::pad_lines(lines, required_footer.len()),
        )
    } else {
        (full_footer, required_footer)
    };
    let shell = shell_layout::full_width_layout(area, full_footer, required_footer);
    let inner = shell.content_inner();
    let status = Rect::new(inner.x, inner.y, inner.width, 1);
    let separator = Rect::new(inner.x, inner.y.saturating_add(1), inner.width, 1);
    let body = Rect::new(
        inner.x,
        inner.y.saturating_add(2),
        inner.width,
        inner.height.saturating_sub(2),
    );
    let (changes, relations) = pane_areas(body, view.maximized());
    let changes_body = Block::bordered().inner(changes);
    let lines = overview_lines(content, state.review(), view);
    let line_count = lines.len();
    let max_vertical = u16::try_from(line_count.saturating_sub(usize::from(changes_body.height)))
        .unwrap_or(u16::MAX);
    PreparedOverview {
        layout: OverviewLayout {
            shell,
            status,
            separator,
            changes,
            relations,
            changes_body,
            max_vertical,
        },
        lines,
    }
}

pub(crate) fn render_with_quit_confirmation(
    frame: &mut Frame<'_>,
    state: &ReviewSessionState,
    view: &OverviewViewState,
    now: Instant,
    quit_confirmation: bool,
) {
    let area = frame.area();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_terminal_size_notice(frame, area, quit_confirmation);
        return;
    }
    let content = OverviewContent::project(state, view.filter(), view.expanded());
    let PreparedOverview {
        layout,
        lines: prepared_lines,
    } = prepare(area, state, view, &content, quit_confirmation);
    if (layout.changes.width == 0 || layout.changes.height == 0)
        && (layout.relations.width == 0 || layout.relations.height == 0)
    {
        render_terminal_size_notice(frame, area, quit_confirmation);
        return;
    }
    header::render_overview_review(frame, layout.shell.header(), state.review());
    frame.render_widget(
        Block::new().style(theme::overview_background_style()),
        layout.shell.content(),
    );
    frame.render_widget(
        Paragraph::new(status_line(state, view)).style(theme::overview_text_style()),
        layout.status(),
    );
    frame.render_widget(
        separator::render(layout.separator().width),
        layout.separator(),
    );

    let lines = if state.copy_feedback().flash_active(now) {
        copy_flash_lines(prepared_lines)
    } else {
        prepared_lines
    };
    if layout.changes.width > 0 && layout.changes.height > 0 {
        render_changes_panel(
            frame,
            layout.changes,
            &lines,
            view,
            layout.max_vertical(),
            state.review(),
        );
    }
    if layout.relations.width > 0 && layout.relations.height > 0 {
        let scroll = relations::render(
            frame,
            layout.relations,
            state.prepared_overview().relations(),
            &RelationGraphView {
                title: RelationGraphTitle {
                    environment: None,
                    scope: "whole env",
                },
                selected_node: view.selected_node_id(&content),
                focused: view.focus() == OverviewPane::Relations,
                maximized: view.maximized() == Some(OverviewPane::Relations),
                scroll: view.relations_scroll(),
            },
        );
        view.set_relations_scroll(scroll);
    }
    let notice = state.copy_feedback().notice_at(now).map(|notice| {
        (
            notice.message(),
            if matches!(notice, CopyNotice::Failed) {
                theme::error_style()
            } else {
                theme::accent_style()
            },
        )
    });
    footer::render(
        frame,
        layout.shell.footer(),
        layout.shell.footer_lines(),
        notice,
    );
    frame.render_widget(
        separator::render(layout.shell.footer_separator().width),
        layout.shell.footer_separator(),
    );
    render_overlay(frame, area, state.review(), view);
}

fn render_terminal_size_notice(frame: &mut Frame<'_>, area: Rect, quit_confirmation: bool) {
    terminal_notice::render_wrapped(
        frame,
        area,
        if quit_confirmation {
            "Terminal too small. Resize or press Enter/Esc to decide."
        } else {
            "Terminal too small. Resize or press q to quit."
        },
    );
}

fn status_line(state: &ReviewSessionState, view: &OverviewViewState) -> Line<'static> {
    let counts = state.review().summary();
    let mut spans = vec![
        environments::ready_status_marker(),
        Span::styled("Ready", theme::overview_text_style()),
    ];
    append_count(
        &mut spans,
        counts.creates,
        "+",
        "add",
        theme::overview_total_add_style(),
    );
    append_count(
        &mut spans,
        counts.updates,
        "~",
        "update",
        theme::overview_total_update_style(),
    );
    append_count(
        &mut spans,
        counts.replaces,
        "",
        "replace",
        theme::overview_total_replace_style(),
    );
    append_count(
        &mut spans,
        counts.deletes,
        "-",
        "destroy",
        theme::overview_total_destroy_style(),
    );
    spans.push(Span::styled(
        format!("  Repeated: {}", state.prepared_overview().repeated()),
        theme::overview_muted_style(),
    ));
    if view.searching() {
        spans.extend([
            Span::styled("  Filter: /", theme::overview_muted_style()),
            Span::styled(
                view.search_query().unwrap_or_default().to_owned(),
                theme::overview_text_style(),
            ),
        ]);
    } else if !view.filter().is_empty() {
        spans.extend([
            Span::styled("  Filter: ", theme::overview_muted_style()),
            Span::styled(view.filter().to_owned(), theme::overview_text_style()),
            Span::styled(" (display only)", theme::overview_muted_style()),
        ]);
    }
    Line::from(spans)
}

fn append_count(
    spans: &mut Vec<Span<'static>>,
    count: usize,
    symbol: &str,
    label: &str,
    style: Style,
) {
    if count > 0 {
        spans.push(Span::styled(format!("  {symbol}{count} {label}"), style));
    }
}

fn pane_areas(body: Rect, maximized: Option<OverviewPane>) -> (Rect, Rect) {
    match maximized {
        Some(OverviewPane::Changes) => (body, Rect::default()),
        Some(OverviewPane::Relations) => (Rect::default(), body),
        None => {
            let changes_height = body.height.saturating_mul(4) / 10;
            (
                Rect::new(body.x, body.y, body.width, changes_height),
                Rect::new(
                    body.x,
                    body.y.saturating_add(changes_height),
                    body.width,
                    body.height.saturating_sub(changes_height),
                ),
            )
        }
    }
}

fn render_changes_panel(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: &[Line<'static>],
    view: &OverviewViewState,
    max_vertical: u16,
    review: &PlanReview,
) {
    let focused = view.focus() == OverviewPane::Changes;
    let title = changes_pane_title(review);
    let (pane_name, environment) = title
        .split_once(" · ")
        .expect("the changes pane title has an environment name");
    let title = Line::from(vec![
        Span::styled(
            if focused { "* " } else { "  " },
            theme::relation_frame_style(focused),
        ),
        Span::styled(pane_name.to_owned(), theme::overview_pane_title_style()),
        Span::styled(
            format!(" · {environment}"),
            theme::overview_header_muted_style(),
        ),
    ]);
    let block = Block::bordered()
        .title(title)
        .border_style(theme::relation_frame_style(focused))
        .style(theme::overview_text_style());
    let body = block.inner(area);
    frame.render_widget(block, area);
    if body.width == 0 || body.height == 0 {
        return;
    }
    let vertical = view.scroll().min(max_vertical);
    let max_horizontal = lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or_default()
        .saturating_sub(usize::from(body.width));
    view.set_max_changes_horizontal(u16::try_from(max_horizontal).unwrap_or(u16::MAX));
    frame.render_widget(
        Paragraph::new(lines.to_owned())
            .style(theme::overview_text_style())
            .scroll((vertical, view.changes_horizontal())),
        body,
    );
    if lines.len() > usize::from(body.height) {
        scrollbar::render_vertical(
            frame,
            body,
            lines.len(),
            usize::from(body.height),
            usize::from(vertical),
        );
    }
}

fn changes_pane_title(review: &PlanReview) -> String {
    let environment = match review.context().display_name() {
        ExecutionContextValue::Known(name) => name.clone(),
        ExecutionContextValue::Loading => context::target(review.root()),
    };
    format!("[2] Changes · {environment}")
}

fn overview_lines(
    content: &OverviewContent,
    review: &PlanReview,
    view: &OverviewViewState,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(vec![
        Span::styled("  Change ", theme::overview_text_style()),
        Span::styled("Address", theme::overview_text_style()),
    ])];
    if content.unsupported > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "Other changes: {} output/import/move or unsupported change(s). Press v for the full plan.",
                content.unsupported
            ),
            theme::overview_warning_style(),
        )));
    }
    if content.drift > 0 {
        lines.push(Line::from(Span::styled(
            format!("Drift detected in {} resource(s).", content.drift),
            theme::overview_muted_style(),
        )));
    }
    if content.rows.is_empty() {
        lines.push(Line::from(Span::styled(
            if review.has_changes() {
                "No matching resource changes. Press v for the full plan."
            } else {
                "No resource changes to summarize. Press v for the full plan."
            },
            theme::overview_muted_style(),
        )));
        return lines;
    }
    for (index, row) in content.rows.iter().enumerate() {
        let selected = view.selected() == Some(index);
        let marker = if selected { ">" } else { " " };
        let indent = if row.child { "  " } else { "" };
        let expansion = if row.member_index.is_none() && row.count > 1 {
            if view.expanded().contains(&row.group_index) {
                "▾"
            } else {
                "▸"
            }
        } else {
            " "
        };
        let action = if row.count > 1 && row.member_index.is_none() {
            format!("{} x{}", row.action, row.count)
        } else {
            row.action.clone()
        };
        let label_prefix = format!("{indent}{expansion} ");
        let address_style = if selected {
            theme::overview_header_selected_style()
        } else {
            theme::overview_text_style()
        };
        let mut spans = vec![
            Span::styled(format!("{marker} "), theme::overview_text_style()),
            Span::styled(format!("{action:<6} "), action_style(&row.action)),
            Span::styled(label_prefix, theme::overview_text_style()),
            Span::styled(row.display_address.clone(), address_style),
        ];
        if row.has_unknown {
            spans.push(Span::styled(
                " [unknown values]",
                theme::overview_text_style(),
            ));
        }
        lines.push(Line::from(spans));
    }
    lines
}

fn action_style(action: &str) -> Style {
    if matches!(action, "+/-" | "-/+") {
        theme::overview_total_replace_style()
    } else if action.starts_with('+') {
        theme::overview_total_add_style()
    } else if action.starts_with('-') {
        theme::overview_total_destroy_style()
    } else if action.starts_with('~') {
        theme::overview_total_update_style()
    } else {
        theme::overview_muted_style()
    }
}

fn copy_flash_lines(lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| Line::from(Span::styled(line.to_string(), theme::copy_flash_style())))
        .collect()
}

fn footer_items(view: &OverviewViewState, content: &OverviewContent) -> Vec<(u8, Line<'static>)> {
    if view.searching() {
        vec![
            (100, footer::overview_hint(&["Enter"], "confirm")),
            (90, footer::overview_hint(&["Esc"], "cancel")),
        ]
    } else {
        let mut items = match view.focus() {
            OverviewPane::Changes => vec![
                (75, footer::overview_hint(&["/"], "filter")),
                (100, footer::overview_hint(&["Enter"], "open raw")),
            ],
            OverviewPane::Relations => vec![(100, footer::overview_hint(&["Enter"], "open raw"))],
        };
        if view.focus() == OverviewPane::Changes
            && let Some(expanded) = view.selected_group_expanded(content)
        {
            items.push((
                90,
                footer::overview_hint(&["Space"], if expanded { "collapse" } else { "expand" }),
            ));
        }
        items.extend([
            (70, footer::overview_hint(&["v"], "full plan")),
            if view.maximized().is_some() {
                (50, footer::overview_hint(&["f", "Esc"], "restore"))
            } else {
                (50, footer::overview_hint(&["f"], "maximize"))
            },
            (110, footer::overview_hint(&["?"], "help")),
            (120, footer::overview_hint(&["q"], "quit")),
        ]);
        if !view.filter().is_empty() && view.maximized().is_none() {
            items.insert(0, (80, footer::overview_hint(&["Esc"], "clear filter")));
        }
        items
    }
}

fn required_footer_items(view: &OverviewViewState) -> Vec<(u8, Line<'static>)> {
    if view.searching() {
        vec![
            (100, footer::overview_hint(&["Enter"], "confirm")),
            (90, footer::overview_hint(&["Esc"], "cancel")),
        ]
    } else {
        vec![
            (100, footer::overview_hint(&["Enter"], "open raw")),
            if view.maximized().is_some() {
                (50, footer::overview_hint(&["f", "Esc"], "restore"))
            } else {
                (50, footer::overview_hint(&["f"], "maximize"))
            },
            (110, footer::overview_hint(&["?"], "help")),
            (120, footer::overview_hint(&["q"], "quit")),
        ]
    }
}

fn render_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    review: &PlanReview,
    view: &OverviewViewState,
) {
    let Some(overlay) = view.overlay() else {
        return;
    };
    match overlay {
        OverviewOverlay::Help => help_dialog::render(
            frame,
            area,
            "Help",
            &[
                help_dialog::HelpSection::new(
                    "Navigation",
                    vec![
                        help_dialog::HelpAction::new(
                            "2 / 3",
                            format!(
                                "focus {} / Relations",
                                changes_pane_title(review).trim_start_matches("[2] ")
                            ),
                        ),
                        help_dialog::HelpAction::new(
                            "↑ / ↓ / j / k",
                            "select a Changes row or scroll Relations",
                        ),
                        help_dialog::HelpAction::new(
                            "← / → / h / l",
                            "scroll Changes or Relations horizontally",
                        ),
                        help_dialog::HelpAction::new("PgUp / PgDn", "move one page"),
                        help_dialog::HelpAction::new(
                            "Home / End / g / G",
                            "go to the top or bottom",
                        ),
                        help_dialog::HelpAction::new(
                            "f",
                            format!(
                                "maximize or restore {} / [3] Relations",
                                changes_pane_title(review)
                            ),
                        ),
                    ],
                ),
                help_dialog::HelpSection::new(
                    "Review",
                    vec![
                        help_dialog::HelpAction::new(
                            "✓ Ready",
                            "plan acquired; not a judgment of apply safety",
                        ),
                        help_dialog::HelpAction::new("Enter", "open the selected raw block"),
                        help_dialog::HelpAction::new("/", "filter Changes full addresses"),
                        help_dialog::HelpAction::new(
                            "Space",
                            "expand or collapse only on ▸/▾ group rows",
                        ),
                        help_dialog::HelpAction::new(
                            "[unknown values]",
                            "known changes match; unknown values may still differ",
                        ),
                        help_dialog::HelpAction::new("v", "show the full plan from the top"),
                    ],
                ),
                help_dialog::HelpSection::new(
                    "Actions",
                    vec![
                        help_dialog::HelpAction::new("y", "copy the full plan"),
                        help_dialog::HelpAction::new("c", "show execution context"),
                    ],
                ),
                relations::help_section(),
                help_dialog::HelpSection::new(
                    "Exit",
                    vec![help_dialog::HelpAction::new("q", "quit")],
                ),
            ],
            view.overlay_scroll(),
        ),
        OverviewOverlay::Context => {
            context_dialog::render(frame, area, review.context(), view.overlay_scroll());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeMap, BTreeSet},
        path::PathBuf,
    };

    use super::*;
    use crate::ui::features::overview::OverviewInput;
    use crate::{
        app::{
            execution::{ExecutionContext, VariableSources},
            plan::{
                AttributeType, ConfigurationRelationStatus, Plan, PlanAction, PlanRelations,
                PlanValue, ProviderSchema, ProviderSchemas, RelationEndpoint, RelationEvidence,
                RelationGraph, RelationGraphGroup, RelationGraphLink, RelationGraphLinkKind,
                RelationNode, RelationNodeId, RelationSource, RelationUnresolvedReason,
                ResourceChange, ResourceChangeKind, ResourceMode, ResourceSchema,
                StateRelationStatus, test_support::output_change,
            },
            review::{PlanBlock, PlanBlockKind, PlanDocument, PlanMetadata},
            session::test_support::overview_session,
        },
        ui::test_support::{
            assert_dialog_scrolled_up, buffer_text, dialog_body_rows, render_to_buffer,
        },
    };
    use ratatui::style::Color;

    fn render(
        frame: &mut Frame<'_>,
        state: &ReviewSessionState,
        view: &OverviewViewState,
        now: Instant,
    ) {
        render_with_quit_confirmation(frame, state, view, now, false);
    }

    fn review() -> PlanReview {
        review_with_plan(server_plan())
    }

    fn review_with_plan(plan: Plan) -> PlanReview {
        let addresses = vec![
            "terraform_data.server[\"one\"]".to_owned(),
            "terraform_data.server[\"two\"]".to_owned(),
        ];
        let document = PlanDocument::with_blocks_and_line_kinds(
            "Terraform will perform actions.\n\nserver blocks\n".to_owned(),
            vec![
                PlanBlock::new(0..2, PlanBlockKind::Common),
                PlanBlock::with_addresses(2..3, PlanBlockKind::Resource, addresses),
            ],
            Vec::new(),
        );
        PlanReview::new(
            PathBuf::from("/repo/infra"),
            "default".to_owned(),
            document,
            plan,
            PlanMetadata::new(true),
            Vec::new(),
        )
    }

    fn server_plan() -> Plan {
        let addresses = vec![
            "terraform_data.server[\"one\"]".to_owned(),
            "terraform_data.server[\"two\"]".to_owned(),
        ];
        let change = |address: String| ResourceChange {
            address,
            provider: None,
            resource_type: Some("terraform_data".to_owned()),
            resource_name: Some("server".to_owned()),
            mode: ResourceMode::Managed,
            actions: vec![PlanAction::Update],
            kind: ResourceChangeKind::Update,
            before: Some(PlanValue::Object(BTreeMap::from([(
                "input".to_owned(),
                PlanValue::String("old".to_owned()),
            )]))),
            after: Some(PlanValue::Object(BTreeMap::from([(
                "input".to_owned(),
                PlanValue::String("new".to_owned()),
            )]))),
            before_sensitive: None,
            after_sensitive: None,
            after_unknown: None,
            replace_paths: None,
            action_reason: None,
            previous_address: None,
            importing: None,
        };
        Plan {
            resource_changes: addresses.into_iter().map(change).collect(),
            output_changes: vec![
                output_change("endpoint", PlanAction::Update),
                output_change("unchanged", PlanAction::NoOp),
            ],
            ..Plan::empty()
        }
    }

    fn unknown_review() -> PlanReview {
        let provider = "registry.example/provider".to_owned();
        let mut plan = server_plan();
        for change in &mut plan.resource_changes {
            change.provider = Some(provider.clone());
            change.before = Some(PlanValue::Object(BTreeMap::from([(
                "input".to_owned(),
                PlanValue::String("old".to_owned()),
            )])));
            change.after = Some(PlanValue::Object(BTreeMap::from([
                ("input".to_owned(), PlanValue::String("new".to_owned())),
                ("output".to_owned(), PlanValue::Null),
            ])));
            change.after_unknown = Some(PlanValue::Object(BTreeMap::from([(
                "output".to_owned(),
                PlanValue::Bool(true),
            )])));
        }
        review_with_plan(plan).with_provider_schemas(Some(ProviderSchemas {
            providers: BTreeMap::from([(
                provider,
                ProviderSchema {
                    resources: BTreeMap::from([(
                        "terraform_data".to_owned(),
                        ResourceSchema {
                            attributes: BTreeMap::from([
                                ("input".to_owned(), AttributeType::String),
                                ("output".to_owned(), AttributeType::String),
                            ]),
                            block_types: BTreeMap::new(),
                        },
                    )]),
                },
            )]),
        }))
    }

    fn related_review() -> PlanReview {
        let mut plan = server_plan();
        let mut network = plan.resource_changes[0].clone();
        network.address = "terraform_data.network".to_owned();
        network.resource_name = Some("network".to_owned());
        plan.resource_changes.push(network);
        let relations = PlanRelations::from_saved_plan(
            ConfigurationRelationStatus::Available,
            vec![RelationEvidence::resolved(
                RelationEndpoint::Instance("terraform_data.server[\"one\"]".to_owned()),
                RelationEndpoint::Instance("terraform_data.network".to_owned()),
                RelationSource::Configuration,
            )],
            true,
        )
        .with_state(StateRelationStatus::NoPriorState, Vec::new());

        review_with_plan(plan).with_relations(relations)
    }

    #[test]
    fn overview_ready_marker_and_tool_metadata_use_their_own_styles() {
        let state = overview_session(review());
        let view = OverviewViewState::default();
        let buffer = render_to_buffer((120, 40), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let text = buffer_text(&buffer);
        let status_y = text
            .lines()
            .position(|line| line.contains("✓ Ready"))
            .expect("the Ready status is visible");
        let status_line = text.lines().nth(status_y).unwrap();
        let marker_x = ratatui::text::Line::from(
            &status_line[..status_line.find('✓').expect("status marker")],
        )
        .width();
        assert_eq!(
            buffer
                .cell((
                    u16::try_from(marker_x).unwrap(),
                    u16::try_from(status_y).unwrap()
                ))
                .unwrap()
                .fg,
            Color::Green
        );
        assert_eq!(
            buffer
                .cell((
                    u16::try_from(marker_x + 2).unwrap(),
                    u16::try_from(status_y).unwrap()
                ))
                .unwrap()
                .fg,
            Color::Reset
        );

        let header = text.lines().next().unwrap();
        let tool_x =
            ratatui::text::Line::from(&header[..header.find("terraform").unwrap()]).width();
        assert_eq!(
            buffer.cell((u16::try_from(tool_x).unwrap(), 0)).unwrap().fg,
            Color::Reset
        );
    }

    fn mixed_relation_graph() -> RelationGraph {
        let nodes = [
            ("terraform_data.source", BTreeSet::new()),
            ("terraform_data.config", BTreeSet::new()),
            ("terraform_data.candidate", BTreeSet::new()),
            (
                "terraform_data.unknown",
                BTreeSet::from([RelationUnresolvedReason::Variable]),
            ),
        ]
        .into_iter()
        .map(|(address, unresolved)| RelationNode {
            id: RelationNodeId::from_addresses([address.to_owned()]).unwrap(),
            display_address: address.to_owned(),
            operation: ResourceChangeKind::Update,
            change_count: 1,
            breadcrumbs: Vec::new(),
            differs: false,
            has_unknown: false,
            unresolved,
        })
        .collect::<Vec<_>>();
        let link = |from: usize, to: usize, kind, sources: &[RelationSource]| RelationGraphLink {
            from: nodes[from].id.clone(),
            to: nodes[to].id.clone(),
            kind,
            sources: sources.iter().copied().collect(),
        };
        let links = vec![
            link(
                0,
                1,
                RelationGraphLinkKind::Solid,
                &[RelationSource::Configuration],
            ),
            link(
                1,
                2,
                RelationGraphLinkKind::Dotted,
                &[RelationSource::Configuration],
            ),
            link(2, 3, RelationGraphLinkKind::Solid, &[RelationSource::State]),
        ];
        RelationGraph {
            connected_groups: vec![RelationGraphGroup {
                nodes: nodes.iter().map(|node| node.id.clone()).collect(),
                contains_destructive_change: false,
            }],
            nodes,
            links,
            links_unknown: Vec::new(),
            no_links_shown: Vec::new(),
        }
    }

    fn scroll_filtered_row_into_view(
        state: &ReviewSessionState,
        view: &mut OverviewViewState,
        content: &OverviewContent,
    ) {
        let layout = layout(Rect::new(0, 0, 40, 16), state, view, content);
        view.apply(
            OverviewInput::Down,
            layout.changes_body(),
            layout.relations(),
            layout.max_vertical(),
            content,
        );
        for _ in 0..5 {
            view.apply(
                OverviewInput::Right,
                layout.changes_body(),
                layout.relations(),
                layout.max_vertical(),
                content,
            );
        }
    }

    #[test]
    fn quit_confirmation_replaces_overview_actions_at_supported_sizes() {
        let state = overview_session(review());
        let view = OverviewViewState::default();
        let content = OverviewContent::project(&state, view.filter(), view.expanded());

        for (width, height) in [(40, 16), (80, 24), (120, 40)] {
            let area = Rect::new(0, 0, width, height);
            let normal_layout = layout(area, &state, &view, &content);
            let confirmation_layout = prepare(area, &state, &view, &content, true).layout;
            let buffer = render_to_buffer((width, height), |frame| {
                render_with_quit_confirmation(frame, &state, &view, Instant::now(), true);
            });
            let text = buffer_text(&buffer);

            assert_eq!(
                confirmation_layout.changes, normal_layout.changes,
                "{width}x{height}"
            );
            assert_eq!(
                confirmation_layout.relations, normal_layout.relations,
                "{width}x{height}"
            );
            assert!(text.contains("[Enter]"), "{width}x{height}: {text}");
            assert!(text.contains("[Esc]"), "{width}x{height}: {text}");
            assert!(text.contains("Quit"), "{width}x{height}: {text}");
            assert!(!text.contains("q quit"), "{width}x{height}: {text}");
            assert!(!text.contains("Enter open raw"), "{width}x{height}: {text}");
        }
    }

    #[test]
    fn narrow_mixed_relations_keep_legend_entries_complete() {
        let state = overview_session(related_review());
        let view = OverviewViewState::default();
        let content = OverviewContent::project(&state, view.filter(), view.expanded());
        let graph = mixed_relation_graph();
        let relations_area = layout(Rect::new(0, 0, 40, 16), &state, &view, &content).relations();
        let output = render_to_buffer((40, 16), |frame| {
            relations::render(
                frame,
                relations_area,
                &graph,
                &RelationGraphView {
                    title: RelationGraphTitle {
                        environment: None,
                        scope: "whole env",
                    },
                    selected_node: None,
                    focused: true,
                    maximized: false,
                    scroll: relations::RelationGraphScroll::default(),
                },
            );
        });
        let text = buffer_text(&output);

        assert!(
            text.contains("A→B uses A; block-level may not apply"),
            "{text}"
        );
        assert!(text.contains("(state) from state"), "{text}");
        assert!(
            text.contains("? unresolved: relationship unknown"),
            "{text}"
        );
    }

    #[test]
    fn renders_grouped_overview_with_fixed_counts_and_unsupported_notice() {
        let state = overview_session(review());
        let view = OverviewViewState::default();
        let buffer = render_to_buffer((100, 24), |frame| {
            render(frame, &state, &view, Instant::now());
        });

        let text = buffer_text(&buffer);
        assert!(!text.contains("Space expand"));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn empty_changes_notices_cover_outputs_and_drift() {
        struct EmptyCase {
            name: &'static str,
            plan: Plan,
            applyable: bool,
            expected: &'static [&'static str],
        }

        let outputs = |action| Plan {
            output_changes: vec![
                output_change("endpoint", action),
                output_change("unchanged", PlanAction::NoOp),
            ],
            ..Plan::empty()
        };
        let drift = Plan {
            drifted_resources: vec!["terraform_data.drifted".to_owned()],
            ..Plan::empty()
        };
        for case in [
            EmptyCase {
                name: "no_op_outputs",
                plan: outputs(PlanAction::NoOp),
                applyable: false,
                expected: &["No resource changes to summarize."],
            },
            EmptyCase {
                name: "changed_output",
                plan: outputs(PlanAction::Delete),
                applyable: true,
                expected: &[
                    "Other changes: 1 output/import/move or unsupported change(s).",
                    "No matching resource changes.",
                ],
            },
            EmptyCase {
                name: "normal_drift_only",
                plan: drift.clone(),
                applyable: false,
                expected: &[
                    "Drift detected in 1 resource(s).",
                    "No resource changes to summarize.",
                ],
            },
            EmptyCase {
                name: "refresh_only_drift_only",
                plan: drift,
                applyable: true,
                expected: &[
                    "Other changes: 1 output/import/move or unsupported change(s).",
                    "No matching resource changes.",
                ],
            },
        ] {
            let review = PlanReview::new(
                PathBuf::from("/repo/infra"),
                "default".to_owned(),
                PlanDocument::with_blocks_and_line_kinds(String::new(), Vec::new(), Vec::new()),
                case.plan,
                PlanMetadata::new(case.applyable),
                Vec::new(),
            );
            let state = overview_session(review.clone());
            let content = OverviewContent::project(&state, "", &BTreeSet::new());

            let text = overview_lines(&content, &review, &OverviewViewState::default())
                .into_iter()
                .skip(1)
                .map(|line| line.to_string())
                .collect::<Vec<_>>();

            assert_eq!(
                text.len(),
                case.expected.len(),
                "case {}: {text:?}",
                case.name
            );
            for (line, expected) in text.iter().zip(case.expected) {
                assert!(line.starts_with(expected), "case {}: {text:?}", case.name);
            }
        }
    }

    #[test]
    fn unknown_group_note_reaches_changes_and_relation_nodes() {
        let state = overview_session(unknown_review());
        let view = OverviewViewState::default();
        let buffer = render_to_buffer((120, 40), |frame| {
            render(frame, &state, &view, Instant::now());
        });

        let text = buffer_text(&buffer);
        assert!(text.contains("[unknown values]"), "{text}");
        assert_eq!(text.matches("[unknown values]").count(), 2, "{text}");
    }

    #[test]
    fn selected_group_highlights_its_complete_relation_node() {
        let state = overview_session(related_review());
        let mut view = OverviewViewState::default();
        let content = OverviewContent::project(&state, "", view.expanded());
        view.apply(
            OverviewInput::Down,
            Rect::default(),
            Rect::default(),
            0,
            &content,
        );

        assert_eq!(
            content.rows[0]
                .node_id
                .as_ref()
                .expect("mapped relation node")
                .addresses()
                .len(),
            2
        );
        assert_eq!(state.prepared_overview().relations().links.len(), 1);
        let buffer = render_to_buffer((120, 40), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let text = buffer_text(&buffer);
        assert!(text.contains("terraform_data.network"));
        assert!(text.contains("terraform_data.server[*]"));
        assert!(text.contains("──>"));
        assert!(text.contains("> ~ terraform_data.server[*]"));
    }

    #[test]
    fn split_and_maximized_layouts_keep_both_panes_available_at_target_sizes() {
        let state = overview_session(related_review());
        let content = OverviewContent::project(&state, "", &BTreeSet::new());
        let mut view = OverviewViewState::default();

        for (width, height) in [(40, 16), (80, 24), (120, 40), (165, 50)] {
            let buffer = render_to_buffer((width, height), |frame| {
                render(frame, &state, &view, Instant::now());
            });
            let text = buffer_text(&buffer);
            assert!(text.contains("[2] Changes"), "{width}x{height}: {text}");
            assert!(text.contains("[3] Relations"), "{width}x{height}: {text}");
            let split = layout(Rect::new(0, 0, width, height), &state, &view, &content);
            let body_height = split.changes.height + split.relations.height;
            assert_eq!(
                split.changes.height,
                body_height * 4 / 10,
                "{width}x{height}"
            );
        }

        let area = Rect::new(0, 0, 120, 40);
        let split = layout(area, &state, &view, &content);

        view.apply(
            OverviewInput::FocusRelations,
            Rect::default(),
            Rect::default(),
            0,
            &content,
        );
        view.apply(
            OverviewInput::ToggleMaximize,
            Rect::default(),
            Rect::default(),
            0,
            &content,
        );
        let maximized = layout(area, &state, &view, &content);
        assert_eq!(maximized.changes, Rect::default());
        assert_eq!(maximized.relations.width, area.width);
        assert!(maximized.relations.height > split.relations.height);
    }

    #[test]
    fn changes_pane_scrolls_full_addresses_horizontally() {
        let address = format!("terraform_data.{}tail-marker", "long_segment_".repeat(7));
        let state = overview_session(review());
        let mut view = OverviewViewState::default();
        let mut content = OverviewContent::project(&state, "", view.expanded());
        content.rows[0].display_address = address.clone();
        let lines = overview_lines(&content, state.review(), &view);
        assert!(lines.iter().any(|line| line.to_string().contains(&address)));

        let initial = render_to_buffer((32, 5), |frame| {
            render_changes_panel(frame, frame.area(), &lines, &view, 0, state.review());
        });
        assert!(!buffer_text(&initial).contains("tail-marker"));

        for _ in 0..100 {
            view.apply(
                OverviewInput::Right,
                Rect::default(),
                Rect::default(),
                0,
                &content,
            );
        }
        let scrolled = render_to_buffer((32, 5), |frame| {
            render_changes_panel(frame, frame.area(), &lines, &view, 0, state.review());
        });
        assert!(buffer_text(&scrolled).contains("tail-marker"));
        let max_horizontal = view.changes_horizontal();
        for _ in 0..100 {
            view.apply(
                OverviewInput::Right,
                Rect::default(),
                Rect::default(),
                0,
                &content,
            );
        }
        assert_eq!(view.changes_horizontal(), max_horizontal);

        let mut short_content = content;
        short_content.rows.truncate(1);
        short_content.rows[0].display_address = "terraform_data.short".to_owned();
        short_content.unsupported = 0;
        assert_eq!(short_content.rows.len(), 1);
        let short_lines = overview_lines(&short_content, state.review(), &view);
        for _ in 0..100 {
            view.apply(
                OverviewInput::Right,
                Rect::default(),
                Rect::default(),
                0,
                &short_content,
            );
        }
        let short = render_to_buffer((40, 5), |frame| {
            render_changes_panel(frame, frame.area(), &short_lines, &view, 0, state.review());
        });
        assert!(
            buffer_text(&short).contains("terraform_data.short"),
            "{}",
            buffer_text(&short)
        );
        assert_eq!(view.changes_horizontal(), 0);
        view.apply(
            OverviewInput::Right,
            Rect::default(),
            Rect::default(),
            0,
            &short_content,
        );
        assert_eq!(view.changes_horizontal(), 0);
    }

    #[test]
    fn relations_footer_omits_basic_arrow_navigation() {
        let content = OverviewContent::project(&overview_session(review()), "", &BTreeSet::new());
        let mut view = OverviewViewState::default();
        view.apply(
            OverviewInput::FocusRelations,
            Rect::default(),
            Rect::default(),
            0,
            &content,
        );

        let footer_text = footer_items(&view, &content)
            .into_iter()
            .map(|(_, line)| line.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(footer_text.contains("Enter open raw"));
        assert!(!footer_text.contains('↑'));
        assert!(!footer_text.contains('↓'));
        assert!(!footer_text.contains('←'));
        assert!(!footer_text.contains('→'));
    }

    #[test]
    fn selected_group_footer_tracks_expansion_and_filtered_members() {
        let state = overview_session(review());
        let mut view = OverviewViewState::default();
        let changes_body = Rect::new(0, 0, 80, 4);
        let relations_body = Rect::new(0, 0, 80, 8);
        let content = OverviewContent::project(&state, "", view.expanded());

        view.apply(
            OverviewInput::Down,
            changes_body,
            relations_body,
            0,
            &content,
        );
        let collapsed = render_to_buffer((100, 24), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let collapsed_text = buffer_text(&collapsed);
        assert!(collapsed_text.contains("▸ terraform_data.server[*]"));
        assert!(collapsed_text.contains("Space expand"));
        let narrow_collapsed = render_to_buffer((40, 16), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        assert!(buffer_text(&narrow_collapsed).contains("Space expand"));

        view.apply(
            OverviewInput::ToggleExpand,
            changes_body,
            relations_body,
            0,
            &content,
        );
        let expanded_content = OverviewContent::project(&state, "", view.expanded());
        let expanded = render_to_buffer((100, 24), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let expanded_text = buffer_text(&expanded);
        assert!(expanded_text.contains("▾ terraform_data.server[*]"));
        assert!(expanded_text.contains("terraform_data.server[\"one\"]"));
        assert!(expanded_text.contains("Space collapse"));
        let narrow_expanded = render_to_buffer((40, 16), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        assert!(buffer_text(&narrow_expanded).contains("Space collapse"));

        view.apply(
            OverviewInput::Down,
            changes_body,
            relations_body,
            0,
            &expanded_content,
        );
        assert_eq!(view.selected_group_expanded(&expanded_content), None);
        let child = render_to_buffer((100, 24), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let child_text = buffer_text(&child);
        assert!(!child_text.contains("Space expand"));
        assert!(!child_text.contains("Space collapse"));
        let expanded_groups = view.expanded().clone();
        view.apply(
            OverviewInput::ToggleExpand,
            changes_body,
            relations_body,
            0,
            &expanded_content,
        );
        assert_eq!(view.expanded(), &expanded_groups);

        view.apply(
            OverviewInput::SearchStart,
            changes_body,
            relations_body,
            0,
            &expanded_content,
        );
        for character in "one".chars() {
            view.apply(
                OverviewInput::SearchChar(character),
                changes_body,
                relations_body,
                0,
                &expanded_content,
            );
        }
        view.apply(
            OverviewInput::SearchConfirm,
            changes_body,
            relations_body,
            0,
            &expanded_content,
        );
        let filtered_content = OverviewContent::project(&state, view.filter(), view.expanded());
        assert_eq!(filtered_content.rows.len(), 1);
        assert_eq!(view.selected_group_expanded(&filtered_content), None);
        scroll_filtered_row_into_view(&state, &mut view, &filtered_content);
        let filtered = render_to_buffer((50, 16), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let filtered_text = buffer_text(&filtered);
        assert!(filtered_text.contains("server[\"one\"]"), "{filtered_text}");
        assert!(filtered_text.contains("[3] Relations"));
        assert!(filtered_text.contains("terraform_data.server[*]"));
        assert!(!filtered_text.contains("Space expand"));
    }

    #[test]
    fn renders_help_as_a_grouped_modal_that_scrolls_on_small_terminals() {
        let state = overview_session(review());
        let mut view = OverviewViewState::default();
        let content = OverviewContent::project(&state, "", view.expanded());
        view.apply(
            OverviewInput::OpenHelp,
            Rect::new(0, 0, 80, 24),
            Rect::default(),
            0,
            &content,
        );

        for (width, height) in [(40, 16), (80, 24), (120, 40)] {
            let buffer = render_to_buffer((width, height), |frame| {
                render(frame, &state, &view, Instant::now());
            });
            let text = buffer_text(&buffer);
            assert!(text.contains("Help"), "{width}x{height}: {text}");
            assert!(text.contains("Navigation"), "{width}x{height}: {text}");
            if width >= 120 {
                assert!(
                    text.contains("open the selected raw block"),
                    "{width}x{height}: {text}"
                );
                assert!(
                    text.contains("expand or collapse only on ▸/▾ group rows"),
                    "{width}x{height}: {text}"
                );
            }
            assert_eq!(text.matches("close").count(), 1, "{width}x{height}: {text}");
            if (width, height) == (80, 24) {
                assert!(
                    buffer
                        .cell((0, 0))
                        .expect("dimmed background")
                        .modifier
                        .contains(ratatui::style::Modifier::DIM)
                );
            }
            insta::assert_snapshot!(format!("overview_help_{width}x{height}"), text);
        }

        view.overlay_bottom();
        let bottom = render_to_buffer((40, 16), |frame| {
            render(frame, &state, &view, Instant::now());
        });
        let bottom_text = buffer_text(&bottom);
        assert!(bottom_text.contains("Exit"));
        assert!(bottom_text.contains("quit"));
        assert_eq!(bottom_text.matches("close").count(), 1);
        insta::assert_snapshot!("overview_help_40x16_bottom", bottom_text);
    }

    fn long_context_state() -> ReviewSessionState {
        overview_session(review().with_context(
            ExecutionContext::loading("/repo/environments/production").with_variable_sources(
                VariableSources::new(
                    Vec::new(),
                    Vec::new(),
                    false,
                    (0..32).map(|index| format!("TF_VAR_{index:02}")).collect(),
                ),
            ),
        ))
    }

    fn context_view(state: &ReviewSessionState) -> OverviewViewState {
        let mut view = OverviewViewState::default();
        let content = OverviewContent::project(state, "", view.expanded());
        view.apply(
            OverviewInput::OpenContext,
            Rect::new(0, 0, 80, 24),
            Rect::default(),
            0,
            &content,
        );
        view
    }

    fn last_context_body_line(
        state: &ReviewSessionState,
        view: &OverviewViewState,
        (width, height): (u16, u16),
    ) -> (String, String) {
        let text = buffer_text(&render_to_buffer((width, height), |frame| {
            render(frame, state, view, Instant::now());
        }));
        let rows = text.lines().collect::<Vec<_>>();
        let footer = rows
            .iter()
            .position(|row| row.contains("close"))
            .unwrap_or_else(|| panic!("{width}x{height}: context footer\n{text}"));
        (rows[footer - 1].to_owned(), text)
    }

    #[test]
    fn context_dialog_keeps_the_last_line_visible_after_end_paging_and_resize() {
        let long = long_context_state();
        let short = overview_session(review());
        let mut view = context_view(&long);

        let (_, top) = last_context_body_line(&long, &view, (80, 24));
        assert!(top.contains("Execution directory"), "{top}");
        assert!(!top.contains("TF_VAR_31"), "{top}");

        view.overlay_bottom();
        for size in [(80, 24), (40, 16)] {
            let (line, text) = last_context_body_line(&long, &view, size);
            assert!(line.contains("TF_VAR_31"), "{size:?}\n{text}");
        }

        view.overlay_top();
        for _ in 0..10 {
            view.scroll_overlay(8);
        }
        let (line, text) = last_context_body_line(&long, &view, (80, 24));
        assert!(line.contains("TF_VAR_31"), "{text}");

        view.overlay_bottom();
        let (line, text) = last_context_body_line(&short, &view, (80, 24));
        assert!(line.contains("none detected"), "{text}");
        assert!(text.contains("Execution directory"), "{text}");
    }

    #[test]
    fn context_dialog_scrolls_up_from_the_end_by_one_line_and_one_page() {
        for (name, delta, expected) in [("up", -1, "TF_VAR_30"), ("page_up", -8, "TF_VAR_23")] {
            let state = long_context_state();
            let mut view = context_view(&state);
            view.overlay_bottom();
            last_context_body_line(&state, &view, (80, 24));

            view.scroll_overlay(delta);
            let (line, text) = last_context_body_line(&state, &view, (80, 24));

            assert!(line.contains(expected), "case: {name}\n{text}");
        }
    }

    #[test]
    fn help_dialog_scrolls_up_from_the_end_by_one_line_and_one_page() {
        let state = overview_session(review());
        let content = OverviewContent::project(&state, "", &BTreeSet::new());
        let help_rows = |view: &OverviewViewState| {
            dialog_body_rows(
                &render_to_buffer((40, 16), |frame| {
                    render(frame, &state, view, Instant::now());
                }),
                "Help",
            )
        };
        for (name, delta, lines) in [("up", -1, 1), ("page_up", -8, 8)] {
            let mut view = OverviewViewState::default();
            view.apply(
                OverviewInput::OpenHelp,
                Rect::new(0, 0, 40, 16),
                Rect::default(),
                0,
                &content,
            );
            view.overlay_bottom();
            let end = help_rows(&view);

            view.scroll_overlay(delta);
            let scrolled = help_rows(&view);

            assert!(
                end.last().is_some_and(|row| row.contains("quit")),
                "case: {name}"
            );
            assert_dialog_scrolled_up(name, &end, &scrolled, lines);
        }
    }
}
