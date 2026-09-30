use std::path::Path;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};

use crate::app::{
    execution::{ExecutionContext, ExecutionContextValue},
    plan::PlanSummary,
    review::PlanReview,
};
use crate::ui::theme;

use super::changes;
use super::context::{
    display_width, relative_directory, take_from_start, target, tool_label, truncate_middle,
};

const REVIEW_HEADER_SEPARATOR: &str = " ";
const PRODUCTION_SUFFIX: &str = " [PROD]";
const GAP: &str = "  ";

/// Whether the plan header names the tool. A screen that already shows the tool elsewhere hides it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanHeaderTool {
    Shown,
    Hidden,
}

struct HeaderField {
    label: &'static str,
    value: String,
    minimum_value_width: usize,
    kind: HeaderFieldKind,
}

#[derive(Clone, Copy)]
enum HeaderFieldKind {
    Target,
    Workspace,
    Tool,
    Directory,
}

fn render(frame: &mut Frame<'_>, area: Rect, lines: Vec<Line<'static>>) {
    frame.render_widget(Paragraph::new(lines).style(theme::secondary_style()), area);
}

pub(crate) fn render_review(frame: &mut Frame<'_>, area: Rect, review: &PlanReview) {
    render(
        frame,
        area,
        vec![compact_review_header_line(review, area.width, false)],
    );
}

pub(crate) fn render_overview_review(frame: &mut Frame<'_>, area: Rect, review: &PlanReview) {
    render(
        frame,
        area,
        vec![compact_review_header_line(review, area.width, true)],
    );
}

pub(crate) fn render_plan_review(
    frame: &mut Frame<'_>,
    area: Rect,
    review: &PlanReview,
    tool: PlanHeaderTool,
) {
    frame.render_widget(
        Paragraph::new(plan_review_lines(review, area.width, tool))
            .wrap(Wrap { trim: false })
            .style(theme::secondary_style()),
        area,
    );
}

pub(crate) fn plan_review_height(review: &PlanReview, width: u16, tool: PlanHeaderTool) -> u16 {
    u16::try_from(
        Paragraph::new(plan_review_lines(review, width, tool))
            .wrap(Wrap { trim: false })
            .line_count(width.max(1)),
    )
    .unwrap_or(u16::MAX)
}

fn plan_review_lines(review: &PlanReview, width: u16, tool: PlanHeaderTool) -> Vec<Line<'static>> {
    let mut context = plan_review_header_line(review, u16::MAX, tool);
    let changes = plan_review_changes_line(review);
    if context.width() + GAP.len() + changes.width() <= usize::from(width) {
        context.push_span(GAP);
        context.extend(changes.spans);
        vec![context]
    } else {
        vec![plan_review_header_line(review, width, tool), changes]
    }
}

pub(crate) fn render_execution(frame: &mut Frame<'_>, area: Rect, context: &ExecutionContext) {
    let workspace = match context.workspace() {
        ExecutionContextValue::Known(workspace) => Some(workspace.as_str()),
        ExecutionContextValue::Loading => Some("loading..."),
    };
    render(
        frame,
        area,
        vec![header_line(context.cwd_path(), workspace, area.width)],
    );
}

// Target, workspace, tool with version, and directory, as both review headers show them.
fn review_header_values(review: &PlanReview) -> [String; 4] {
    let context = review.context();
    let target_name = match context.display_name() {
        ExecutionContextValue::Known(name) if context.is_production() == Some(true) => {
            format!("{name} [PROD]")
        }
        ExecutionContextValue::Known(name) => name.clone(),
        ExecutionContextValue::Loading => target(review.root()),
    };
    let workspace = match context.workspace() {
        ExecutionContextValue::Known(value) => value.as_str(),
        ExecutionContextValue::Loading => review.workspace(),
    };
    [
        target_name,
        workspace.to_owned(),
        tool_label(context),
        relative_directory(context.cwd_path(), context.launch_root_path()),
    ]
}

