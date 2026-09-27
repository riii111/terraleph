use std::collections::{BTreeMap, BTreeSet};

use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Paragraph},
};

use crate::{
    app::plan::{
        RelationGraph, RelationGraphGroup, RelationGraphLink, RelationGraphLinkKind, RelationNode,
        RelationNodeId, RelationSource, RelationUnresolvedReason, ResourceChangeKind,
    },
    ui::{
        primitives::{atoms::scrollbar, molecules::help_dialog},
        theme,
    },
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RelationGraphScroll {
    pub(crate) vertical: u16,
    pub(crate) horizontal: u16,
}

#[derive(Clone, Copy)]
pub(crate) struct RelationGraphTitle<'a> {
    pub(crate) environment: Option<&'a str>,
    pub(crate) scope: &'a str,
}

pub(crate) struct RelationGraphView<'a> {
    pub(crate) title: RelationGraphTitle<'a>,
    pub(crate) selected_node: Option<&'a RelationNodeId>,
    pub(crate) focused: bool,
    pub(crate) maximized: bool,
    pub(crate) scroll: RelationGraphScroll,
}

pub(crate) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    graph: &RelationGraph,
    view: &RelationGraphView<'_>,
) -> RelationGraphScroll {
    let title = title_line(view.title, view.focused);
    let block = Block::bordered()
        .title(title)
        .border_style(theme::relation_frame_style(view.focused))
        .style(theme::relation_text_style());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return RelationGraphScroll {
            vertical: 0,
            horizontal: 0,
        };
    }

    let (legend, legend_height) = visible_legend_lines(
        legend_lines(graph),
        compact_legend_lines(graph),
        inner.width,
        inner.height.saturating_sub(1),
    );
    let lines = graph_lines(graph, view.selected_node, view.maximized, inner.width);
    // The legend position depends only on the content, never on the scroll offset.
    let short_height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(1);
    let content_height =
        if legend_height > 0 && short_height.saturating_add(legend_height) <= inner.height {
            short_height
        } else {
            inner.height.saturating_sub(legend_height)
        };
    let content_area = Rect::new(inner.x, inner.y, inner.width, content_height);
    // An overflowing graph gives its last column to the scrollbar so no text sits under it.
    let text_area = if lines.len() > usize::from(content_height) {
        Rect::new(
            inner.x,
            inner.y,
            inner.width.saturating_sub(1),
            content_height,
        )
    } else {
        content_area
    };
    let legend_area = Rect::new(
        inner.x,
        inner.y.saturating_add(content_height),
        inner.width,
        legend_height,
    );
    let max_vertical = offset_limit(lines.len(), usize::from(content_area.height));
    let max_horizontal = offset_limit(
        lines.iter().map(Line::width).max().unwrap_or_default(),
        usize::from(text_area.width),
    );
    let scroll = RelationGraphScroll {
        vertical: view.scroll.vertical.min(max_vertical),
        horizontal: view.scroll.horizontal.min(max_horizontal),
    };

    let content_length = lines.len();
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::relation_text_style())
            .scroll((scroll.vertical, scroll.horizontal)),
        text_area,
    );
    scrollbar::render_vertical(
        frame,
        content_area,
        content_length,
        usize::from(content_area.height),
        usize::from(scroll.vertical),
    );
    if legend_height > 0 {
        frame.render_widget(
            Paragraph::new(legend)
                .wrap(ratatui::widgets::Wrap { trim: false })
                .style(theme::relation_text_style()),
            legend_area,
        );
    }
    scroll
}

