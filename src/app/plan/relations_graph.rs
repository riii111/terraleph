use std::collections::{BTreeMap, BTreeSet};

use super::{
    ConfigurationRelationStatus, PlanRelations, RelationEndpoint, RelationEvidence, RelationSource,
    RelationUnresolvedReason, ResourceChangeKind, StateRelationStatus,
    path::{normalize_resource_address, resource_address_matches_block},
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RelationNodeId(Vec<String>);

impl RelationNodeId {
    #[must_use]
    pub(crate) fn from_addresses(addresses: impl IntoIterator<Item = String>) -> Option<Self> {
        let addresses = addresses.into_iter().collect::<BTreeSet<_>>();
        (!addresses.is_empty()).then(|| Self(addresses.into_iter().collect()))
    }

    #[must_use]
    pub(crate) fn addresses(&self) -> &[String] {
        &self.0
    }
}

impl RelationNode {
    #[must_use]
    pub(crate) fn new(
        full_addresses: impl IntoIterator<Item = String>,
        display_address: String,
        operation: ResourceChangeKind,
        change_count: usize,
        breadcrumbs: Vec<String>,
        differs: bool,
        has_unknown: bool,
    ) -> Option<Self> {
        Some(Self {
            id: RelationNodeId::from_addresses(full_addresses)?,
            display_address,
            operation,
            change_count,
            breadcrumbs,
            differs,
            has_unknown,
            unresolved: BTreeSet::new(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RelationGraphLinkKind {
    Dotted,
    Solid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelationGraphLink {
    pub(crate) from: RelationNodeId,
    pub(crate) to: RelationNodeId,
    pub(crate) kind: RelationGraphLinkKind,
    pub(crate) sources: BTreeSet<RelationSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelationNode {
    pub(crate) id: RelationNodeId,
    pub(crate) display_address: String,
    pub(crate) operation: ResourceChangeKind,
    pub(crate) change_count: usize,
    pub(crate) breadcrumbs: Vec<String>,
    pub(crate) differs: bool,
    pub(crate) has_unknown: bool,
    pub(crate) unresolved: BTreeSet<RelationUnresolvedReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelationGraphGroup {
    pub(crate) nodes: Vec<RelationNodeId>,
    pub(crate) contains_destructive_change: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelationGraph {
    pub(crate) nodes: Vec<RelationNode>,
    pub(crate) links: Vec<RelationGraphLink>,
    pub(crate) connected_groups: Vec<RelationGraphGroup>,
    pub(crate) links_unknown: Vec<RelationNodeId>,
    pub(crate) no_links_shown: Vec<RelationNodeId>,
}

pub(in crate::app) fn build_relation_graph(
    relations: &PlanRelations,
    nodes: Vec<RelationNode>,
) -> RelationGraph {
    let mut nodes = nodes
        .into_iter()
        .map(|node| (node.id.clone(), node))
        .collect::<BTreeMap<_, _>>();
    let address_index = RelationAddressIndex::new(&nodes);
    let mut links = BTreeMap::<(RelationNodeId, RelationNodeId), RelationGraphLink>::new();

    apply_configuration_status(relations.configuration_status, nodes.values_mut());
    apply_state_status(relations.state_status, nodes.values_mut());
    add_evidence(
        &relations.configuration,
        &address_index,
        &mut nodes,
        &mut links,
    );
    add_evidence(&relations.state, &address_index, &mut nodes, &mut links);

    let links = links.into_values().collect::<Vec<_>>();
    let connected_nodes = links
        .iter()
        .flat_map(|link| [link.from.clone(), link.to.clone()])
        .collect::<BTreeSet<_>>();
    let connected_groups = connected_groups(&nodes, &links);
    let links_unknown = nodes
        .values()
        .filter(|node| !connected_nodes.contains(&node.id) && !node.unresolved.is_empty())
        .map(|node| node.id.clone())
        .collect();
    let no_links_shown = nodes
        .values()
        .filter(|node| !connected_nodes.contains(&node.id) && node.unresolved.is_empty())
        .map(|node| node.id.clone())
        .collect();

    RelationGraph {
        nodes: nodes.into_values().collect(),
        links,
        connected_groups,
        links_unknown,
        no_links_shown,
    }
}

fn add_evidence(
    evidence: &[RelationEvidence],
    address_index: &RelationAddressIndex,
    nodes: &mut BTreeMap<RelationNodeId, RelationNode>,
    links: &mut BTreeMap<(RelationNodeId, RelationNodeId), RelationGraphLink>,
) {
    for evidence in evidence {
        let dependents = address_index.nodes_for(&evidence.dependent);
        if let Some(reason) = evidence.unresolved {
            for dependent in &dependents {
                if let Some(node) = nodes.get_mut(dependent) {
                    node.unresolved.insert(reason);
                }
            }
        }

        let Some(referenced) = &evidence.referenced else {
            continue;
        };
        let referenced_nodes = address_index.nodes_for(referenced);
        let kind = if evidence.dependent.is_instance() && referenced.is_instance() {
            RelationGraphLinkKind::Solid
        } else {
            RelationGraphLinkKind::Dotted
        };
        for dependent in &dependents {
            for referenced in &referenced_nodes {
                if dependent == referenced {
                    continue;
                }
                let key = (referenced.clone(), dependent.clone());
                let link = links.entry(key).or_insert_with(|| RelationGraphLink {
                    from: referenced.clone(),
                    to: dependent.clone(),
                    kind,
                    sources: BTreeSet::new(),
                });
                if kind == RelationGraphLinkKind::Solid {
                    link.kind = RelationGraphLinkKind::Solid;
                }
                link.sources.insert(evidence.source);
            }
        }
    }
}

fn apply_configuration_status<'a>(
    status: ConfigurationRelationStatus,
    nodes: impl Iterator<Item = &'a mut RelationNode>,
) {
    let reason = match status {
        ConfigurationRelationStatus::NotCollected => {
            Some(RelationUnresolvedReason::ConfigurationNotCollected)
        }
        ConfigurationRelationStatus::Partial => {
            Some(RelationUnresolvedReason::ConfigurationPartial)
        }
        ConfigurationRelationStatus::Unavailable => {
            Some(RelationUnresolvedReason::ConfigurationUnavailable)
        }
        ConfigurationRelationStatus::Available => None,
    };
    if let Some(reason) = reason {
        for node in nodes {
            node.unresolved.insert(reason);
        }
    }
}

fn apply_state_status<'a>(
    status: StateRelationStatus,
    nodes: impl Iterator<Item = &'a mut RelationNode>,
) {
    let reason = match status {
        StateRelationStatus::NotCollected => Some(RelationUnresolvedReason::StateNotCollected),
        StateRelationStatus::Unavailable => Some(RelationUnresolvedReason::StateUnavailable),
        StateRelationStatus::Available | StateRelationStatus::NoPriorState => None,
    };
    if let Some(reason) = reason {
        for node in nodes {
            node.unresolved.insert(reason);
        }
    }
}

fn connected_groups(
    nodes: &BTreeMap<RelationNodeId, RelationNode>,
    links: &[RelationGraphLink],
) -> Vec<RelationGraphGroup> {
    let mut adjacency = BTreeMap::<RelationNodeId, BTreeSet<RelationNodeId>>::new();
    for link in links {
        adjacency
            .entry(link.from.clone())
            .or_default()
            .insert(link.to.clone());
        adjacency
            .entry(link.to.clone())
            .or_default()
            .insert(link.from.clone());
    }

    let mut visited = BTreeSet::new();
    let mut groups = Vec::new();
    for start in adjacency.keys() {
        if !visited.insert(start.clone()) {
            continue;
        }
        let mut pending = vec![start.clone()];
        let mut group_nodes = BTreeSet::new();
        while let Some(current) = pending.pop() {
            group_nodes.insert(current.clone());
            if let Some(neighbors) = adjacency.get(&current) {
                for neighbor in neighbors {
                    if visited.insert(neighbor.clone()) {
                        pending.push(neighbor.clone());
                    }
                }
            }
        }
        let group_nodes = group_nodes.into_iter().collect::<Vec<_>>();
        let contains_destructive_change = group_nodes.iter().any(|id| {
            nodes.get(id).is_some_and(|node| {
                matches!(
                    node.operation,
                    ResourceChangeKind::Delete | ResourceChangeKind::Replace
                )
            })
        });
        groups.push(RelationGraphGroup {
            nodes: group_nodes,
            contains_destructive_change,
        });
    }

    groups.sort_by(|left, right| {
        right
            .contains_destructive_change
            .cmp(&left.contains_destructive_change)
            .then_with(|| left.nodes.cmp(&right.nodes))
    });
    groups
}

struct RelationAddressIndex {
    instances: BTreeMap<String, BTreeSet<RelationNodeId>>,
    blocks: BTreeMap<String, BTreeMap<RelationNodeId, BTreeSet<String>>>,
}

impl RelationAddressIndex {
    fn new(nodes: &BTreeMap<RelationNodeId, RelationNode>) -> Self {
        let mut index = Self {
            instances: BTreeMap::new(),
            blocks: BTreeMap::new(),
        };
        for id in nodes.keys() {
            for address in id.addresses() {
                index
                    .instances
                    .entry(address.clone())
                    .or_default()
                    .insert(id.clone());
                if let Some(normalized) = normalize_resource_address(address) {
                    index
                        .blocks
                        .entry(normalized.normalized().to_owned())
                        .or_default()
                        .entry(id.clone())
                        .or_default()
                        .insert(address.clone());
                }
            }
        }
        index
    }

    fn nodes_for(&self, endpoint: &RelationEndpoint) -> Vec<RelationNodeId> {
        let index = match endpoint {
            RelationEndpoint::Instance(_) => &self.instances,
            RelationEndpoint::Block(address) => {
                let Some(normalized) = normalize_resource_address(address) else {
                    return Vec::new();
                };
                return self
                    .blocks
                    .get(normalized.normalized())
                    .into_iter()
                    .flat_map(|candidates| candidates.iter())
                    .filter(|(_, addresses)| {
                        addresses
                            .iter()
                            .any(|candidate| resource_address_matches_block(address, candidate))
                    })
                    .map(|(id, _)| id.clone())
                    .collect();
            }
        };
        index
            .get(endpoint.address())
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(
        addresses: &[&str],
        display_address: &str,
        operation: ResourceChangeKind,
    ) -> RelationNode {
        RelationNode::new(
            addresses.iter().map(|address| (*address).to_owned()),
            display_address.to_owned(),
            operation,
            addresses.len(),
            Vec::new(),
            false,
            false,
        )
        .expect("non-empty addresses should create a node")
    }

    fn individual_node(address: &str, operation: ResourceChangeKind) -> RelationNode {
        node(&[address], address, operation)
    }

    fn id(address: &str) -> RelationNodeId {
        RelationNodeId::from_addresses([address.to_owned()]).expect("address should create an id")
    }

    fn relation(
        dependent: RelationEndpoint,
        referenced: RelationEndpoint,
        source: RelationSource,
    ) -> RelationEvidence {
        RelationEvidence::resolved(dependent, referenced, source)
    }

    fn graph(
        configuration_status: ConfigurationRelationStatus,
        configuration: Vec<RelationEvidence>,
        state_status: StateRelationStatus,
        state: Vec<RelationEvidence>,
        nodes: &[RelationNode],
    ) -> RelationGraph {
        build_relation_graph(
            &PlanRelations {
                configuration_status,
                configuration,
                state_status,
                state,
            },
            nodes.to_vec(),
        )
    }

    #[test]
    fn node_input_sorts_and_deduplicates_addresses_and_rejects_empty_sets() {
        let input = RelationNode::new(
            [
                "aws_instance.z[0]".to_owned(),
                "aws_instance.a".to_owned(),
                "aws_instance.z[0]".to_owned(),
            ],
            "aws_instance.node".to_owned(),
            ResourceChangeKind::Update,
            2,
            Vec::new(),
            false,
            false,
        )
        .expect("addresses should create a node");

        assert_eq!(
            input.id.addresses(),
            &["aws_instance.a".to_owned(), "aws_instance.z[0]".to_owned()]
        );
        assert!(
            RelationNode::new(
                Vec::new(),
                "aws_instance.empty".to_owned(),
                ResourceChangeKind::Update,
                0,
                Vec::new(),
                false,
                false,
            )
            .is_none()
        );
    }

    #[test]
    fn builds_stable_directed_groups_and_merges_link_evidence() {
        let nodes = [
            individual_node("aws_instance.web", ResourceChangeKind::Delete),
            individual_node("aws_security_group.web", ResourceChangeKind::Update),
            individual_node("aws_route53_record.unrelated", ResourceChangeKind::Create),
            individual_node("aws_s3_bucket.logs", ResourceChangeKind::Replace),
            individual_node("aws_kms_key.logs", ResourceChangeKind::Update),
            individual_node("aws_acm_certificate.issued", ResourceChangeKind::Create),
            individual_node(
                "aws_acm_certificate_validation.issued",
                ResourceChangeKind::Update,
            ),
        ];
        let graph = graph(
            ConfigurationRelationStatus::Available,
            vec![
                relation(
                    RelationEndpoint::Instance("aws_instance.web".to_owned()),
                    RelationEndpoint::Instance("aws_security_group.web".to_owned()),
                    RelationSource::Configuration,
                ),
                relation(
                    RelationEndpoint::Instance("aws_instance.web".to_owned()),
                    RelationEndpoint::Instance("aws_security_group.web".to_owned()),
                    RelationSource::State,
                ),
                relation(
                    RelationEndpoint::Instance("aws_security_group.web".to_owned()),
                    RelationEndpoint::Instance("aws_instance.web".to_owned()),
                    RelationSource::Configuration,
                ),
                relation(
                    RelationEndpoint::Instance("aws_kms_key.logs".to_owned()),
                    RelationEndpoint::Instance("aws_s3_bucket.logs".to_owned()),
                    RelationSource::Configuration,
                ),
                relation(
                    RelationEndpoint::Instance("aws_acm_certificate_validation.issued".to_owned()),
                    RelationEndpoint::Instance("aws_acm_certificate.issued".to_owned()),
                    RelationSource::Configuration,
                ),
            ],
            StateRelationStatus::NoPriorState,
            Vec::new(),
            &nodes,
        );

        assert_eq!(graph.links.len(), 4);
        assert_eq!(graph.links[3].from, id("aws_security_group.web"));
        assert_eq!(graph.links[3].to, id("aws_instance.web"));
        assert_eq!(graph.links[3].kind, RelationGraphLinkKind::Solid);
        assert_eq!(
            graph.links[3].sources,
            BTreeSet::from([RelationSource::Configuration, RelationSource::State])
        );
        assert_eq!(
            graph.connected_groups,
            vec![
                RelationGraphGroup {
                    nodes: vec![id("aws_instance.web"), id("aws_security_group.web")],
                    contains_destructive_change: true,
                },
                RelationGraphGroup {
                    nodes: vec![id("aws_kms_key.logs"), id("aws_s3_bucket.logs")],
                    contains_destructive_change: true,
                },
                RelationGraphGroup {
                    nodes: vec![
                        id("aws_acm_certificate.issued"),
                        id("aws_acm_certificate_validation.issued"),
                    ],
                    contains_destructive_change: false,
                },
            ]
        );
        assert_eq!(
            graph.no_links_shown,
            vec![id("aws_route53_record.unrelated")]
        );
    }

    #[test]
    fn creates_a_link_when_any_member_of_an_aggregate_has_evidence() {
        let aggregate = RelationNode::new(
            [
                "aws_instance.web[1]".to_owned(),
                "aws_instance.web[0]".to_owned(),
            ],
            "aws_instance.web[*]".to_owned(),
            ResourceChangeKind::Update,
            2,
            vec!["app".to_owned()],
            true,
            true,
        )
        .expect("aggregate addresses should create a node");
        let aggregate_id = aggregate.id.clone();
        let nodes = [
            aggregate,
            node(
                &["aws_security_group.web"],
                "aws_security_group.web",
                ResourceChangeKind::Update,
            ),
        ];
        let graph = graph(
            ConfigurationRelationStatus::Available,
            vec![relation(
                RelationEndpoint::Instance("aws_instance.web[1]".to_owned()),
                RelationEndpoint::Instance("aws_security_group.web".to_owned()),
                RelationSource::Configuration,
            )],
            StateRelationStatus::NoPriorState,
            Vec::new(),
            &nodes,
        );

        assert_eq!(graph.links.len(), 1);
        assert_eq!(graph.links[0].to, aggregate_id);
        let aggregate = graph
            .nodes
            .iter()
            .find(|node| node.display_address == "aws_instance.web[*]")
            .expect("aggregate node should be retained");
        assert_eq!(aggregate.change_count, 2);
        assert_eq!(aggregate.breadcrumbs, ["app"]);
        assert!(aggregate.differs);
        assert!(aggregate.has_unknown);
        assert_eq!(
            aggregate.id.addresses(),
            &[
                "aws_instance.web[0]".to_owned(),
                "aws_instance.web[1]".to_owned()
            ]
        );
    }

    #[test]
    fn groups_branches_and_cycles_without_repeating_nodes() {
        let nodes = [
            node(
                &["aws_vpc.network"],
                "aws_vpc.network",
                ResourceChangeKind::Create,
            ),
            node(
                &["aws_subnet.private"],
                "aws_subnet.private",
                ResourceChangeKind::Update,
            ),
            node(
                &["aws_route_table.private"],
                "aws_route_table.private",
                ResourceChangeKind::Update,
            ),
        ];
        let graph = graph(
            ConfigurationRelationStatus::Available,
            vec![
                relation(
                    RelationEndpoint::Instance("aws_subnet.private".to_owned()),
                    RelationEndpoint::Instance("aws_vpc.network".to_owned()),
                    RelationSource::Configuration,
                ),
                relation(
                    RelationEndpoint::Instance("aws_route_table.private".to_owned()),
                    RelationEndpoint::Instance("aws_vpc.network".to_owned()),
                    RelationSource::Configuration,
                ),
                relation(
                    RelationEndpoint::Instance("aws_vpc.network".to_owned()),
                    RelationEndpoint::Instance("aws_route_table.private".to_owned()),
                    RelationSource::Configuration,
                ),
            ],
            StateRelationStatus::NoPriorState,
            Vec::new(),
            &nodes,
        );

        assert_eq!(graph.links.len(), 3);
        assert_eq!(graph.connected_groups.len(), 1);
        assert_eq!(
            graph.connected_groups[0].nodes,
            vec![
                id("aws_route_table.private"),
                id("aws_subnet.private"),
                id("aws_vpc.network"),
            ]
        );
    }

    #[test]
    fn expands_block_evidence_to_matching_changed_nodes_without_prefix_matches() {
        let nodes = [
            node(
                &["aws_instance.web[0]"],
                "aws_instance.web[*]",
                ResourceChangeKind::Update,
            ),
            node(
                &["aws_instance.web[1]"],
                "aws_instance.web[*]",
                ResourceChangeKind::Update,
            ),
            node(
                &["aws_instance.web-old[0]"],
                "aws_instance.web-old[*]",
                ResourceChangeKind::Update,
            ),
            node(
                &["aws_security_group.web"],
                "aws_security_group.web",
                ResourceChangeKind::Update,
            ),
        ];
        let graph = graph(
            ConfigurationRelationStatus::Available,
            vec![
                relation(
                    RelationEndpoint::Block("aws_instance.web".to_owned()),
                    RelationEndpoint::Instance("aws_security_group.web".to_owned()),
                    RelationSource::Configuration,
                ),
                relation(
                    RelationEndpoint::Instance("aws_security_group.web".to_owned()),
                    RelationEndpoint::Block("aws_instance.web".to_owned()),
                    RelationSource::Configuration,
                ),
            ],
            StateRelationStatus::NoPriorState,
            Vec::new(),
            &nodes,
        );

        assert_eq!(graph.links.len(), 4);
        assert!(
            graph
                .links
                .iter()
                .all(|link| link.kind == RelationGraphLinkKind::Dotted)
        );
        assert_eq!(
            graph
                .links
                .iter()
                .map(|link| (link.from.clone(), link.to.clone()))
                .collect::<Vec<_>>(),
            vec![
                (id("aws_instance.web[0]"), id("aws_security_group.web")),
                (id("aws_instance.web[1]"), id("aws_security_group.web")),
                (id("aws_security_group.web"), id("aws_instance.web[0]")),
                (id("aws_security_group.web"), id("aws_instance.web[1]")),
            ]
        );
    }

    #[test]
    fn block_evidence_preserves_fixed_keys_while_expanding_omitted_keys() {
        let nodes = [
            individual_node(
                "module.outer[0].module.inner[0].terraform_data.inside[2]",
                ResourceChangeKind::Update,
            ),
            individual_node(
                "module.outer[0].module.inner[1].terraform_data.inside[2]",
                ResourceChangeKind::Update,
            ),
            individual_node(
                "module.outer[0].module.inner[1].terraform_data.inside[3]",
                ResourceChangeKind::Update,
            ),
            individual_node(
                "module.outer[1].module.inner[0].terraform_data.inside[2]",
                ResourceChangeKind::Update,
            ),
            individual_node("aws_security_group.target", ResourceChangeKind::Update),
        ];
        let graph = graph(
            ConfigurationRelationStatus::Available,
            vec![relation(
                RelationEndpoint::Block(
                    "module.outer[0].module.inner.terraform_data.inside[2]".to_owned(),
                ),
                RelationEndpoint::Instance("aws_security_group.target".to_owned()),
                RelationSource::Configuration,
            )],
            StateRelationStatus::NoPriorState,
            Vec::new(),
            &nodes,
        );

        assert_eq!(
            graph
                .links
                .iter()
                .map(|link| (link.from.clone(), link.to.clone()))
                .collect::<Vec<_>>(),
            vec![
                (
                    id("aws_security_group.target"),
                    id("module.outer[0].module.inner[0].terraform_data.inside[2]")
                ),
                (
                    id("aws_security_group.target"),
                    id("module.outer[0].module.inner[1].terraform_data.inside[2]")
                ),
            ]
        );
        assert_eq!(
            graph.no_links_shown,
            vec![
                id("module.outer[0].module.inner[1].terraform_data.inside[3]"),
                id("module.outer[1].module.inner[0].terraform_data.inside[2]"),
            ]
        );
    }

    #[test]
    fn state_instance_evidence_selects_only_the_matching_changed_node() {
        let nodes = [
            node(
                &["aws_instance.web[0]"],
                "aws_instance.web[0]",
                ResourceChangeKind::Update,
            ),
            node(
                &["aws_instance.web[1]"],
                "aws_instance.web[1]",
                ResourceChangeKind::Update,
            ),
            node(
                &["aws_security_group.web"],
                "aws_security_group.web",
                ResourceChangeKind::Update,
            ),
        ];
        let graph = graph(
            ConfigurationRelationStatus::Available,
            Vec::new(),
            StateRelationStatus::Available,
            vec![relation(
                RelationEndpoint::Instance("aws_instance.web[1]".to_owned()),
                RelationEndpoint::Instance("aws_security_group.web".to_owned()),
                RelationSource::State,
            )],
            &nodes,
        );

        assert_eq!(graph.links.len(), 1);
        assert_eq!(graph.links[0].to, id("aws_instance.web[1]"));
        assert_eq!(graph.links[0].kind, RelationGraphLinkKind::Solid);
        assert_eq!(
            graph.links[0].sources,
            BTreeSet::from([RelationSource::State])
        );
    }

    #[test]
    fn preserves_known_links_and_marks_status_uncertainty_on_every_node() {
        struct StatusCase {
            name: &'static str,
            configuration_status: ConfigurationRelationStatus,
            state_status: StateRelationStatus,
            expected: BTreeSet<RelationUnresolvedReason>,
        }

        let nodes = [
            node(
                &["aws_instance.web"],
                "aws_instance.web",
                ResourceChangeKind::Update,
            ),
            node(
                &["aws_security_group.web"],
                "aws_security_group.web",
                ResourceChangeKind::Update,
            ),
        ];
        let evidence = vec![relation(
            RelationEndpoint::Instance("aws_instance.web".to_owned()),
            RelationEndpoint::Instance("aws_security_group.web".to_owned()),
            RelationSource::Configuration,
        )];
        let cases = [
            StatusCase {
                name: "configuration_partial",
                configuration_status: ConfigurationRelationStatus::Partial,
                state_status: StateRelationStatus::NoPriorState,
                expected: BTreeSet::from([RelationUnresolvedReason::ConfigurationPartial]),
            },
            StatusCase {
                name: "configuration_unavailable",
                configuration_status: ConfigurationRelationStatus::Unavailable,
                state_status: StateRelationStatus::NoPriorState,
                expected: BTreeSet::from([RelationUnresolvedReason::ConfigurationUnavailable]),
            },
            StatusCase {
                name: "configuration_not_collected",
                configuration_status: ConfigurationRelationStatus::NotCollected,
                state_status: StateRelationStatus::NoPriorState,
                expected: BTreeSet::from([RelationUnresolvedReason::ConfigurationNotCollected]),
            },
            StatusCase {
                name: "state_unavailable",
                configuration_status: ConfigurationRelationStatus::Available,
                state_status: StateRelationStatus::Unavailable,
                expected: BTreeSet::from([RelationUnresolvedReason::StateUnavailable]),
            },
            StatusCase {
                name: "state_not_collected",
                configuration_status: ConfigurationRelationStatus::Available,
                state_status: StateRelationStatus::NotCollected,
                expected: BTreeSet::from([RelationUnresolvedReason::StateNotCollected]),
            },
            StatusCase {
                name: "no_prior_state_is_known_empty",
                configuration_status: ConfigurationRelationStatus::Available,
                state_status: StateRelationStatus::NoPriorState,
                expected: BTreeSet::new(),
            },
        ];

        for case in cases {
            let graph = graph(
                case.configuration_status,
                evidence.clone(),
                case.state_status,
                Vec::new(),
                &nodes,
            );
            assert_eq!(graph.links.len(), 1, "case: {}", case.name);
            assert!(graph.links_unknown.is_empty(), "case: {}", case.name);
            assert!(graph.no_links_shown.is_empty(), "case: {}", case.name);
            assert!(
                graph
                    .nodes
                    .iter()
                    .all(|node| node.unresolved == case.expected),
                "case: {}",
                case.name
            );
        }
    }

    #[test]
    fn separates_unknown_links_from_nodes_with_no_shown_links() {
        let unknown = RelationEvidence::unresolved(
            RelationEndpoint::Instance("aws_instance.unresolved".to_owned()),
            RelationSource::Configuration,
            RelationUnresolvedReason::LocalValue,
        );
        let known = relation(
            RelationEndpoint::Instance("aws_instance.known".to_owned()),
            RelationEndpoint::Instance("aws_security_group.known".to_owned()),
            RelationSource::Configuration,
        );
        let mixed = RelationEvidence::unresolved(
            RelationEndpoint::Instance("aws_instance.known".to_owned()),
            RelationSource::Configuration,
            RelationUnresolvedReason::LocalValue,
        );
        let nodes = [
            node(
                &["aws_instance.unresolved"],
                "unknown",
                ResourceChangeKind::Update,
            ),
            node(&["aws_instance.known"], "known", ResourceChangeKind::Update),
            node(
                &["aws_security_group.known"],
                "linked",
                ResourceChangeKind::Update,
            ),
            node(
                &["aws_instance.outside"],
                "outside",
                ResourceChangeKind::Update,
            ),
            node(
                &["aws_s3_bucket.isolated"],
                "isolated",
                ResourceChangeKind::Update,
            ),
        ];
        let outside = relation(
            RelationEndpoint::Instance("aws_instance.outside".to_owned()),
            RelationEndpoint::Instance("aws_vpc.unchanged".to_owned()),
            RelationSource::Configuration,
        );
        let graph = graph(
            ConfigurationRelationStatus::Available,
            vec![known, mixed, unknown, outside],
            StateRelationStatus::NoPriorState,
            Vec::new(),
            &nodes,
        );

        assert_eq!(graph.links.len(), 1);
        assert_eq!(graph.links_unknown, vec![id("aws_instance.unresolved")]);
        assert_eq!(
            graph.no_links_shown,
            vec![id("aws_instance.outside"), id("aws_s3_bucket.isolated")]
        );
        assert!(graph.nodes.iter().any(|node| {
            node.id == id("aws_instance.known")
                && node.unresolved == BTreeSet::from([RelationUnresolvedReason::LocalValue])
                && !graph.links_unknown.contains(&node.id)
        }));
    }
}
