use std::{
    collections::BTreeMap,
    fmt::{self, Debug, Formatter},
};

use super::{
    EnvironmentPlan,
    comparison::{
        CellState, ComparisonRow, ComparisonScope, DifferenceReason, EnvironmentSelection,
        compare_environments_for_selection,
    },
};
use crate::app::{
    plan::{
        RelationGraph, RelationNode, RelationNodeId, ResourceChange, ResourceChangeKind,
        build_relation_graph,
        comparison::resource_has_unknown,
        grouping::{
            ChangeGroup, GroupingCandidate, GroupingKey, PlanGrouping, group_resource_changes,
            grouping_candidate,
        },
        path::{module_breadcrumbs, normalize_resource_addresses, resource_display_address},
    },
    review::PlanReview,
    session::ReviewSessionState,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvironmentOverview {
    pub(crate) scope: ComparisonScope,
    pub(crate) rows: Vec<OverviewRow>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvironmentOverviewWithRelations {
    pub(crate) overview: EnvironmentOverview,
    pub(crate) relations: BTreeMap<usize, EnvironmentRelationGraph>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvironmentRelationGraph {
    pub(crate) graph: Option<RelationGraph>,
    pub(crate) row_node_ids: BTreeMap<OverviewRowId, RelationNodeId>,
}

/// Grouping, relation node mapping, and relation graph of one reviewed plan.
/// The review session prepares it once, so the single-environment Overview only projects rows
/// from it while the user filters, selects, expands, or resizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SingleEnvironmentOverview {
    groups: Vec<ChangeGroup>,
    repeated: usize,
    relations: RelationGraph,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OverviewRow {
    Individual(ComparisonRow),
    Group(OverviewGroup),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OverviewGroup {
    pub(crate) id: GroupId,
    pub(crate) display_address: String,
    pub(crate) has_unknown: bool,
    pub(crate) cells: Vec<GroupCell>,
    pub(crate) children: Vec<ComparisonRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum OverviewRowId {
    Group(GroupId),
    Individual(String),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GroupId(GroupingKey);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupCell {
    pub(crate) state: CellState,
    pub(crate) members: Vec<String>,
}

pub(crate) fn environment_overview_for_selection(
    plans: &[EnvironmentPlan],
    selection: &EnvironmentSelection,
) -> EnvironmentOverview {
    let comparison = compare_environments_for_selection(plans, selection);
    let candidates: Vec<BTreeMap<_, _>> = selection
        .indexes()
        .iter()
        .map(|index| {
            let plan = &plans[*index];
            let Some(review) = plan.review().map(ReviewSessionState::review) else {
                return BTreeMap::new();
            };
            review
                .plan()
                .resource_changes
                .iter()
                .filter_map(|change| {
                    Some((
                        change.address.as_str(),
                        grouping_candidate(change, review.provider_schemas())?,
                    ))
                })
                .collect()
        })
        .collect();
    let mut rows = Vec::new();
    let mut groups = BTreeMap::<GroupingKey, Vec<ComparisonRow>>::new();
    for row in comparison.rows {
        if let Some(candidate) = shared_candidate(&row, &candidates) {
            groups.entry(candidate.key.clone()).or_default().push(row);
        } else {
            rows.push(OverviewRow::Individual(row));
        }
    }
    for (key, mut children) in groups {
        children.sort_by(|a, b| a.address.cmp(&b.address));
        let cells = group_cells(&children, selection.indexes().len());
        if is_common_group(&cells) {
            let address =
                normalize_resource_addresses(children.iter().map(|row| row.address.as_str()))
                    .expect("grouping candidates share a normalized address");
            rows.push(OverviewRow::Group(OverviewGroup {
                id: GroupId(key),
                display_address: address.display().to_owned(),
                has_unknown: children.iter().any(|child| child.has_unknown),
                cells,
                children,
            }));
        } else {
            rows.extend(children.into_iter().map(OverviewRow::Individual));
        }
    }
    rows.sort_by(|left, right| row_order(left).cmp(&row_order(right)));
    EnvironmentOverview {
        scope: comparison.scope,
        rows,
    }
}

pub(crate) fn environment_overview_with_relations_for_selection(
    plans: &[EnvironmentPlan],
    selection: &EnvironmentSelection,
) -> EnvironmentOverviewWithRelations {
    let overview = environment_overview_for_selection(plans, selection);
    let selected_columns: BTreeMap<_, _> = selection
        .indexes()
        .iter()
        .enumerate()
        .map(|(column, index)| (*index, column))
        .collect();
    let relations = plans
        .iter()
        .enumerate()
        .map(|(environment, plan)| {
            let mut row_node_ids = BTreeMap::new();
            let Some(state) = plan.review() else {
                return (
                    environment,
                    EnvironmentRelationGraph {
                        graph: None,
                        row_node_ids,
                    },
                );
            };

            let graph = selected_columns.get(&environment).map_or_else(
                || state.prepared_overview().relations().clone(),
                |column| {
                    let review = state.review();
                    let node_inputs =
                        comparison_node_inputs(&overview, *column, review, &mut row_node_ids);
                    build_relation_graph(review.relations(), node_inputs)
                },
            );
            (
                environment,
                EnvironmentRelationGraph {
                    graph: Some(graph),
                    row_node_ids,
                },
            )
        })
        .collect();

    EnvironmentOverviewWithRelations {
        overview,
        relations,
    }
}

impl SingleEnvironmentOverview {
    // Only the review session builds it, once per review, so screens cannot rebuild it per frame.
    pub(in crate::app) fn new(review: &PlanReview) -> Self {
        let mut grouping =
            group_resource_changes(&review.plan().resource_changes, review.provider_schemas());
        let (node_inputs, group_node_ids) = grouped_plan_node_inputs(review, &grouping);
        for (group, node_id) in grouping.groups.iter_mut().zip(group_node_ids) {
            group.node_id = node_id;
        }
        Self {
            groups: grouping.groups,
            repeated: grouping.repeated,
            relations: build_relation_graph(review.relations(), node_inputs),
        }
    }

    pub(crate) fn groups(&self) -> &[ChangeGroup] {
        &self.groups
    }

    pub(crate) const fn repeated(&self) -> usize {
        self.repeated
    }

    pub(crate) const fn relations(&self) -> &RelationGraph {
        &self.relations
    }
}

fn comparison_node_inputs(
    overview: &EnvironmentOverview,
    column: usize,
    review: &PlanReview,
    row_node_ids: &mut BTreeMap<OverviewRowId, RelationNodeId>,
) -> Vec<RelationNode> {
    let changes: BTreeMap<_, _> = review
        .plan()
        .resource_changes
        .iter()
        .map(|change| (change.address.as_str(), change))
        .collect();
    let mut node_inputs = Vec::new();

    for row in &overview.rows {
        match row {
            OverviewRow::Individual(row) => {
                if !matches!(row.cells[column], CellState::Change { .. }) {
                    continue;
                }
                let addresses = [row.address.clone()];
                let input = relation_node_input(
                    &changes,
                    &addresses,
                    &row.address,
                    row.difference.is_some(),
                    false,
                );
                record_node(
                    input,
                    OverviewRowId::Individual(row.address.clone()),
                    &mut node_inputs,
                    row_node_ids,
                );
            }
            OverviewRow::Group(group) => {
                let cell = &group.cells[column];
                let has_unknown = cell
                    .members
                    .iter()
                    .filter_map(|address| changes.get(address.as_str()))
                    .any(|change| resource_has_unknown(change));
                // Compare lists every group under Same change; instance count gaps are not differences there.
                let input = relation_node_input(
                    &changes,
                    &cell.members,
                    &group.display_address,
                    false,
                    has_unknown,
                );
                record_node(
                    input,
                    OverviewRowId::Group(group.id.clone()),
                    &mut node_inputs,
                    row_node_ids,
                );
            }
        }
    }

    node_inputs
}

/// Returns the node inputs and, for each group, the node its changed members map to.
fn grouped_plan_node_inputs(
    review: &PlanReview,
    grouping: &PlanGrouping,
) -> (Vec<RelationNode>, Vec<Option<RelationNodeId>>) {
    let changes: BTreeMap<_, _> = review
        .plan()
        .resource_changes
        .iter()
        .map(|change| (change.address.as_str(), change))
        .collect();
    let mut node_inputs = Vec::new();
    let mut group_node_ids = Vec::with_capacity(grouping.groups.len());

    for group in &grouping.groups {
        let addresses: Vec<_> = group
            .members
            .iter()
            .filter(|change| change.kind != ResourceChangeKind::NoOp)
            .map(|change| change.address.clone())
            .collect();

        let node_id = if group.is_repeated() && addresses.len() > 1 {
            let input = relation_node_input(
                &changes,
                &addresses,
                &group.display_address,
                false,
                group.has_unknown,
            );
            push_node(input, &mut node_inputs)
        } else {
            let member_node_ids = addresses
                .iter()
                .map(|address| {
                    let input = relation_node_input(
                        &changes,
                        std::slice::from_ref(address),
                        address,
                        false,
                        false,
                    );
                    push_node(input, &mut node_inputs)
                })
                .collect::<Vec<_>>();
            match member_node_ids.as_slice() {
                [node_id] => node_id.clone(),
                _ => None,
            }
        };
        group_node_ids.push(node_id);
    }

    (node_inputs, group_node_ids)
}

fn relation_node_input(
    changes: &BTreeMap<&str, &ResourceChange>,
    addresses: &[String],
    display_address: &str,
    differs: bool,
    has_unknown: bool,
) -> Option<RelationNode> {
    let operation = changes.get(addresses.first()?.as_str())?.kind;
    if operation == ResourceChangeKind::NoOp
        || addresses.iter().any(|address| {
            changes
                .get(address.as_str())
                .is_none_or(|change| change.kind != operation)
        })
    {
        return None;
    }

    RelationNode::new(
        addresses.iter().cloned(),
        resource_display_address(display_address).unwrap_or_else(|| display_address.to_owned()),
        operation,
        addresses.len(),
        relation_breadcrumbs(addresses, display_address),
        differs,
        has_unknown,
    )
}

fn relation_breadcrumbs(addresses: &[String], display_address: &str) -> Vec<String> {
    let Some(mut display_breadcrumbs) = module_breadcrumbs(display_address) else {
        return Vec::new();
    };
    let Some(member_breadcrumbs) = addresses
        .iter()
        .map(|address| module_breadcrumbs(address))
        .collect::<Option<Vec<_>>>()
    else {
        return display_breadcrumbs;
    };
    let Some(first) = member_breadcrumbs.first() else {
        return display_breadcrumbs;
    };
    if first.len() != display_breadcrumbs.len()
        || member_breadcrumbs
            .iter()
            .any(|breadcrumbs| breadcrumbs.len() != first.len())
    {
        return display_breadcrumbs;
    }

    for (index, breadcrumb) in display_breadcrumbs.iter_mut().enumerate() {
        if member_breadcrumbs
            .iter()
            .all(|candidate| candidate[index] == first[index])
        {
            breadcrumb.clone_from(&first[index]);
        }
    }
    display_breadcrumbs
}

fn record_node(
    input: Option<RelationNode>,
    row_id: OverviewRowId,
    node_inputs: &mut Vec<RelationNode>,
    row_node_ids: &mut BTreeMap<OverviewRowId, RelationNodeId>,
) {
    if let Some(node_id) = push_node(input, node_inputs) {
        row_node_ids.insert(row_id, node_id);
    }
}

fn push_node(
    input: Option<RelationNode>,
    node_inputs: &mut Vec<RelationNode>,
) -> Option<RelationNodeId> {
    let input = input?;
    let node_id = input.id.clone();
    node_inputs.push(input);
    Some(node_id)
}

fn shared_candidate<'a>(
    row: &ComparisonRow,
    candidates: &'a [BTreeMap<&str, GroupingCandidate>],
) -> Option<&'a GroupingCandidate> {
    let mut shared: Option<&GroupingCandidate> = None;
    for (cell, candidates) in row.cells.iter().zip(candidates) {
        match cell {
            CellState::Change { .. } => {
                let candidate = candidates.get(row.address.as_str())?;
                if shared.is_some_and(|shared| shared.key != candidate.key) {
                    return None;
                }
                shared = Some(candidate);
            }
            CellState::NoOp => return None,
            CellState::Missing | CellState::Unavailable => {}
        }
    }
    shared
}

fn group_cells(children: &[ComparisonRow], environment_count: usize) -> Vec<GroupCell> {
    (0..environment_count)
        .map(|environment| {
            let members: Vec<_> = children
                .iter()
                .filter(|row| matches!(row.cells[environment], CellState::Change { .. }))
                .collect();
            let cell = &members.first().copied().unwrap_or(&children[0]).cells[environment];
            GroupCell {
                state: cell.clone(),
                members: members.iter().map(|row| row.address.clone()).collect(),
            }
        })
        .collect()
}

fn is_common_group(cells: &[GroupCell]) -> bool {
    cells
        .iter()
        .filter(|cell| cell.state != CellState::Unavailable)
        .all(|cell| !cell.members.is_empty())
        && cells.iter().any(|cell| cell.members.len() >= 2)
}

fn row_order(row: &OverviewRow) -> (bool, Option<DifferenceReason>, &str) {
    match row {
        OverviewRow::Individual(row) => (row.difference.is_none(), row.difference, &row.address),
        OverviewRow::Group(group) => (true, None, &group.display_address),
    }
}

impl Debug for GroupId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("GroupId(<opaque>)")
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeSet, path::PathBuf};

    use rstest::rstest;

    use super::*;
    use crate::app::{
        environments::{
            Environment, EnvironmentAvailability, EnvironmentIdentity, EnvironmentSession,
            PlanResult,
        },
        execution::Tool,
        plan::{
            AttributeType, Plan, PlanAction, PlanValue, ProviderSchema, ProviderSchemas,
            ResourceMode, ResourceSchema,
        },
        review::{PlanBlock, PlanBlockKind, PlanDocument, PlanMetadata},
    };

    fn environment_overview(plans: &[EnvironmentPlan]) -> EnvironmentOverview {
        let selection = EnvironmentSelection::new(None, plans.len())
            .expect("all environment indexes form a valid selection");
        environment_overview_for_selection(plans, &selection)
    }

    #[test]
    fn unequal_counts_group_patterns_and_restore_exact_address_comparisons() {
        let session = ready_session([20, 20, 200].map(|count| changes(count, "new")));

        let overview = environment_overview(session.plans());

        let group = only_group(&overview);
        assert_eq!(member_counts(group), vec![20, 20, 200]);
        assert_eq!(group.children.len(), 200);
        assert_eq!(group.display_address, "test_resource.item[*]");
        let extra = group
            .children
            .iter()
            .find(|row| row.address == "test_resource.item[199]")
            .unwrap();
        assert_eq!(extra.difference, Some(DifferenceReason::Missing));
        assert_eq!(extra.cells[0], CellState::Missing);
        assert_partition(&session, &overview);
    }

    #[test]
    fn groups_matching_unknown_changes_across_small_and_large_environment_counts() {
        let provider = "registry.example/provider".to_owned();
        let resource_type = "test_resource".to_owned();
        let schemas = ProviderSchemas {
            providers: BTreeMap::from([(
                provider.clone(),
                ProviderSchema {
                    resources: BTreeMap::from([(
                        resource_type.clone(),
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
        };
        for counts in [[2, 2, 4], [20, 20, 200]] {
            let mut session = pending_session(3);
            for count in counts {
                let changes = (0..count)
                    .map(|index| {
                        let mut change = change(&format!("test_resource.server[{index}]"), "new");
                        change.provider = Some(provider.clone());
                        change.resource_type = Some(resource_type.clone());
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
                        change
                    })
                    .collect();
                complete_next(
                    &mut session,
                    review(changes).with_provider_schemas(Some(schemas.clone())),
                );
            }

            let overview = environment_overview(session.plans());
            let group = only_group(&overview);
            assert_eq!(member_counts(group), counts);
            assert!(group.has_unknown);

            let selection = EnvironmentSelection::new(None, session.plans().len()).unwrap();
            let with_relations =
                environment_overview_with_relations_for_selection(session.plans(), &selection);
            for relation in with_relations.relations.values() {
                let graph = relation.graph.as_ref().unwrap();
                let node = graph
                    .nodes
                    .iter()
                    .find(|node| node.display_address == "test_resource.server[*]")
                    .unwrap();
                assert!(node.has_unknown);
                assert!(counts.contains(&node.change_count));
            }
        }
    }

    #[test]
    fn selected_environments_group_only_their_common_changes() {
        let session = ready_session([changes(2, "excluded"), changes(2, "new"), changes(3, "new")]);
        let all = environment_overview(session.plans());
        let selection = EnvironmentSelection::new(Some(vec![2, 1]), session.plans().len()).unwrap();

        let overview = environment_overview_for_selection(session.plans(), &selection);

        assert!(
            all.rows
                .iter()
                .all(|row| matches!(row, OverviewRow::Individual(_)))
        );
        let group = only_group(&overview);
        assert_eq!(member_counts(group), [2, 3]);
        assert_eq!(group.children.len(), 3);
    }

    #[test]
    fn common_changes_keep_the_exception_individual() {
        let mut resources = changes(200, "new");
        resources.push(change("test_resource.item[200]", "exception"));
        let session = ready_session([resources.clone(), resources]);

        let overview = environment_overview(session.plans());

        assert_eq!(overview.rows.len(), 2);
        assert!(overview.rows.iter().any(|row| matches!(row,
            OverviewRow::Group(group) if member_counts(group) == [200, 200]
        )));
        assert!(overview.rows.iter().any(|row| matches!(row,
            OverviewRow::Individual(row) if row.address == "test_resource.item[200]"
        )));
        assert_partition(&session, &overview);
    }

    #[test]
    fn conflicting_full_addresses_remain_individual_in_every_environment() {
        let mut action = change("test_resource.item[0]", "new");
        action.actions = vec![PlanAction::Delete, PlanAction::Create];
        action.kind = ResourceChangeKind::Replace;
        let mut no_op = change("test_resource.item[0]", "old");
        no_op.actions = vec![PlanAction::NoOp];
        no_op.kind = ResourceChangeKind::NoOp;
        let mut attrs = change("test_resource.item[0]", "new");
        attrs.after = Some(PlanValue::Object(BTreeMap::from([
            ("name".to_owned(), PlanValue::String("new".to_owned())),
            ("extra".to_owned(), PlanValue::Bool(true)),
        ])));
        for (name, conflict, reason) in [
            (
                "value",
                change("test_resource.item[0]", "other"),
                DifferenceReason::Value,
            ),
            ("action", action, DifferenceReason::Action),
            ("no-op", no_op, DifferenceReason::Action),
            ("attrs", attrs, DifferenceReason::Attrs),
        ] {
            let mut right = changes(3, "new");
            right[0] = conflict;
            let session = ready_session([changes(3, "new"), right]);

            let overview = environment_overview(session.plans());

            assert_eq!(overview.rows.len(), 2, "{name}");
            let OverviewRow::Individual(row) = &overview.rows[0] else {
                panic!("conflict must stay individual: {name}");
            };
            assert_eq!(row.address, "test_resource.item[0]", "{name}");
            assert_eq!(row.difference, Some(reason), "{name}");
            assert!(
                matches!(&overview.rows[1], OverviewRow::Group(group)
                if member_counts(group) == [2, 2]),
                "{name}"
            );
            assert_partition(&session, &overview);
        }
    }

    #[test]
    fn missing_reason_does_not_hide_value_conflicts_between_present_environments() {
        let session = ready_session([
            changes(3, "new"),
            vec![
                change("test_resource.item[0]", "different"),
                change("test_resource.item[1]", "new"),
            ],
            vec![change("test_resource.item[1]", "new")],
        ]);

        let overview = environment_overview(session.plans());

        assert!(matches!(&overview.rows[0], OverviewRow::Individual(row)
            if row.address == "test_resource.item[0]" && row.difference == Some(DifferenceReason::Missing)));
        assert!(matches!(&overview.rows[1], OverviewRow::Group(group)
            if member_counts(group) == [2, 1, 1]));
        assert_partition(&session, &overview);
    }

    #[test]
    fn presence_without_a_change_record_prevents_grouping_that_address() {
        let mut session = pending_session(2);
        complete_next(&mut session, review(changes(3, "new")));
        let mut plan = Plan::empty();
        plan.resource_changes = changes(3, "new").into_iter().skip(1).collect();
        plan.value_addresses
            .insert("test_resource.item[0]".to_owned());
        complete_next(&mut session, plan_review(plan));

        let overview = environment_overview(session.plans());

        assert!(matches!(&overview.rows[0], OverviewRow::Individual(row)
            if row.cells[1] == CellState::NoOp));
        assert_partition(&session, &overview);
    }

    #[test]
    fn module_keys_are_grouped_without_inventing_individual_matches() {
        let session = ready_session([
            vec![
                change(r#"module.app["dev"].test_resource.item[0]"#, "new"),
                change(r#"module.app["dev"].test_resource.item[1]"#, "new"),
            ],
            vec![change(r#"module.app["prod"].test_resource.item[9]"#, "new")],
        ]);

        let overview = environment_overview(session.plans());

        let group = only_group(&overview);
        assert_eq!(group.display_address, "module.app[*].test_resource.item[*]");
        assert_eq!(member_counts(group), vec![2, 1]);
        assert!(
            group
                .children
                .iter()
                .all(|row| row.difference == Some(DifferenceReason::Missing))
        );
        assert_eq!(
            group.cells[1].members,
            [r#"module.app["prod"].test_resource.item[9]"#]
        );

        let selection = EnvironmentSelection::new(None, session.plans().len()).unwrap();
        let result = environment_overview_with_relations_for_selection(session.plans(), &selection);
        for (environment, module_key, address_count) in [(0, "dev", 2), (1, "prod", 1)] {
            let graph = result.relations[&environment]
                .graph
                .as_ref()
                .expect("module environments are ready");
            assert_eq!(graph.nodes.len(), 1);
            assert_eq!(graph.nodes[0].id.addresses().len(), address_count);
            assert_eq!(graph.nodes[0].display_address, "test_resource.item[*]");
            assert_eq!(
                graph.nodes[0].breadcrumbs,
                [format!("app[\"{module_key}\"]")]
            );
        }
        assert_partition(&session, &overview);
    }

    #[test]
    fn group_display_retains_key_positions_from_every_member() {
        struct Case {
            name: &'static str,
            left: &'static str,
            right: [&'static str; 2],
            display: &'static str,
        }

        for case in [
            Case {
                name: "unkeyed_and_keyed",
                left: "test_resource.item",
                right: ["test_resource.item[0]", "test_resource.item[1]"],
                display: "test_resource.item[*]",
            },
            Case {
                name: "module_and_resource_keys",
                left: r#"module.app["dev"].test_resource.item"#,
                right: [
                    "module.app.test_resource.item[0]",
                    "module.app.test_resource.item[1]",
                ],
                display: "module.app[*].test_resource.item[*]",
            },
        ] {
            let session = ready_session([
                vec![change(case.left, "new")],
                case.right.map(|address| change(address, "new")).to_vec(),
            ]);

            let overview = environment_overview(session.plans());

            let group = only_group(&overview);
            assert_eq!(group.display_address, case.display, "case: {}", case.name);
            assert_eq!(member_counts(group), [1, 2], "case: {}", case.name);
            assert_partition(&session, &overview);
        }
    }

    #[rstest]
    #[case::one_each([1, 1])]
    #[case::only_one_environment([2, 0])]
    fn insufficient_members_stay_individual(#[case] counts: [usize; 2]) {
        let session = ready_session(counts.map(|count| changes(count, "new")));

        let overview = environment_overview(session.plans());

        assert!(
            overview
                .rows
                .iter()
                .all(|row| matches!(row, OverviewRow::Individual(_)))
        );
        assert_partition(&session, &overview);
    }

    #[test]
    fn multiple_patterns_have_distinct_stable_opaque_ids() {
        let mut resources = changes(2, "synthetic-private-pattern");
        resources.extend([
            change("test_resource.item[2]", "other-pattern"),
            change("test_resource.item[3]", "other-pattern"),
        ]);
        let session = ready_session([resources.clone(), resources.clone()]);
        let overview = environment_overview(session.plans());
        let ids: BTreeSet<_> = overview
            .rows
            .iter()
            .map(|row| match row {
                OverviewRow::Group(group) => group.id.clone(),
                OverviewRow::Individual(_) => panic!("both patterns should repeat"),
            })
            .collect();
        resources.reverse();
        let reordered = ready_session([resources.clone(), resources]);

        assert_eq!(ids.len(), 2);
        assert_eq!(overview, environment_overview(reordered.plans()));
        assert!(!format!("{overview:?}").contains("synthetic-private-pattern"));
        assert!(!format!("{overview:?}").contains("other-pattern"));
        assert_partition(&session, &overview);
    }

    #[rstest]
    #[case::unknown(true)]
    #[case::sensitive(false)]
    fn ungroupable_markers_keep_even_equal_changes_individual(#[case] unknown: bool) {
        let mut marked = changes(2, "synthetic-secret");
        for change in &mut marked {
            if unknown {
                change.after_unknown = Some(PlanValue::Bool(true));
            } else {
                change.after_sensitive = Some(PlanValue::Bool(true));
            }
        }
        let session = ready_session([marked.clone(), marked]);

        let overview = environment_overview(session.plans());

        assert_eq!(overview.rows.len(), 2);
        assert!(
            overview
                .rows
                .iter()
                .all(|row| matches!(row, OverviewRow::Individual(row) if row.difference.is_none()))
        );
        assert!(!format!("{overview:?}").contains("synthetic-secret"));
        assert_partition(&session, &overview);
    }

    #[test]
    fn sensitivity_in_one_environment_excludes_the_address_everywhere() {
        let left = changes(3, "new");
        let mut right = left.clone();
        right[0].after_sensitive = Some(PlanValue::Bool(true));
        let session = ready_session([left, right]);

        let overview = environment_overview(session.plans());

        assert!(overview.rows.iter().any(|row| matches!(row,
            OverviewRow::Individual(row) if row.address == "test_resource.item[0]" && row.difference.is_none()
        )));
        assert!(overview.rows.iter().any(|row| matches!(row,
            OverviewRow::Group(group) if member_counts(group) == [2, 2]
        )));
        assert_partition(&session, &overview);
    }

    #[test]
    fn ready_additions_rebuild_groups_and_keep_unavailable_cells_distinct() {
        let mut session = pending_session(3);
        let waiting = environment_overview(session.plans());
        assert_eq!(waiting.scope, ComparisonScope::Waiting);
        assert!(waiting.rows.is_empty());
        complete_next(&mut session, review(changes(2, "new")));
        let partial = environment_overview(session.plans());
        let group = only_group(&partial);
        assert_eq!(member_counts(group), vec![2, 0, 0]);
        assert_eq!(group.cells[1].state, CellState::Unavailable);
        let original_id = group.id.clone();
        assert_eq!(partial.scope, ComparisonScope::Partial);
        let run = session.start_next().unwrap();
        assert_eq!(partial, environment_overview(session.plans()));
        session.complete(
            run,
            PlanResult::Error("synthetic failure".to_owned()),
            Vec::new(),
        );
        assert_eq!(partial, environment_overview(session.plans()));
        assert!(session.retry(1));
        complete_next(&mut session, review(changes(3, "new")));
        let added = environment_overview(session.plans());
        assert_eq!(only_group(&added).id, original_id);
        assert_eq!(member_counts(only_group(&added)), vec![2, 3, 0]);
        assert_partition(&session, &added);

        complete_next(&mut session, review(changes(1, "different")));
        let complete = environment_overview(session.plans());

        assert_eq!(complete.scope, ComparisonScope::All);
        assert!(
            complete
                .rows
                .iter()
                .all(|row| matches!(row, OverviewRow::Individual(_)))
        );
        assert_partition(&session, &complete);
    }

    #[test]
    fn relation_graphs_use_each_compared_groups_changed_addresses() {
        let session = ready_session([changes(2, "new"), changes(3, "new")]);
        let selection = EnvironmentSelection::new(None, session.plans().len()).unwrap();

        let result = environment_overview_with_relations_for_selection(session.plans(), &selection);

        let group = only_group(&result.overview);
        let group_id = OverviewRowId::Group(group.id.clone());
        for (environment, expected_count) in [(0, 2), (1, 3)] {
            let relation = &result.relations[&environment];
            let graph = relation
                .graph
                .as_ref()
                .expect("ready environments have a graph");
            assert_eq!(graph.nodes.len(), 1);
            let node = &graph.nodes[0];
            assert_eq!(node.change_count, expected_count);
            assert_eq!(node.id.addresses().len(), expected_count);
            assert!(!node.differs);
            assert_eq!(relation.row_node_ids.get(&group_id), Some(&node.id));
            assert_eq!(relation.row_node_ids.len(), 1);
            assert!(group.children.iter().all(|child| {
                !relation
                    .row_node_ids
                    .contains_key(&OverviewRowId::Individual(child.address.clone()))
            }));
        }
    }

    #[test]
    fn excluded_environment_gets_its_plan_groups_without_comparison_highlights() {
        let session = ready_session([changes(2, "new"), changes(3, "other")]);
        let selection = EnvironmentSelection::new(Some(vec![0]), session.plans().len()).unwrap();

        let result = environment_overview_with_relations_for_selection(session.plans(), &selection);

        only_group(&result.overview);
        let included = result.relations[&0]
            .graph
            .as_ref()
            .expect("the compared environment is ready");
        assert_eq!(included.nodes.len(), 1);
        assert_eq!(included.nodes[0].id.addresses().len(), 2);
        assert!(!included.nodes[0].differs);

        let excluded_relation = &result.relations[&1];
        let excluded = excluded_relation
            .graph
            .as_ref()
            .expect("excluded ready environments still get a graph");
        assert_eq!(excluded.nodes.len(), 1);
        assert_eq!(excluded.nodes[0].id.addresses().len(), 3);
        assert!(!excluded.nodes[0].differs);
        assert!(excluded_relation.row_node_ids.is_empty());
    }

    #[test]
    fn partial_ready_and_retry_rebuild_only_available_environment_graphs() {
        let mut session = pending_session(2);
        complete_next(&mut session, review(changes(2, "new")));
        let selection = EnvironmentSelection::new(None, session.plans().len()).unwrap();

        let partial =
            environment_overview_with_relations_for_selection(session.plans(), &selection);

        assert_eq!(partial.overview.scope, ComparisonScope::Partial);
        assert!(partial.relations[&0].graph.is_some());
        assert!(partial.relations[&1].graph.is_none());
        assert!(partial.relations[&1].row_node_ids.is_empty());

        let run = session.start_next().unwrap();
        assert_eq!(run, 1);
        assert!(session.complete(
            run,
            PlanResult::Error("synthetic retry".to_owned()),
            Vec::new()
        ));
        assert!(session.retry(1));
        complete_next(&mut session, review(changes(3, "different")));

        let retried =
            environment_overview_with_relations_for_selection(session.plans(), &selection);

        assert!(matches!(retried.overview.scope, ComparisonScope::All));
        let graph = retried.relations[&1]
            .graph
            .as_ref()
            .expect("a completed retry rebuilds its graph");
        assert_eq!(graph.nodes.len(), 3);
        assert!(graph.nodes.iter().all(|node| node.differs));
        for address in [
            "test_resource.item[0]",
            "test_resource.item[1]",
            "test_resource.item[2]",
        ] {
            let node_id = retried.relations[&1]
                .row_node_ids
                .get(&OverviewRowId::Individual(address.to_owned()))
                .expect("changed individual rows map to a graph node");
            assert_eq!(node_id.addresses(), [address]);
        }
    }

    #[test]
    fn single_environment_group_maps_every_member_to_one_complete_node() {
        let changes = changes(3, "new");
        let review = review(changes.clone());

        let overview = SingleEnvironmentOverview::new(&review);

        let graph = overview.relations();
        assert_eq!(graph.nodes.len(), 1);
        let node = &graph.nodes[0];
        assert_eq!(node.id.addresses().len(), 3);
        assert_eq!(overview.repeated(), 3);
        let [group] = overview.groups() else {
            panic!("repeated changes form one group");
        };
        assert_eq!(group.node_id.as_ref(), Some(&node.id));
        assert_eq!(
            group
                .members
                .iter()
                .map(|member| member.address.as_str())
                .collect::<Vec<_>>(),
            changes
                .iter()
                .map(|change| change.address.as_str())
                .collect::<Vec<_>>()
        );
    }

    fn only_group(overview: &EnvironmentOverview) -> &OverviewGroup {
        assert_eq!(overview.rows.len(), 1);
        let OverviewRow::Group(group) = &overview.rows[0] else {
            panic!("expected a common group");
        };
        group
    }

    fn member_counts(group: &OverviewGroup) -> Vec<usize> {
        group.cells.iter().map(|cell| cell.members.len()).collect()
    }

    fn assert_partition(session: &EnvironmentSession, overview: &EnvironmentOverview) {
        let selection = EnvironmentSelection::new(None, session.plans().len()).unwrap();
        let comparison = compare_environments_for_selection(session.plans(), &selection);
        assert_eq!(overview.scope, comparison.scope);
        let mut expanded = Vec::new();
        for row in &overview.rows {
            match row {
                OverviewRow::Individual(row) => expanded.push(row.clone()),
                OverviewRow::Group(group) => {
                    expanded.extend(group.children.clone());
                    for (environment, cell) in group.cells.iter().enumerate() {
                        let changes: Vec<_> = group
                            .children
                            .iter()
                            .filter(|row| {
                                matches!(row.cells[environment], CellState::Change { .. })
                            })
                            .collect();
                        assert_eq!(
                            cell.members,
                            changes
                                .iter()
                                .map(|row| row.address.clone())
                                .collect::<Vec<_>>()
                        );
                    }
                }
            }
        }
        assert_eq!(expanded.len(), comparison.rows.len());
        let by_address = |rows: Vec<ComparisonRow>| {
            rows.into_iter()
                .map(|row| (row.address.clone(), row))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(by_address(expanded), by_address(comparison.rows));
    }

    fn pending_session(count: usize) -> EnvironmentSession {
        EnvironmentSession::new(
            (0..count)
                .map(|index| Environment {
                    tool: Tool::Terraform,
                    availability: EnvironmentAvailability::Available(EnvironmentIdentity {
                        directory: PathBuf::from(format!("/synthetic/env{index:02}")),
                        workspace: "default".to_owned(),
                    }),
                })
                .collect(),
            false,
        )
    }

    fn ready_session<const N: usize>(changes: [Vec<ResourceChange>; N]) -> EnvironmentSession {
        let mut session = pending_session(N);
        for changes in changes {
            complete_next(&mut session, review(changes));
        }
        session
    }

    fn complete_next(session: &mut EnvironmentSession, review: PlanReview) {
        let run = session.start_next().unwrap();
        assert!(session.complete(
            run,
            PlanResult::Ready {
                review: Box::new(review),
                changed: true
            },
            Vec::new()
        ));
    }

    fn review(changes: Vec<ResourceChange>) -> PlanReview {
        plan_review(Plan {
            resource_changes: changes,
            ..Plan::empty()
        })
    }

    fn plan_review(plan: Plan) -> PlanReview {
        let mut addresses: Vec<_> = plan
            .resource_changes
            .iter()
            .map(|change| change.address.clone())
            .collect();
        addresses.sort();
        let document = PlanDocument::with_blocks_and_line_kinds(
            addresses.iter().map(|_| "safe plan\n").collect(),
            addresses
                .into_iter()
                .enumerate()
                .map(|(line, address)| {
                    PlanBlock::with_addresses(
                        line..line + 1,
                        PlanBlockKind::Resource,
                        vec![address],
                    )
                })
                .collect(),
            Vec::new(),
        );
        PlanReview::new(
            PathBuf::from("/synthetic"),
            "default".to_owned(),
            document,
            plan,
            PlanMetadata::new(true),
            Vec::new(),
        )
    }

    fn changes(count: usize, after: &str) -> Vec<ResourceChange> {
        (0..count)
            .map(|index| change(&format!("test_resource.item[{index}]"), after))
            .collect()
    }

    fn change(address: &str, after: &str) -> ResourceChange {
        let attributes = |name: &str| {
            PlanValue::Object(BTreeMap::from([(
                "name".to_owned(),
                PlanValue::String(name.to_owned()),
            )]))
        };
        ResourceChange {
            address: address.to_owned(),
            provider: None,
            resource_type: None,
            mode: ResourceMode::Managed,
            actions: vec![PlanAction::Update],
            kind: ResourceChangeKind::Update,
            before: Some(attributes("old")),
            after: Some(attributes(after)),
            before_sensitive: None,
            after_sensitive: None,
            after_unknown: None,
            previous_address: None,
            importing: None,
        }
    }
}