fn plan_review_header_line(review: &PlanReview, width: u16, tool: PlanHeaderTool) -> Line<'static> {
    let [target_name, workspace, tool_name, directory] = review_header_values(review);
    let tool_field = HeaderField {
        label: "Tool: ",
        minimum_value_width: display_width(&tool_name),
        value: tool_name,
        kind: HeaderFieldKind::Tool,
    };
    let mut fields = vec![
        HeaderField {
            label: "Target: ",
            minimum_value_width: 8,
            value: target_name,
            kind: HeaderFieldKind::Target,
        },
        HeaderField {
            label: "Workspace: ",
            minimum_value_width: display_width(&workspace).min(7),
            value: workspace,
            kind: HeaderFieldKind::Workspace,
        },
    ];
    if tool == PlanHeaderTool::Shown {
        fields.push(tool_field);
    }
    fields.push(HeaderField {
        label: "Dir: ",
        minimum_value_width: display_width(&directory).min(7),
        value: directory,
        kind: HeaderFieldKind::Directory,
    });
    fit_header(&fields, width)
}

fn plan_review_changes_line(review: &PlanReview) -> Line<'static> {
    let counts = review.summary();
    let mut line = Line::from(Span::styled("Changes", theme::header_label_style()));
    for change in changes::change_counts(counts) {
        if change.count > 0 {
            line.push_span(Span::styled("  ", theme::secondary_style()));
            line.push_span(Span::styled(change.text, change.style));
        }
    }
    if counts == PlanSummary::default() {
        let text = if review.nonstandard_changes() > 0 {
            "  Other changes"
        } else if review.changed_outputs() > 0 {
            "  Outputs changed"
        } else {
            "  No changes"
        };
        line.push_span(Span::styled(text, theme::secondary_style()));
    }
    let drift = review.noted_drift();
    if drift > 0 {
        line.push_span(Span::styled(
            format!("  Drift detected: {drift}"),
            theme::secondary_style(),
        ));
    }
    line
}

fn compact_review_header_line(
    review: &PlanReview,
    width: u16,
    normal_tool_style: bool,
) -> Line<'static> {
    let [target_name, workspace, tool, directory] = review_header_values(review);
    fit_compact_header(
        &[target_name, format!("ws:{workspace}"), tool, directory],
        width,
        normal_tool_style,
    )
}

fn fit_header(fields: &[HeaderField], width: u16) -> Line<'static> {
    let width = usize::from(width);
    let separator_width = Line::from(REVIEW_HEADER_SEPARATOR).width();
    let full_width = fields
        .iter()
        .map(header_field_width)
        .sum::<usize>()
        .saturating_add(separator_width.saturating_mul(fields.len().saturating_sub(1)));
    let allocations = if full_width <= width {
        fields
            .iter()
            .map(|field| (field, header_field_width(field)))
            .collect::<Vec<_>>()
    } else {
        allocate_header_fields(fields, width, separator_width)
    };
    let mut spans = Vec::new();
    for (field, allocation) in allocations {
        let field_spans = format_header_field(field, allocation);
        if field_spans.is_empty() {
            continue;
        }
        if !spans.is_empty() {
            spans.push(Span::styled(
                REVIEW_HEADER_SEPARATOR,
                theme::header_label_style(),
            ));
        }
        spans.extend(field_spans);
    }
    Line::from(spans)
}