pub(crate) fn title_line(title: RelationGraphTitle<'_>, focused: bool) -> Line<'static> {
    let mut spans = vec![
        Span::styled(
            if focused { "* " } else { "  " },
            if focused {
                theme::relation_frame_style(true)
            } else {
                theme::relation_muted_style()
            },
        ),
        Span::styled(
            "[3] Relations",
            theme::relation_text_style().add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(environment) = title.environment {
        spans.push(Span::styled(
            format!(" · {environment}"),
            theme::relation_text_style(),
        ));
    }
    spans.push(Span::styled(
        format!(" · {}", title.scope),
        theme::relation_muted_style(),
    ));
    Line::from(spans)
}

fn visible_legend_lines(
    lines: Vec<Line<'static>>,
    compact_lines: Vec<Line<'static>>,
    width: u16,
    available_height: u16,
) -> (Vec<Line<'static>>, u16) {
    let (visible, height, complete) = fitting_legend_lines(lines, width, available_height);
    if complete {
        return (visible, height);
    }
    let (compact, compact_height, compact_complete) =
        fitting_legend_lines(compact_lines, width, available_height);
    if compact_complete {
        return (compact, compact_height);
    }
    (visible, height)
}

fn fitting_legend_lines(
    lines: Vec<Line<'static>>,
    width: u16,
    available_height: u16,
) -> (Vec<Line<'static>>, u16, bool) {
    let mut visible = Vec::new();
    let mut height = 0_u16;
    for line in lines {
        let line_height = u16::try_from(
            Paragraph::new(line.clone())
                .wrap(ratatui::widgets::Wrap { trim: false })
                .line_count(width),
        )
        .unwrap_or(u16::MAX);
        if line_height > available_height.saturating_sub(height) {
            return (visible, height, false);
        }
        height = height.saturating_add(line_height);
        visible.push(line);
    }
    (visible, height, true)
}

fn legend_lines(graph: &RelationGraph) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !graph.links.is_empty() {
        lines.push(Line::from(Span::styled(
            "A ──> B  B uses A",
            theme::relation_text_style(),
        )));
        if graph
            .links
            .iter()
            .any(|link| link.kind == RelationGraphLinkKind::Dotted)
        {
            lines.push(Line::from(Span::styled(
                "A ┄┄> B  block-level, may not apply",
                theme::relation_text_style(),
            )));
        }
        if has_grouped_links(graph) {
            lines.push(Line::from(Span::styled(
                "Grouped links may apply to only some members",
                theme::relation_text_style(),
            )));
        }
        if graph
            .links
            .iter()
            .any(|link| link.sources.contains(&RelationSource::State))
        {
            lines.push(Line::from(Span::styled(
                "(state) from state; unmarked from configuration",
                theme::relation_text_style(),
            )));
        }
    }
    if graph.nodes.iter().any(|node| node.differs) {
        lines.push(Line::from(Span::styled(
            "! differs across envs",
            theme::relation_text_style(),
        )));
    }
    if graph.nodes.iter().any(|node| !node.unresolved.is_empty()) {
        lines.push(Line::from(Span::styled(
            "? unresolved means a relationship could not be determined",
            theme::relation_text_style(),
        )));
    }
    lines
}

fn compact_legend_lines(graph: &RelationGraph) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if !graph.links.is_empty() {
        if graph
            .links
            .iter()
            .any(|link| link.kind == RelationGraphLinkKind::Dotted)
        {
            lines.push(Line::from(Span::styled(
                "A→B uses A; block-level may not apply",
                theme::relation_text_style(),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                "A ──> B  B uses A",
                theme::relation_text_style(),
            )));
        }
        if has_grouped_links(graph) {
            lines.push(Line::from(Span::styled(
                "Grouped links may be partial",
                theme::relation_text_style(),
            )));
        }
        if graph
            .links
            .iter()
            .any(|link| link.sources.contains(&RelationSource::State))
        {
            lines.push(Line::from(Span::styled(
                "(state) from state",
                theme::relation_text_style(),
            )));
        }
    }
    if graph.nodes.iter().any(|node| node.differs) {
        lines.push(Line::from(Span::styled(
            "! differs across envs",
            theme::relation_text_style(),
        )));
    }
    if graph.nodes.iter().any(|node| !node.unresolved.is_empty()) {
        lines.push(Line::from(Span::styled(
            "? unresolved: relationship unknown",
            theme::relation_text_style(),
        )));
    }
    lines
}

pub(crate) fn help_section() -> help_dialog::HelpSection {
    help_dialog::HelpSection::new(
        "Relations",
        vec![
            help_dialog::HelpAction::new("A ──> B", "B uses A"),
            help_dialog::HelpAction::new("block-level", "may not apply to this instance"),
            help_dialog::HelpAction::new(
                "(state)",
                "recorded in state at review start; others from config",
            ),
            help_dialog::HelpAction::new("!", "the row is listed under Differs across envs in [2]"),
            help_dialog::HelpAction::new("? unresolved", "a relationship could not be determined"),
            help_dialog::HelpAction::new("Grouped links", "may apply to only some members"),
            help_dialog::HelpAction::new(
                "Scope",
                "configuration references and recorded dependencies",
            ),
            help_dialog::HelpAction::new("Not shown", "cause, impact, or execution order"),
        ],
    )
}

fn graph_lines(
    graph: &RelationGraph,
    selected_node: Option<&RelationNodeId>,
    maximized: bool,
    width: u16,
) -> Vec<Line<'static>> {
    if graph.nodes.is_empty() {
        return vec![Line::from(Span::styled(
            "No changes to show",
            theme::relation_muted_style(),
        ))];
    }

    let node_index = graph
        .nodes
        .iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let mut links_by_node = BTreeMap::<RelationNodeId, Vec<&RelationGraphLink>>::new();
    for link in &graph.links {
        links_by_node
            .entry(link.from.clone())
            .or_default()
            .push(link);
        links_by_node.entry(link.to.clone()).or_default().push(link);
    }

    let mut output_lines = Vec::new();
    for group in &graph.connected_groups {
        let ids = group.nodes.iter().cloned().collect::<BTreeSet<_>>();
        let mut group_links = BTreeMap::new();
        for id in &group.nodes {
            for link in links_by_node.get(id).into_iter().flatten() {
                if ids.contains(&link.from) && ids.contains(&link.to) {
                    group_links
                        .entry((link.from.clone(), link.to.clone()))
                        .or_insert(*link);
                }
            }
        }
        let links = group_links.into_values().collect::<Vec<_>>();
        let fits = |lines: &Vec<Line<'static>>| {
            lines.iter().all(|line| line.width() <= usize::from(width))
        };
        let diagram = tree_lines(
            group,
            &links,
            &node_index,
            selected_node,
            maximized,
            usize::from(width),
        )
        .or_else(|| merge_lines(group, &links, &node_index, selected_node, maximized).filter(fits));
        let section = diagram.unwrap_or_else(|| {
            fallback_group_lines(group, &links, &node_index, selected_node, maximized)
        });
        push_section_lines(&mut output_lines, section);
    }

    let unknown = graph
        .links_unknown
        .iter()
        .filter_map(|id| node(&node_index, id))
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        push_section_heading(&mut output_lines, "Links unknown");
        for node in unknown {
            let mut row = node_line(node, selected_node, maximized);
            row.spans.push(Span::raw("  "));
            row.spans.push(Span::styled(
                node.unresolved
                    .iter()
                    .copied()
                    .map(unresolved_label)
                    .collect::<Vec<_>>()
                    .join(", "),
                theme::relation_text_style(),
            ));
            output_lines.push(row);
        }
    }

    let no_links = graph
        .no_links_shown
        .iter()
        .filter_map(|id| node(&node_index, id))
        .collect::<Vec<_>>();
    if !no_links.is_empty() {
        push_section_heading(&mut output_lines, "No links shown");
        output_lines.extend(
            no_links
                .into_iter()
                .map(|node| node_line(node, selected_node, maximized)),
        );
    }

    output_lines
}

fn merge_lines(
    group: &RelationGraphGroup,
    links: &[&RelationGraphLink],
    node_index: &BTreeMap<RelationNodeId, &RelationNode>,
    selected_node: Option<&RelationNodeId>,
    maximized: bool,
) -> Option<Vec<Line<'static>>> {
    if group.nodes.len() != 3 || links.len() != 2 {
        return None;
    }
    let (sources, target) = merge_links(links)?;
    merge_diagram_lines(sources, target, node_index, selected_node, maximized)
}

fn merge_diagram_lines(
    sources: [&RelationGraphLink; 2],
    target: &RelationNodeId,
    node_index: &BTreeMap<RelationNodeId, &RelationNode>,
    selected_node: Option<&RelationNodeId>,
    maximized: bool,
) -> Option<Vec<Line<'static>>> {
    let source_lines = sources
        .iter()
        .map(|link| {
            node(node_index, &link.from).map(|node| node_line(node, selected_node, maximized))
        })
        .collect::<Option<Vec<_>>>()?;
    let aligned_width = source_lines
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or_default();
    let edge_prefixes = sources.map(edge_prefix);
    let max_prefix_width = edge_prefixes
        .iter()
        .map(|prefix| text_width(prefix))
        .max()
        .unwrap_or_default();

    let mut first = source_lines[0].spans.clone();
    first.push(Span::raw(
        " ".repeat(aligned_width - source_lines[0].width() + 1),
    ));
    first.push(Span::styled(
        edge_prefixes[0].clone(),
        theme::relation_text_style(),
    ));
    first.push(Span::styled(
        edge_padding(sources[0], max_prefix_width - text_width(&edge_prefixes[0])),
        theme::relation_text_style(),
    ));
    first.push(Span::styled("┐", theme::relation_text_style()));

    let mut second = source_lines[1].spans.clone();
    second.push(Span::raw(
        " ".repeat(aligned_width - source_lines[1].width() + 1),
    ));
    second.push(Span::styled(
        edge_prefixes[1].clone(),
        theme::relation_text_style(),
    ));
    second.push(Span::styled(
        edge_padding(sources[1], max_prefix_width - text_width(&edge_prefixes[1])),
        theme::relation_text_style(),
    ));
    second.push(Span::styled("┴─>", theme::relation_text_style()));
    second.extend(node_line(node(node_index, target)?, selected_node, maximized).spans);
    Some(vec![Line::from(first), Line::from(second)])
}

fn merge_links<'a>(
    links: &[&'a RelationGraphLink],
) -> Option<([&'a RelationGraphLink; 2], &'a RelationNodeId)> {
    let target = &links.first()?.to;
    if links.iter().all(|link| &link.to == target) && links[0].from != links[1].from {
        let mut sources = [links[0], links[1]];
        sources.sort_by(|left, right| left.from.cmp(&right.from));
        Some((sources, target))
    } else {
        None
    }
}

struct TreeContext<'a> {
    children: BTreeMap<&'a RelationNodeId, Vec<&'a RelationGraphLink>>,
    node_index: &'a BTreeMap<RelationNodeId, &'a RelationNode>,
    selected_node: Option<&'a RelationNodeId>,
    maximized: bool,
    width: usize,
}