fn allocate_header_fields(
    fields: &[HeaderField],
    width: usize,
    separator_width: usize,
) -> Vec<(&HeaderField, usize)> {
    let Some(target) = fields.first() else {
        return Vec::new();
    };
    let mut selected = vec![target];
    for candidate in fields.iter().skip(1) {
        let candidate_minimum = selected
            .iter()
            .map(|field| header_field_minimum_width(field))
            .sum::<usize>()
            .saturating_add(header_field_minimum_width(candidate))
            .saturating_add(separator_width.saturating_mul(selected.len()));
        if candidate_minimum <= width {
            selected.push(candidate);
        }
    }

    let available = width.saturating_sub(separator_width.saturating_mul(selected.len() - 1));
    let mut allocations = selected
        .iter()
        .map(|field| header_field_minimum_width(field).min(width))
        .collect::<Vec<_>>();
    let mut remaining = available.saturating_sub(allocations.iter().sum());
    for (allocation, field) in allocations.iter_mut().zip(&selected) {
        let extra = remaining.min(header_field_width(field).saturating_sub(*allocation));
        *allocation += extra;
        remaining -= extra;
    }
    selected.into_iter().zip(allocations).collect()
}

fn header_field_minimum_width(field: &HeaderField) -> usize {
    display_width(field.label)
        .saturating_add(field.minimum_value_width.min(display_width(&field.value)))
}

fn header_field_width(field: &HeaderField) -> usize {
    display_width(field.label).saturating_add(display_width(&field.value))
}

fn format_header_field(field: &HeaderField, allocation: usize) -> Vec<Span<'static>> {
    let label_width = display_width(field.label);
    if allocation < label_width {
        let label = truncate_middle(field.label.trim_end(), allocation);
        return if label.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(label, theme::header_label_style())]
        };
    }
    let value_width = allocation.saturating_sub(label_width);
    let (value, style) = match field.kind {
        HeaderFieldKind::Target => (
            truncate_target(&field.value, value_width),
            theme::header_target_style(),
        ),
        HeaderFieldKind::Workspace => (
            truncate_middle(&field.value, value_width),
            theme::header_value_style(),
        ),
        HeaderFieldKind::Tool => (
            truncate_tool(&field.value, value_width),
            theme::header_value_style(),
        ),
        HeaderFieldKind::Directory => (
            truncate_directory(&field.value, value_width),
            theme::header_value_style(),
        ),
    };
    vec![
        Span::styled(field.label, theme::header_label_style()),
        Span::styled(value, style),
    ]
}

fn truncate_tool(value: &str, max_width: usize) -> String {
    let Some((name, version)) = value.split_once(' ') else {
        return truncate_middle(value, max_width);
    };
    let name_width = display_width(name);
    if max_width <= name_width {
        return take_from_start(name, max_width);
    }
    let version_width = max_width.saturating_sub(name_width).saturating_sub(1);
    format!("{name} {}", truncate_middle(version, version_width))
}

fn truncate_target(value: &str, max_width: usize) -> String {
    if display_width(value) <= max_width {
        return value.to_owned();
    }
    if let Some(prefix) = value.strip_suffix(PRODUCTION_SUFFIX) {
        let suffix_width = display_width(PRODUCTION_SUFFIX);
        if max_width >= suffix_width {
            return format!(
                "{}{PRODUCTION_SUFFIX}",
                truncate_middle(prefix, max_width - suffix_width)
            );
        }
    }
    truncate_middle(value, max_width)
}

fn truncate_directory(value: &str, max_width: usize) -> String {
    if display_width(value) <= max_width {
        return value.to_owned();
    }
    if let Some(relative) = value.strip_prefix("./") {
        if max_width <= 2 {
            return take_from_start("./", max_width);
        }
        return format!("./{}", truncate_middle(relative, max_width - 2));
    }
    truncate_middle(value, max_width)
}

fn fit_compact_header(parts: &[String], width: u16, normal_tool_style: bool) -> Line<'static> {
    let width = usize::from(width);
    let separator_width = Line::from(GAP).width();
    let full = parts.join(GAP);
    let values = if Line::from(full.as_str()).width() <= width {
        parts.to_vec()
    } else {
        let separators = separator_width.saturating_mul(parts.len().saturating_sub(1));
        let available = width.saturating_sub(separators);
        let minimums = parts
            .iter()
            .enumerate()
            .map(|(index, part)| match index {
                0 => Line::from(part.as_str()).width().min(12),
                index if index == parts.len().saturating_sub(1) => 5,
                1 => Line::from(part.as_str()).width().min(10),
                2 => Line::from(part.as_str()).width().min(15),
                _ => 4,
            })
            .collect::<Vec<_>>();
        if available < minimums.iter().sum() {
            return Line::from(Span::styled(
                truncate_middle(&full, width),
                if normal_tool_style {
                    theme::overview_text_style()
                } else {
                    theme::secondary_style()
                },
            ));
        }
        let mut allocations = minimums;
        let mut remaining = available.saturating_sub(allocations.iter().sum());
        for (allocation, part) in allocations.iter_mut().zip(parts) {
            let extra = remaining.min(
                Line::from(part.as_str())
                    .width()
                    .saturating_sub(*allocation),
            );
            *allocation += extra;
            remaining -= extra;
        }
        parts
            .iter()
            .zip(allocations)
            .enumerate()
            .map(|(index, (part, allocation))| {
                truncate_compact_header_part(
                    part,
                    allocation,
                    index == parts.len().saturating_sub(1),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut spans = Vec::new();
    for (index, value) in values.into_iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(GAP, theme::secondary_style()));
        }
        let style = if normal_tool_style && index == 2 {
            theme::overview_text_style()
        } else {
            theme::secondary_style()
        };
        spans.push(Span::styled(value, style));
    }
    Line::from(spans)
}

fn truncate_compact_header_part(value: &str, max_width: usize, is_directory: bool) -> String {
    if is_directory && value.starts_with("./") && Line::from(value).width() > max_width {
        if max_width <= 2 {
            return truncate_middle(value, max_width);
        }
        return format!("./{}", truncate_middle(&value[2..], max_width - 2));
    }
    truncate_middle(value, max_width)
}