// Stops at the first line wider than the pane, so deep chains never build indentation they discard.
fn tree_lines<'a>(
    group: &'a RelationGraphGroup,
    links: &[&'a RelationGraphLink],
    node_index: &'a BTreeMap<RelationNodeId, &'a RelationNode>,
    selected_node: Option<&'a RelationNodeId>,
    maximized: bool,
    width: usize,
) -> Option<Vec<Line<'static>>> {
    if links.len() + 1 != group.nodes.len() {
        return None;
    }
    let mut children = BTreeMap::<&RelationNodeId, Vec<&RelationGraphLink>>::new();
    let mut targets = BTreeSet::new();
    for link in links {
        if !targets.insert(&link.to) {
            return None;
        }
        children.entry(&link.from).or_default().push(*link);
    }
    let mut roots = group.nodes.iter().filter(|id| !targets.contains(id));
    let root = roots.next()?;
    if roots.next().is_some() {
        return None;
    }

    let context = TreeContext {
        children,
        node_index,
        selected_node,
        maximized,
        width,
    };
    let root_line = node_line(node(node_index, root)?, selected_node, maximized);
    if root_line.width() > width {
        return None;
    }
    let mut tree = vec![root_line];
    push_tree_children(&context, root, "", &mut tree)?;
    (tree.len() == group.nodes.len() * 2 - 1).then_some(tree)
}

fn push_tree_children(
    context: &TreeContext<'_>,
    parent: &RelationNodeId,
    prefix: &str,
    lines: &mut Vec<Line<'static>>,
) -> Option<()> {
    let child_links = context.children.get(parent).map_or(&[][..], Vec::as_slice);
    for (index, link) in child_links.iter().enumerate() {
        let last = index + 1 == child_links.len();
        let child = node(context.node_index, &link.to)?;
        let selected = context.selected_node == Some(&child.id);
        let connector = format!("{}{}", if last { "└" } else { "├" }, edge_segment(link));
        let mut spans = vec![
            Span::styled(selection_marker(selected), theme::relation_text_style()),
            Span::styled(prefix.to_owned(), theme::relation_text_style()),
            Span::styled(connector.clone(), theme::relation_text_style()),
            Span::raw(" "),
        ];
        spans.extend(node_spans(child, selected, context.maximized));
        let child_line = Line::from(spans);
        if child_line.width() > context.width {
            return None;
        }
        lines.push(Line::from(vec![
            Span::raw(selection_marker(false)),
            Span::styled(format!("{prefix}│"), theme::relation_text_style()),
        ]));
        lines.push(child_line);

        let child_prefix = format!(
            "{prefix}{}{}",
            if last { " " } else { "│" },
            " ".repeat(text_width(&connector)),
        );
        push_tree_children(context, &link.to, &child_prefix, lines)?;
    }
    Some(())
}

fn fallback_group_lines(
    group: &RelationGraphGroup,
    links: &[&RelationGraphLink],
    node_index: &BTreeMap<RelationNodeId, &RelationNode>,
    selected_node: Option<&RelationNodeId>,
    maximized: bool,
) -> Vec<Line<'static>> {
    let mut incoming = BTreeMap::<RelationNodeId, Vec<&RelationGraphLink>>::new();
    for link in links {
        incoming.entry(link.to.clone()).or_default().push(link);
    }
    dependency_order(&group.nodes, links)
        .into_iter()
        .filter_map(|id| {
            let mut row = node_line(node(node_index, id)?, selected_node, maximized);
            let incoming_links = incoming.get(id);
            let uses = incoming_links
                .into_iter()
                .flatten()
                .map(|link| {
                    format!(
                        "{} ({})",
                        node(node_index, &link.from)
                            .map_or_else(|| link.from.addresses().join(", "), node_path,),
                        evidence_label(link)
                    )
                })
                .collect::<Vec<_>>();
            if !uses.is_empty() {
                row.spans.push(Span::styled(
                    format!("  uses: {}", uses.join(", ")),
                    theme::relation_text_style(),
                ));
            }
            let has_grouped_link = incoming_links.into_iter().flatten().any(|link| {
                [&link.from, &link.to]
                    .into_iter()
                    .any(|id| node(node_index, id).is_some_and(|node| node.change_count > 1))
            });
            if has_grouped_link {
                row.spans.push(Span::styled(
                    "  grouped links may be partial",
                    theme::relation_muted_style(),
                ));
            }
            Some(row)
        })
        .collect()
}

// Nodes on a cycle keep their group order.
fn dependency_order<'a>(
    nodes: &'a [RelationNodeId],
    links: &[&RelationGraphLink],
) -> Vec<&'a RelationNodeId> {
    let index = nodes
        .iter()
        .enumerate()
        .map(|(position, id)| (id, position))
        .collect::<BTreeMap<_, _>>();
    let mut dependents = vec![Vec::new(); nodes.len()];
    let mut incoming = vec![0_usize; nodes.len()];
    for link in links {
        if let (Some(&from), Some(&to)) = (index.get(&link.from), index.get(&link.to))
            && from != to
        {
            dependents[from].push(to);
            incoming[to] += 1;
        }
    }

    let mut remaining = (0..nodes.len()).collect::<BTreeSet<_>>();
    let mut ready = (0..nodes.len())
        .filter(|&position| incoming[position] == 0)
        .collect::<BTreeSet<_>>();
    let mut ordered = Vec::with_capacity(nodes.len());
    while let Some(next) = ready.pop_first().or_else(|| remaining.first().copied()) {
        remaining.remove(&next);
        ordered.push(&nodes[next]);
        for &dependent in &dependents[next] {
            if remaining.contains(&dependent) {
                incoming[dependent] -= 1;
                if incoming[dependent] == 0 {
                    ready.insert(dependent);
                }
            }
        }
    }
    ordered
}

fn node_line(
    node: &RelationNode,
    selected_node: Option<&RelationNodeId>,
    maximized: bool,
) -> Line<'static> {
    let selected = selected_node == Some(&node.id);
    let mut spans = vec![Span::styled(
        selection_marker(selected),
        theme::relation_text_style(),
    )];
    spans.extend(node_spans(node, selected, maximized));
    Line::from(spans)
}

const fn selection_marker(selected: bool) -> &'static str {
    if selected { "> " } else { "  " }
}

fn node_spans(node: &RelationNode, selected: bool, maximized: bool) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::styled(
            operation_symbol(node.operation),
            operation_style(node.operation),
        ),
        Span::styled(" ", theme::relation_text_style()),
    ];
    let breadcrumbs = visible_breadcrumbs(&node.breadcrumbs, maximized);
    if !breadcrumbs.is_empty() {
        spans.push(Span::styled(
            breadcrumbs.join(" › "),
            theme::relation_muted_style(),
        ));
        spans.push(Span::styled(" › ", theme::relation_muted_style()));
    }
    let mut address_style = theme::relation_text_style();
    if selected {
        address_style = address_style.add_modifier(Modifier::UNDERLINED);
    }
    if node.differs {
        address_style = address_style.add_modifier(Modifier::BOLD);
    }
    spans.push(Span::styled(node.display_address.clone(), address_style));
    if node.display_address.contains("[*]") && node.change_count > 0 {
        spans.push(Span::styled(
            format!(" ×{}", node.change_count),
            theme::relation_text_style(),
        ));
    }
    if node.has_unknown {
        spans.push(Span::styled(
            " [unknown values]",
            theme::relation_text_style(),
        ));
    }
    if node.differs {
        spans.push(Span::styled(" !", theme::relation_difference_style()));
    }
    if !node.unresolved.is_empty() {
        spans.push(Span::styled(" ?", theme::relation_muted_style()));
    }
    spans
}