fn header_line(path: &Path, workspace: Option<&str>, width: u16) -> Line<'static> {
    const PREFIX: &str = "Terraleph | ";
    const GAP: usize = 2;
    let width = usize::from(width);
    let right = workspace
        .filter(|workspace| *workspace != "default")
        .map(|workspace| format!("workspace: {workspace}"));
    let right_width = right
        .as_deref()
        .map_or(0, |value| Line::from(value).width());
    let left_width = width.saturating_sub(right_width + usize::from(right.is_some()) * GAP);
    let prefix_width = Line::from(PREFIX).width();
    let left = if left_width <= prefix_width {
        truncate_middle(PREFIX, left_width)
    } else {
        format!(
            "{PREFIX}{}",
            truncate_middle(&target(path), left_width - prefix_width)
        )
    };
    let value = right.map_or_else(
        || left.clone(),
        |right| {
            let gap = width.saturating_sub(Line::from(left.as_str()).width() + right_width);
            format!("{left}{}{right}", " ".repeat(gap))
        },
    );
    Line::from(Span::styled(value, theme::secondary_style()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::plan::{
        Plan, PlanAction, ResourceChangeKind,
        test_support::{output_change, resource_change},
    };
    use crate::app::review::{PlanDocument, PlanMetadata};

    #[test]
    fn zero_resource_counts_preserve_output_and_nonstandard_change_status() {
        struct StatusCase {
            name: &'static str,
            plan: Plan,
            expected: &'static str,
        }

        let read = Plan {
            resource_changes: vec![resource_change(
                "data.terraform_data.read",
                ResourceChangeKind::Read,
            )],
            unsupported_change_count: 1,
            ..Plan::empty()
        };
        let outputs = |actions: [PlanAction; 2]| {
            actions
                .into_iter()
                .zip(["endpoint", "secret"])
                .map(|(action, name)| output_change(name, action))
                .collect()
        };
        for case in [
            StatusCase {
                name: "no_op_outputs",
                plan: Plan {
                    output_changes: outputs([PlanAction::NoOp, PlanAction::NoOp]),
                    ..Plan::empty()
                },
                expected: "No changes",
            },
            StatusCase {
                name: "changed_output",
                plan: Plan {
                    output_changes: outputs([PlanAction::Update, PlanAction::NoOp]),
                    ..Plan::empty()
                },
                expected: "Outputs changed",
            },
            StatusCase {
                name: "read_and_changed_output",
                plan: Plan {
                    output_changes: outputs([PlanAction::Create, PlanAction::NoOp]),
                    ..read
                },
                expected: "Other changes",
            },
        ] {
            let review = PlanReview::new(
                "/dev".into(),
                "default".to_owned(),
                PlanDocument::with_blocks_and_line_kinds(String::new(), Vec::new(), Vec::new()),
                case.plan,
                PlanMetadata::new(false),
                Vec::new(),
            );

            let line = plan_review_changes_line(&review).to_string();
            assert!(line.contains(case.expected), "case {}: {line}", case.name);
        }
    }

    #[test]
    fn drift_status_distinguishes_notes_from_changes() {
        struct DriftCase {
            name: &'static str,
            plan: Plan,
            applyable: bool,
            expected: &'static str,
        }

        let drift = |plan| Plan {
            drifted_resources: vec!["terraform_data.drifted".to_owned()],
            ..plan
        };
        for case in [
            DriftCase {
                name: "normal_drift_only",
                plan: drift(Plan::empty()),
                applyable: false,
                expected: "Changes  No changes  Drift detected: 1",
            },
            DriftCase {
                name: "refresh_only_drift_only",
                plan: drift(Plan::empty()),
                applyable: true,
                expected: "Changes  Other changes",
            },
            DriftCase {
                name: "drift_and_resource_update",
                plan: drift(Plan {
                    resource_changes: vec![resource_change(
                        "terraform_data.drifted",
                        ResourceChangeKind::Update,
                    )],
                    ..Plan::empty()
                }),
                applyable: true,
                expected: "Changes  ~1 update  Drift detected: 1",
            },
            DriftCase {
                name: "drift_and_changed_output",
                plan: drift(Plan {
                    output_changes: vec![output_change("endpoint", PlanAction::Update)],
                    ..Plan::empty()
                }),
                applyable: true,
                expected: "Changes  Outputs changed  Drift detected: 1",
            },
        ] {
            let review = PlanReview::new(
                "/dev".into(),
                "default".to_owned(),
                PlanDocument::with_blocks_and_line_kinds(String::new(), Vec::new(), Vec::new()),
                case.plan,
                PlanMetadata::new(case.applyable),
                Vec::new(),
            );

            let line = plan_review_changes_line(&review).to_string();

            assert_eq!(line, case.expected, "case: {}", case.name);
        }
    }

    #[test]
    fn changes_line_colors_only_the_nonzero_counts() {
        let review = PlanReview::new(
            "/dev".into(),
            "default".to_owned(),
            PlanDocument::with_blocks_and_line_kinds(String::new(), Vec::new(), Vec::new()),
            Plan {
                resource_changes: vec![
                    resource_change("terraform_data.new", ResourceChangeKind::Create),
                    resource_change("terraform_data.api", ResourceChangeKind::Update),
                    resource_change("terraform_data.worker", ResourceChangeKind::Replace),
                ],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        );

        let line = plan_review_changes_line(&review);

        assert_eq!(
            line.spans
                .iter()
                .map(|span| (span.content.as_ref(), span.style))
                .collect::<Vec<_>>(),
            [
                ("Changes", theme::header_label_style()),
                ("  ", theme::secondary_style()),
                ("+1 add", theme::success_style()),
                ("  ", theme::secondary_style()),
                ("~1 update", theme::warning_style()),
                ("  ", theme::secondary_style()),
                ("1 replace", theme::overview_total_replace_style()),
            ]
        );
    }

    #[test]
    fn compact_header_keeps_counts_when_narrow_and_combines_them_when_wide() {
        let review = PlanReview::new(
            "/dev".into(),
            "default".to_owned(),
            PlanDocument::with_blocks_and_line_kinds(String::new(), Vec::new(), Vec::new()),
            Plan {
                resource_changes: [
                    (ResourceChangeKind::Create, 111),
                    (ResourceChangeKind::Update, 222),
                    (ResourceChangeKind::Delete, 333),
                    (ResourceChangeKind::Replace, 444),
                ]
                .into_iter()
                .flat_map(|(kind, count)| {
                    (0..count).map(move |index| {
                        resource_change(&format!("terraform_data.{kind:?}_{index}"), kind)
                    })
                })
                .collect(),
                ..Plan::empty()
            },
            PlanMetadata::new(false),
            Vec::new(),
        );
        assert_eq!(plan_review_height(&review, 240, PlanHeaderTool::Shown), 1);
        let height = plan_review_height(&review, 40, PlanHeaderTool::Shown);
        assert!(height > 2);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, height)).unwrap();
        terminal
            .draw(|frame| render_plan_review(frame, frame.area(), &review, PlanHeaderTool::Shown))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        let normalized = rendered.split_whitespace().collect::<Vec<_>>().join(" ");
        for count in ["+111", "~222", "444 replace", "-333 destroy"] {
            assert!(normalized.contains(count), "{rendered}");
        }
    }

    #[test]
    fn narrow_review_header_keeps_target_label_when_context_fields_do_not_fit() {
        let line = fit_header(
            &[
                HeaderField {
                    label: "Target: ",
                    minimum_value_width: 8,
                    value: "production [PROD]".to_owned(),
                    kind: HeaderFieldKind::Target,
                },
                HeaderField {
                    label: "Workspace: ",
                    minimum_value_width: 7,
                    value: "default".to_owned(),
                    kind: HeaderFieldKind::Workspace,
                },
                HeaderField {
                    label: "Tool: ",
                    minimum_value_width: 15,
                    value: "terraform 1.9.0".to_owned(),
                    kind: HeaderFieldKind::Tool,
                },
                HeaderField {
                    label: "Dir: ",
                    minimum_value_width: 7,
                    value: "./environments/production".to_owned(),
                    kind: HeaderFieldKind::Directory,
                },
            ],
            24,
        );
        let value = line.to_string();

        assert!(value.starts_with("Target: "), "{value}");
        assert!(value.contains("PROD"), "{value}");
        assert!(line.width() <= 24, "{value}");
    }

    #[test]
    fn plan_header_names_the_tool_only_when_the_screen_shows_it() {
        let review = PlanReview::new(
            "/dev".into(),
            "default".to_owned(),
            PlanDocument::with_blocks_and_line_kinds(String::new(), Vec::new(), Vec::new()),
            Plan::empty(),
            PlanMetadata::new(false),
            Vec::new(),
        );

        let shown = plan_review_header_line(&review, 200, PlanHeaderTool::Shown).to_string();
        let hidden = plan_review_header_line(&review, 200, PlanHeaderTool::Hidden).to_string();

        assert!(shown.contains("Tool: "), "{shown}");
        assert!(!hidden.contains("Tool: "), "{hidden}");
        assert!(hidden.contains("Workspace: "), "{hidden}");
        assert!(hidden.contains("Dir: "), "{hidden}");
    }

    #[test]
    fn truncated_production_targets_keep_a_distinguishing_suffix() {
        let first = truncate_target("very-long-target-name-alpha [PROD]", 20);
        let second = truncate_target("very-long-target-name-bravo [PROD]", 20);

        assert_ne!(first, second);
        assert!(first.ends_with(PRODUCTION_SUFFIX), "{first}");
        assert!(second.ends_with(PRODUCTION_SUFFIX), "{second}");
    }
}