fn has_grouped_links(graph: &RelationGraph) -> bool {
    graph.links.iter().any(|link| {
        [&link.from, &link.to].into_iter().any(|id| {
            graph
                .nodes
                .iter()
                .find(|node| node.id == *id)
                .is_some_and(|node| node.change_count > 1)
        })
    })
}

fn visible_breadcrumbs(breadcrumbs: &[String], maximized: bool) -> Vec<String> {
    if maximized || breadcrumbs.len() < 3 {
        return breadcrumbs.to_vec();
    }
    vec![
        breadcrumbs[0].clone(),
        "…".to_owned(),
        breadcrumbs[breadcrumbs.len() - 1].clone(),
    ]
}

fn node_path(node: &RelationNode) -> String {
    let mut parts = node.breadcrumbs.clone();
    parts.push(node.display_address.clone());
    parts.join(" › ")
}

fn edge_prefix(link: &RelationGraphLink) -> String {
    edge_segment(link).trim_end_matches('>').to_owned()
}

fn edge_padding(link: &RelationGraphLink, width: usize) -> String {
    let glyph = match link.kind {
        RelationGraphLinkKind::Solid => "─",
        RelationGraphLinkKind::Dotted => "┄",
    };
    glyph.repeat(width)
}

fn text_width(text: &str) -> usize {
    Line::from(Span::raw(text.to_owned())).width()
}

fn node<'a>(
    index: &BTreeMap<RelationNodeId, &'a RelationNode>,
    id: &RelationNodeId,
) -> Option<&'a RelationNode> {
    index.get(id).copied()
}

const fn operation_symbol(operation: ResourceChangeKind) -> &'static str {
    match operation {
        ResourceChangeKind::Create | ResourceChangeKind::Import => "+",
        ResourceChangeKind::Update => "~",
        ResourceChangeKind::Replace => "-/+",
        ResourceChangeKind::Delete => "-",
        ResourceChangeKind::NoOp => "=",
        ResourceChangeKind::Read => "r",
        ResourceChangeKind::Move => "m",
        ResourceChangeKind::Unknown => "?",
        ResourceChangeKind::Unsupported => "!",
    }
}

fn operation_style(operation: ResourceChangeKind) -> ratatui::style::Style {
    match operation {
        ResourceChangeKind::Create | ResourceChangeKind::Import => theme::relation_create_style(),
        ResourceChangeKind::Update => theme::relation_update_style(),
        ResourceChangeKind::Replace => theme::relation_replace_style(),
        ResourceChangeKind::Delete => theme::relation_delete_style(),
        ResourceChangeKind::Unsupported => theme::relation_warning_style(),
        ResourceChangeKind::NoOp
        | ResourceChangeKind::Read
        | ResourceChangeKind::Move
        | ResourceChangeKind::Unknown => theme::relation_text_style(),
    }
}

// Configuration is the unmarked default; a line names its sources only when state is involved.
fn edge_segment(link: &RelationGraphLink) -> String {
    let glyph = match link.kind {
        RelationGraphLinkKind::Solid => "─",
        RelationGraphLinkKind::Dotted => "┄",
    };
    let evidence = if link.sources.contains(&RelationSource::State) {
        source_label(link)
    } else {
        String::new()
    };
    if evidence.is_empty() {
        format!("{glyph}{glyph}>")
    } else {
        format!("{glyph}({evidence}){glyph}>")
    }
}

fn evidence_label(link: &RelationGraphLink) -> String {
    let mut labels = source_label(link);
    if link.kind == RelationGraphLinkKind::Dotted {
        if !labels.is_empty() {
            labels.push_str(", ");
        }
        labels.push_str("block-level");
    }
    labels
}

fn source_label(link: &RelationGraphLink) -> String {
    link.sources
        .iter()
        .map(|source| match source {
            RelationSource::Configuration => "config",
            RelationSource::State => "state",
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn push_section_heading(lines: &mut Vec<Line<'static>>, heading: &'static str) {
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    lines.push(Line::from(Span::styled(
        heading,
        theme::relation_section_style(),
    )));
}

fn push_section_lines(lines: &mut Vec<Line<'static>>, section: Vec<Line<'static>>) {
    if !lines.is_empty() {
        lines.push(Line::default());
    }
    lines.extend(section);
}

const fn unresolved_label(reason: RelationUnresolvedReason) -> &'static str {
    match reason {
        RelationUnresolvedReason::LocalValue => "local unresolved",
        RelationUnresolvedReason::Variable => "module variable unresolved",
        RelationUnresolvedReason::MissingAddress => "address unresolved",
        RelationUnresolvedReason::AmbiguousModule => "module reference unresolved",
        RelationUnresolvedReason::CyclicReference => "reference cycle",
        RelationUnresolvedReason::InvalidConfiguration => "invalid configuration relation",
        RelationUnresolvedReason::InvalidState => "invalid state relation",
        RelationUnresolvedReason::ConfigurationPartial => "configuration partial",
        RelationUnresolvedReason::ConfigurationUnavailable => "configuration unavailable",
        RelationUnresolvedReason::ConfigurationNotCollected => "configuration not collected",
        RelationUnresolvedReason::StateUnavailable => "state unavailable",
        RelationUnresolvedReason::StateNotCollected => "state not collected",
    }
}

fn offset_limit(content: usize, viewport: usize) -> u16 {
    u16::try_from(content.saturating_sub(viewport)).unwrap_or(u16::MAX)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use ratatui::{layout::Rect, style::Color};

    use crate::{
        app::plan::{
            RelationGraph, RelationGraphGroup, RelationGraphLink, RelationGraphLinkKind,
            RelationNode, RelationNodeId, RelationSource, RelationUnresolvedReason,
            ResourceChangeKind,
        },
        ui::{
            test_support::{buffer_text, buffer_visual_snapshot, render_to_buffer},
            theme,
        },
    };

    use super::{
        RelationGraphScroll, RelationGraphTitle, RelationGraphView, compact_legend_lines,
        dependency_order, graph_lines, legend_lines, node_line, render, title_line,
    };

    #[test]
    fn relation_title_keeps_environment_and_scope_styles_separate() {
        let title = title_line(
            RelationGraphTitle {
                environment: Some("prod · blue"),
                scope: "not compared",
            },
            false,
        );

        assert_eq!(
            title.to_string(),
            "  [3] Relations · prod · blue · not compared"
        );
        assert_eq!(title.spans[2].content, " · prod · blue");
        assert_eq!(title.spans[2].style.fg, Some(Color::Reset));
        assert_eq!(title.spans[2].style.bg, Some(Color::Reset));
        assert_eq!(title.spans[3].content, " · not compared");
        assert_eq!(title.spans[3].style.fg, Some(Color::DarkGray));

        let single_environment = title_line(
            RelationGraphTitle {
                environment: None,
                scope: "whole env",
            },
            true,
        );
        assert_eq!(
            single_environment.to_string(),
            "* [3] Relations · whole env"
        );
        assert_eq!(single_environment.spans[2].style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn fallback_order_puts_used_nodes_first_and_keeps_group_order_on_cycles() {
        let nodes = (0..1_000)
            .map(|index| {
                node(
                    &format!("aws_service.n{index:04}"),
                    ResourceChangeKind::Update,
                )
            })
            .collect::<Vec<_>>();
        let reversed_chain = nodes
            .windows(2)
            .map(|pair| {
                link(
                    &pair[1],
                    &pair[0],
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                )
            })
            .collect::<Vec<_>>();
        let ids = nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
        let cycle = [
            link(
                &nodes[0],
                &nodes[1],
                RelationGraphLinkKind::Solid,
                &[RelationSource::Configuration],
            ),
            link(
                &nodes[1],
                &nodes[0],
                RelationGraphLinkKind::Solid,
                &[RelationSource::Configuration],
            ),
        ];

        let chain = dependency_order(&ids, &reversed_chain.iter().collect::<Vec<_>>());
        let cycle = dependency_order(&ids[..2], &cycle.iter().collect::<Vec<_>>());

        assert!(chain.iter().copied().eq(ids.iter().rev()));
        assert!(cycle.iter().copied().eq(ids[..2].iter()));
    }

    #[test]
    fn chain_deeper_than_the_pane_falls_back_to_uses_rows() {
        let nodes = (0..5_000)
            .map(|index| {
                node(
                    &format!("aws_service.n{index:04}"),
                    ResourceChangeKind::Update,
                )
            })
            .collect::<Vec<_>>();
        let links = nodes
            .windows(2)
            .map(|pair| {
                link(
                    &pair[0],
                    &pair[1],
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                )
            })
            .collect::<Vec<_>>();
        let graph = graph(nodes, links);

        let rendered = graph_lines(&graph, None, false, 80)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        assert!(rendered.iter().all(|line| !line.contains("└──>")));
        assert_eq!(rendered.len(), 5_000);
        assert_eq!(
            rendered[1],
            "  ~ aws_service.n0001  uses: aws_service.n0000 (config)"
        );
    }

    #[test]
    fn relation_legend_shows_only_applicable_explanations() {
        let no_links = legend_lines(&single_node_graph())
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(no_links, ["! differs across envs"]);

        let unresolved = legend_lines(&isolated_graph());
        let unresolved = unresolved
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(unresolved.len(), 1);
        assert!(unresolved[0].starts_with("? unresolved"));
        assert!(!unresolved[0].contains("A ──> B"));

        let plain_a = node("aws_vpc.main", ResourceChangeKind::Create);
        let plain_b = node("aws_subnet.web", ResourceChangeKind::Create);
        let plain = graph(
            vec![plain_a.clone(), plain_b.clone()],
            vec![link(
                &plain_a,
                &plain_b,
                RelationGraphLinkKind::Solid,
                &[RelationSource::Configuration],
            )],
        );
        let plain = legend_lines(&plain)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(plain, ["A ──> B  B uses A"]);

        let annotated = legend_lines(&branch_graph())
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert_eq!(
            annotated,
            [
                "A ──> B  B uses A",
                "A ┄┄> B  block-level, may not apply",
                "(state) from state; unmarked from configuration",
            ]
        );
    }

    #[test]
    fn grouped_node_note_and_link_scope_remain_distinct_from_unresolved_links() {
        let mut aggregate = node("aws_instance.web[*]", ResourceChangeKind::Update);
        aggregate.change_count = 2;
        aggregate.has_unknown = true;
        aggregate
            .unresolved
            .insert(RelationUnresolvedReason::ConfigurationUnavailable);
        let target = node("aws_subnet.web", ResourceChangeKind::Create);
        let graph = graph(
            vec![aggregate.clone(), target.clone()],
            vec![link(
                &aggregate,
                &target,
                RelationGraphLinkKind::Solid,
                &[RelationSource::Configuration],
            )],
        );
        let output = render_to_buffer((165, 50), |frame| {
            render(
                frame,
                Rect::new(0, 0, 165, 50),
                &graph,
                &view(None, "whole env", None, false, 0, 0),
            );
        });
        insta::assert_snapshot!(
            "grouped_link_unknown_node_styles_165x50",
            buffer_visual_snapshot(&output)
        );

        let rendered_node = node_line(&aggregate, None, false).to_string();
        let legend = legend_lines(&graph)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let compact = compact_legend_lines(&graph)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        let fallback = graph_lines(&graph, None, false, 8)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        assert!(
            rendered_node.contains("[unknown values]"),
            "{rendered_node}"
        );
        assert!(rendered_node.contains(" ?"), "{rendered_node}");
        assert_eq!(
            legend,
            [
                "A ──> B  B uses A",
                "Grouped links may apply to only some members",
                "? unresolved means a relationship could not be determined",
            ]
        );
        assert!(
            compact
                .iter()
                .any(|line| line == "Grouped links may be partial")
        );
        assert!(
            fallback
                .iter()
                .any(|line| line.contains("uses:") && line.contains("grouped links may be partial")),
            "{fallback:?}"
        );
        assert_eq!(
            fallback
                .iter()
                .filter(|line| line.contains("grouped links may be partial"))
                .count(),
            1,
            "{fallback:?}"
        );
    }

    #[test]
    fn branch_draws_each_dependent_on_its_own_rail() {
        let graph = branch_graph();

        let text = buffer_text(&render_to_buffer((75, 20), |frame| {
            render(
                frame,
                Rect::new(0, 0, 75, 20),
                &graph,
                &view(Some("prod"), "whole env", None, false, 0, 0),
            );
        }));

        let rows = text.lines().collect::<Vec<_>>();
        let root = rows
            .iter()
            .position(|row| row.contains("-/+ app › terraform_data.db"))
            .expect("tree root is drawn");
        assert!(rows[root + 1].ends_with("  │"), "{text}");
        assert!(
            rows[root + 2].contains("├─(state)─> ~ app › aws_ecs_service.api"),
            "{text}"
        );
        assert!(
            rows[root + 4].contains("└┄(config,state)┄> ~ app › dns › aws_route53_record.db"),
            "{text}"
        );
        assert!(!text.contains("uses:"), "{text}");
    }

    #[test]
    fn tree_keeps_sibling_rails_beside_nested_dependents() {
        let source = node("terraform_data.source", ResourceChangeKind::Update);
        let left = node("aws_service.left", ResourceChangeKind::Update);
        let right = node("aws_service.right", ResourceChangeKind::Update);
        let nested = node("aws_listener.nested", ResourceChangeKind::Create);
        let graph = graph(
            vec![source.clone(), left.clone(), right.clone(), nested.clone()],
            vec![
                link(
                    &source,
                    &left,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
                link(
                    &source,
                    &right,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
                link(
                    &left,
                    &nested,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
            ],
        );

        let text = buffer_text(&render_to_buffer((165, 50), |frame| {
            render(
                frame,
                Rect::new(0, 0, 165, 50),
                &graph,
                &view(Some("prod"), "whole env", None, false, 0, 0),
            );
        }));

        let rows = text
            .lines()
            .skip_while(|row| !row.contains("~ terraform_data.source"))
            .take(7)
            .map(|row| row.trim_start_matches('│').trim_end_matches('│').trim_end())
            .collect::<Vec<_>>();
        assert_eq!(
            rows,
            [
                "  ~ terraform_data.source",
                "  │",
                "  ├──> ~ aws_service.left",
                "  │    │",
                "  │    └──> + aws_listener.nested",
                "  │",
                "  └──> ~ aws_service.right",
            ],
            "{text}"
        );
    }

    #[test]
    fn branch_keeps_all_nodes_and_evidence_visible() {
        let graph = branch_graph();
        let output = render_to_buffer((165, 50), |frame| {
            render(
                frame,
                Rect::new(0, 0, 165, 50),
                &graph,
                &RelationGraphView {
                    title: RelationGraphTitle {
                        environment: Some("prod"),
                        scope: "whole env",
                    },
                    selected_node: None,
                    focused: true,
                    maximized: false,
                    scroll: RelationGraphScroll {
                        vertical: 0,
                        horizontal: 0,
                    },
                },
            );
        });
        let text = buffer_text(&output);
        insta::assert_snapshot!(
            "branch_graph_styles_165x50",
            buffer_visual_snapshot(&output)
        );

        assert!(text.contains("terraform_data.db"), "{text}");
        assert!(text.contains("aws_ecs_service.api"));
        assert!(text.contains("aws_route53_record.db"));
        assert!(text.contains("├─(state)─>"), "{text}");
        assert!(text.contains("└┄(config,state)┄>"), "{text}");
        assert!(text.contains("A ──> B  B uses A"));
        assert!(text.contains("A ┄┄> B  block-level, may not apply"));
        assert!(text.contains("(state) from state; unmarked from configuration"));

        let rows = text.lines().collect::<Vec<_>>();
        let first = rows.iter().position(|row| row.contains('├')).unwrap();
        let last = rows.iter().position(|row| row.contains('└')).unwrap();
        assert_eq!(
            glyph_column(rows[first], '├'),
            glyph_column(rows[last], '└')
        );
        let rail = glyph_column(rows[first], '├');
        assert!(
            rows[first + 1..last]
                .iter()
                .all(|row| row.chars().nth(rail) == Some('│')),
            "{text}"
        );
    }

    #[test]
    fn chains_and_merges_render_with_direction_and_source_annotations() {
        let chain = chain_graph();
        let chain_text = buffer_text(&render_to_buffer((165, 50), |frame| {
            render(
                frame,
                Rect::new(0, 0, 165, 50),
                &chain,
                &view(Some("prod"), "whole env", None, false, 0, 0),
            );
        }));
        let merge = merge_graph();
        let merge_text = buffer_text(&render_to_buffer((165, 50), |frame| {
            render(
                frame,
                Rect::new(0, 0, 165, 50),
                &merge,
                &view(Some("prod"), "whole env", None, false, 0, 0),
            );
        }));

        let chain_rows = chain_text.lines().collect::<Vec<_>>();
        let root = chain_rows
            .iter()
            .position(|row| row.contains("+ aws_vpc.main"))
            .expect("chain root is drawn");
        assert!(
            chain_rows[root + 2].contains("└──> ~ aws_subnet.web"),
            "{chain_text}"
        );
        assert!(
            chain_rows[root + 4].contains("└─(state)─> + aws_instance.api"),
            "{chain_text}"
        );
        assert!(merge_text.contains("┴─>"));
        assert!(merge_text.contains("(config,state)"));
        assert!(merge_text.contains("┐"), "{merge_text}");

        let merge_rows = merge_text.lines().collect::<Vec<_>>();
        let top = merge_rows
            .iter()
            .position(|row| row.contains('┐') && row.contains("aws_"))
            .unwrap();
        assert_eq!(
            glyph_column(merge_rows[top], '┐'),
            glyph_column(merge_rows[top + 1], '┴')
        );
        let annotated = merge_rows[top + 1];
        assert!(annotated.contains("(config,state)"), "{merge_text}");
        assert_edge_annotation_trails(annotated, '┴');
    }

    #[test]
    fn overlapping_branch_and_merge_fall_back_to_directional_uses_rows() {
        let graph = branch_merge_graph();
        let output = render_to_buffer((165, 50), |frame| {
            render(
                frame,
                Rect::new(0, 0, 165, 50),
                &graph,
                &view(Some("dev"), "whole env", None, false, 0, 0),
            );
        });
        let text = buffer_text(&output);
        insta::assert_snapshot!(
            "branch_merge_fallback_styles_165x50",
            buffer_visual_snapshot(&output)
        );

        for address in [
            "terraform_data.source",
            "aws_service.left",
            "aws_service.right",
            "aws_listener.target",
            "aws_route53_record.tail",
        ] {
            assert_eq!(
                text.lines()
                    .filter(|line| line
                        .split("  uses:")
                        .next()
                        .is_some_and(|row| row.contains(address)))
                    .count(),
                1,
                "{address}\n{text}"
            );
        }
        assert!(
            text.contains(
                "aws_listener.target  uses: aws_service.left (config), aws_service.right (state)"
            ),
            "{text}"
        );
        assert!(text.contains("aws_service.left  uses: terraform_data.source (config)"));
        assert!(
            text.contains("aws_service.right  uses: terraform_data.source (config, block-level)")
        );
        assert!(text.contains("aws_route53_record.tail  uses: aws_listener.target (config)"));
    }

    #[test]
    fn unsupported_cycles_fall_back_to_incoming_uses_rows() {
        let graph = cycle_graph();
        let text = buffer_text(&render_to_buffer((120, 40), |frame| {
            render(
                frame,
                Rect::new(0, 0, 120, 40),
                &graph,
                &view(Some("dev"), "whole env", None, true, 0, 0),
            );
        }));

        assert!(text.contains("uses: aws_db.main (config)"), "{text}");
        assert!(text.contains("uses: aws_service.api (state, block-level)"));
        assert!(!text.contains("aws_db.main ──>"));
        assert!(!text.contains("aws_service.api ──>"));
    }

    #[test]
    fn unknown_reasons_precede_no_links_and_remain_distinct() {
        let graph = isolated_graph();
        let output = render_to_buffer((80, 24), |frame| {
            render(
                frame,
                Rect::new(0, 0, 80, 24),
                &graph,
                &view(None, "whole env", None, false, 0, 0),
            );
        });
        let text = buffer_text(&output);
        insta::assert_snapshot!(
            "unknown_and_no_links_styles_80x24",
            buffer_visual_snapshot(&output)
        );

        assert!(text.find("Links unknown").unwrap() < text.find("No links shown").unwrap());
        assert!(text.contains("configuration unavailable"));
        assert!(text.contains("state unavailable"));
        assert!(text.contains("module reference unresolved"));
        assert!(text.contains("terraform_data.db"));
        assert!(text.contains("aws_security_group.web"));
        let unresolved_node = text
            .lines()
            .find(|line| line.contains("terraform_data.db"))
            .unwrap();
        assert_eq!(unresolved_node.matches('?').count(), 1, "{unresolved_node}");
    }

    #[test]
    fn fallback_uses_full_module_path_to_distinguish_references() {
        let x = node_with(
            "module.a.module.x.module.z.aws_db.main",
            "aws_db.main",
            ResourceChangeKind::Update,
            1,
            &["a", "x", "z"],
            false,
            &[],
        );
        let y = node_with(
            "module.a.module.y.module.z.aws_db.main",
            "aws_db.main",
            ResourceChangeKind::Update,
            1,
            &["a", "y", "z"],
            false,
            &[],
        );
        let third = node("aws_vpc.main", ResourceChangeKind::Update);
        let dependent = node("aws_service.api", ResourceChangeKind::Update);
        let graph = graph(
            vec![x.clone(), y.clone(), third.clone(), dependent.clone()],
            vec![
                link(
                    &x,
                    &dependent,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
                link(
                    &y,
                    &dependent,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
                link(
                    &third,
                    &dependent,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
            ],
        );
        let text = buffer_text(&render_to_buffer((165, 50), |frame| {
            render(
                frame,
                Rect::new(0, 0, 165, 50),
                &graph,
                &view(None, "whole env", None, false, 0, 0),
            );
        }));

        assert!(text.contains("a › x › z › aws_db.main (config)"), "{text}");
        assert!(text.contains("a › y › z › aws_db.main (config)"), "{text}");
        assert!(!text.contains("a › … › z › aws_db.main (config)"));
    }

    #[test]
    fn selected_diff_node_keeps_operation_color_and_underlines_only_address() {
        let graph = single_node_graph();
        let output = render_to_buffer((80, 24), |frame| {
            render(
                frame,
                Rect::new(0, 0, 80, 24),
                &graph,
                &view(
                    Some("prod"),
                    "not compared",
                    graph.nodes.first().map(|node| &node.id),
                    false,
                    0,
                    0,
                ),
            );
        });

        let text = buffer_text(&output);
        assert!(
            text.contains("> -/+ app › … › dns › aws_db.main[*] ×3 !"),
            "{text}"
        );
        assert_eq!(output.cell((1, 2)).unwrap().symbol(), ">");
        assert_eq!(output.cell((3, 2)).unwrap().fg, Color::Magenta);
        assert!(
            !output
                .cell((3, 2))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
        assert!(
            output
                .cell((23, 2))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
        assert!(
            output
                .cell((23, 2))
                .unwrap()
                .modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
        assert_eq!(output.cell((0, 0)).unwrap().fg, Color::DarkGray);
        assert_eq!(output.cell((1, 23)).unwrap().fg, Color::DarkGray);

        let expanded_text = buffer_text(&render_to_buffer((100, 30), |frame| {
            render(
                frame,
                Rect::new(0, 0, 100, 30),
                &graph,
                &RelationGraphView {
                    title: RelationGraphTitle {
                        environment: Some("prod"),
                        scope: "whole env",
                    },
                    selected_node: None,
                    focused: false,
                    maximized: true,
                    scroll: RelationGraphScroll {
                        vertical: 0,
                        horizontal: 0,
                    },
                },
            );
        }));
        assert!(expanded_text.contains("app › net › dns › aws_db.main[*]"));
        assert!(!expanded_text.contains("app › … › dns"));
    }

    #[test]
    fn narrow_view_keeps_frame_and_content_visible_without_an_unneeded_legend() {
        let graph = long_graph();
        let mut returned = RelationGraphScroll {
            vertical: 0,
            horizontal: 0,
        };
        let output = render_to_buffer((40, 16), |frame| {
            returned = render(
                frame,
                Rect::new(0, 0, 40, 16),
                &graph,
                &view(Some("prod"), "whole env", None, false, u16::MAX, u16::MAX),
            );
        });
        let text = buffer_text(&output);
        insta::assert_snapshot!(
            "narrow_relation_styles_40x16",
            buffer_visual_snapshot(&output)
        );

        assert!(returned.vertical > 0);
        assert!(returned.horizontal > 0);
        assert!(text.contains("┌"));
        assert!(text.contains("this_19"));
        assert!(!text.contains("A ──> B"));
        assert!(!text.contains("(state) from state"));
    }

    #[test]
    fn short_view_keeps_relation_content_visible() {
        let graph = branch_graph();

        for height in [4, 5] {
            let text = buffer_text(&render_to_buffer((120, height), |frame| {
                render(
                    frame,
                    Rect::new(0, 0, 120, height),
                    &graph,
                    &view(Some("prod"), "whole env", None, false, 0, 0),
                );
            }));

            assert!(
                text.contains("terraform_data.db"),
                "height={height}\n{text}"
            );
        }
    }

    #[test]
    fn focused_and_operation_styles_use_terminal_palette_colors() {
        assert_eq!(theme::relation_frame_style(true).fg, Some(Color::Cyan));
        assert_eq!(theme::relation_create_style().fg, Some(Color::Green));
        assert_eq!(theme::relation_update_style().fg, Some(Color::Yellow));
        assert_eq!(theme::relation_replace_style().fg, Some(Color::Magenta));
        assert_eq!(theme::relation_delete_style().fg, Some(Color::Red));
        assert_eq!(theme::relation_text_style().bg, Some(Color::Reset));
    }

    fn view<'a>(
        environment: Option<&'a str>,
        scope: &'a str,
        selected_node: Option<&'a RelationNodeId>,
        focused: bool,
        vertical: u16,
        horizontal: u16,
    ) -> RelationGraphView<'a> {
        RelationGraphView {
            title: RelationGraphTitle { environment, scope },
            selected_node,
            focused,
            maximized: false,
            scroll: RelationGraphScroll {
                vertical,
                horizontal,
            },
        }
    }

    fn glyph_column(line: &str, glyph: char) -> usize {
        line.chars()
            .position(|character| character == glyph)
            .unwrap()
    }

    fn assert_edge_annotation_trails(line: &str, junction: char) {
        let characters = line.chars().collect::<Vec<_>>();
        let junction_column = characters
            .iter()
            .position(|character| *character == junction)
            .unwrap();
        let label_end = characters[..junction_column]
            .iter()
            .rposition(|character| *character == ')')
            .unwrap();
        let label_start = characters[..label_end]
            .iter()
            .rposition(|character| *character == '(')
            .unwrap();
        let edge_glyph = characters[label_start - 1];
        assert!(
            characters[label_end + 1..junction_column]
                .iter()
                .all(|character| *character == edge_glyph)
        );
    }

    fn node(address: &str, operation: ResourceChangeKind) -> RelationNode {
        let parts = address.split('.').collect::<Vec<_>>();
        let resource_index = parts
            .iter()
            .position(|part| part.starts_with("aws_") || part.starts_with("terraform_"))
            .unwrap_or_default();
        let breadcrumbs = parts[..resource_index]
            .chunks(2)
            .filter_map(|pair| {
                (pair.first() == Some(&"module"))
                    .then(|| pair.get(1).copied())
                    .flatten()
            })
            .map(str::to_owned)
            .collect();
        RelationNode {
            id: RelationNodeId::from_addresses([address.to_owned()]).unwrap(),
            display_address: parts[resource_index..].join("."),
            operation,
            change_count: 1,
            breadcrumbs,
            differs: false,
            has_unknown: false,
            unresolved: BTreeSet::new(),
        }
    }

    fn node_with(
        address: &str,
        display_address: &str,
        operation: ResourceChangeKind,
        change_count: usize,
        breadcrumbs: &[&str],
        differs: bool,
        unresolved: &[RelationUnresolvedReason],
    ) -> RelationNode {
        RelationNode {
            id: RelationNodeId::from_addresses([address.to_owned()]).unwrap(),
            display_address: display_address.to_owned(),
            operation,
            change_count,
            breadcrumbs: breadcrumbs.iter().map(|part| (*part).to_owned()).collect(),
            differs,
            has_unknown: false,
            unresolved: unresolved.iter().copied().collect(),
        }
    }

    fn link(
        from: &RelationNode,
        to: &RelationNode,
        kind: RelationGraphLinkKind,
        sources: &[RelationSource],
    ) -> RelationGraphLink {
        RelationGraphLink {
            from: from.id.clone(),
            to: to.id.clone(),
            kind,
            sources: sources.iter().copied().collect(),
        }
    }

    fn graph(nodes: Vec<RelationNode>, links: Vec<RelationGraphLink>) -> RelationGraph {
        let linked = links
            .iter()
            .flat_map(|link| [link.from.clone(), link.to.clone()])
            .collect::<BTreeSet<_>>();
        let group_nodes = linked.iter().cloned().collect::<Vec<_>>();
        let connected_groups = if group_nodes.is_empty() {
            Vec::new()
        } else {
            vec![RelationGraphGroup {
                nodes: group_nodes,
                contains_destructive_change: false,
            }]
        };
        let links_unknown = nodes
            .iter()
            .filter(|node| !linked.contains(&node.id) && !node.unresolved.is_empty())
            .map(|node| node.id.clone())
            .collect();
        let no_links_shown = nodes
            .iter()
            .filter(|node| !linked.contains(&node.id) && node.unresolved.is_empty())
            .map(|node| node.id.clone())
            .collect();
        RelationGraph {
            nodes,
            links,
            connected_groups,
            links_unknown,
            no_links_shown,
        }
    }

    fn branch_graph() -> RelationGraph {
        let source = node("module.app.terraform_data.db", ResourceChangeKind::Replace);
        let service = node("module.app.aws_ecs_service.api", ResourceChangeKind::Update);
        let record = node(
            "module.app.module.dns.aws_route53_record.db",
            ResourceChangeKind::Update,
        );
        graph(
            vec![source.clone(), service.clone(), record.clone()],
            vec![
                link(
                    &source,
                    &service,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::State],
                ),
                link(
                    &source,
                    &record,
                    RelationGraphLinkKind::Dotted,
                    &[RelationSource::Configuration, RelationSource::State],
                ),
            ],
        )
    }

    fn branch_merge_graph() -> RelationGraph {
        let source = node("terraform_data.source", ResourceChangeKind::Replace);
        let left = node("aws_service.left", ResourceChangeKind::Update);
        let right = node("aws_service.right", ResourceChangeKind::Update);
        let target = node("aws_listener.target", ResourceChangeKind::Update);
        let tail = node("aws_route53_record.tail", ResourceChangeKind::Update);
        graph(
            vec![
                source.clone(),
                left.clone(),
                right.clone(),
                target.clone(),
                tail.clone(),
            ],
            vec![
                link(
                    &source,
                    &left,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
                link(
                    &source,
                    &right,
                    RelationGraphLinkKind::Dotted,
                    &[RelationSource::Configuration],
                ),
                link(
                    &left,
                    &target,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
                link(
                    &right,
                    &target,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::State],
                ),
                link(
                    &target,
                    &tail,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
            ],
        )
    }

    fn cycle_graph() -> RelationGraph {
        let database = node("aws_db.main", ResourceChangeKind::Update);
        let service = node("aws_service.api", ResourceChangeKind::Replace);
        graph(
            vec![database.clone(), service.clone()],
            vec![
                link(
                    &database,
                    &service,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
                link(
                    &service,
                    &database,
                    RelationGraphLinkKind::Dotted,
                    &[RelationSource::State],
                ),
            ],
        )
    }

    fn chain_graph() -> RelationGraph {
        let vpc = node("aws_vpc.main", ResourceChangeKind::Create);
        let subnet = node("aws_subnet.web", ResourceChangeKind::Update);
        let instance = node("aws_instance.api", ResourceChangeKind::Create);
        graph(
            vec![vpc.clone(), subnet.clone(), instance.clone()],
            vec![
                link(
                    &vpc,
                    &subnet,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration],
                ),
                link(
                    &subnet,
                    &instance,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::State],
                ),
            ],
        )
    }

    fn merge_graph() -> RelationGraph {
        let config = node("aws_vpc.main", ResourceChangeKind::Update);
        let state = node("aws_subnet.web", ResourceChangeKind::Update);
        let dependent = node("aws_instance.api", ResourceChangeKind::Replace);
        graph(
            vec![config.clone(), state.clone(), dependent.clone()],
            vec![
                link(
                    &config,
                    &dependent,
                    RelationGraphLinkKind::Solid,
                    &[RelationSource::Configuration, RelationSource::State],
                ),
                link(
                    &state,
                    &dependent,
                    RelationGraphLinkKind::Dotted,
                    &[RelationSource::Configuration],
                ),
            ],
        )
    }

    fn isolated_graph() -> RelationGraph {
        graph(
            vec![
                node_with(
                    "module.app.terraform_data.db",
                    "terraform_data.db",
                    ResourceChangeKind::Delete,
                    1,
                    &["app"],
                    false,
                    &[RelationUnresolvedReason::ConfigurationUnavailable],
                ),
                node_with(
                    "module.app.aws_security_group.web",
                    "aws_security_group.web",
                    ResourceChangeKind::Create,
                    1,
                    &["app"],
                    false,
                    &[RelationUnresolvedReason::AmbiguousModule],
                ),
                node_with(
                    "module.app.aws_route53_record.db",
                    "aws_route53_record.db",
                    ResourceChangeKind::Update,
                    1,
                    &["app"],
                    false,
                    &[RelationUnresolvedReason::StateUnavailable],
                ),
                node("aws_vpc.main", ResourceChangeKind::Update),
            ],
            Vec::new(),
        )
    }

    fn single_node_graph() -> RelationGraph {
        graph(
            vec![node_with(
                "module.app.module.dns.aws_db.main[0]",
                "aws_db.main[*]",
                ResourceChangeKind::Replace,
                3,
                &["app", "net", "dns"],
                true,
                &[],
            )],
            Vec::new(),
        )
    }

    fn long_graph() -> RelationGraph {
        graph(
            (0..20)
                .map(|index| {
                    node_with(
                        &format!("aws_long_resource.this_{index}"),
                        &format!("aws_long_resource_with_many_segments.this_{index}"),
                        ResourceChangeKind::Update,
                        1,
                        &["a_very_long_module_name", "nested_module"],
                        false,
                        &[],
                    )
                })
                .collect(),
            Vec::new(),
        )
    }
}
