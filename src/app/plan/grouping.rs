use std::collections::{BTreeMap, BTreeSet};

use super::{
    AttributeType, PlanAction, ProviderSchemas, RelationNodeId, ResourceChange, ResourceChangeKind,
    attribute_diff::{
        AttributeChangeKind, AttributeDiff, AttributePathSegment, GroupingValue, UnknownShape,
        diff_resource_attributes,
    },
    comparison::resource_has_unknown,
    path::normalize_resource_address,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChangeGroup {
    pub(crate) display_address: String,
    pub(crate) members: Vec<GroupMember>,
    pub(crate) has_unknown: bool,
    /// The node every changed member maps to. Only `SingleEnvironmentOverview` fills it;
    /// it stays `None` everywhere else.
    pub(crate) node_id: Option<RelationNodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupMember {
    pub(crate) address: String,
    pub(crate) kind: ResourceChangeKind,
    pub(crate) actions: Vec<PlanAction>,
}

impl From<&ResourceChange> for GroupMember {
    fn from(change: &ResourceChange) -> Self {
        Self {
            address: change.address.clone(),
            kind: change.kind,
            actions: change.actions.clone(),
        }
    }
}

impl ChangeGroup {
    #[must_use]
    pub(crate) const fn is_repeated(&self) -> bool {
        self.members.len() >= 2
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanGrouping {
    pub(crate) groups: Vec<ChangeGroup>,
    pub(crate) repeated: usize,
}

pub(crate) struct GroupingCandidate {
    display_address: String,
    pub(crate) key: GroupingKey,
    pub(crate) has_unknown: bool,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct GroupingKey {
    normalized_address: String,
    actions: Vec<PlanAction>,
    attributes: Vec<AttributeSignature>,
}

pub(in crate::app) fn group_resource_changes(
    changes: &[ResourceChange],
    schemas: Option<&ProviderSchemas>,
) -> PlanGrouping {
    let mut groups = Vec::new();
    let mut candidates = BTreeMap::<GroupingKey, Vec<Candidate<'_>>>::new();

    for (position, change) in changes.iter().enumerate() {
        let Some(candidate) = grouping_candidate(change, schemas) else {
            groups.push(LocatedGroup {
                position,
                group: single_group(change),
            });
            continue;
        };
        candidates
            .entry(candidate.key)
            .or_default()
            .push(Candidate {
                position,
                display_address: candidate.display_address,
                has_unknown: candidate.has_unknown,
                change,
            });
    }

    for bucket in candidates.into_values() {
        if bucket.len() < 2 || has_duplicate_addresses(&bucket) {
            groups.extend(bucket.into_iter().map(|candidate| LocatedGroup {
                position: candidate.position,
                group: single_group(candidate.change),
            }));
            continue;
        }

        let position = bucket
            .iter()
            .map(|candidate| candidate.position)
            .min()
            .expect("a non-empty grouping bucket should have a first position");
        let display_address = bucket[0].display_address.clone();
        let has_unknown = bucket.iter().any(|candidate| candidate.has_unknown);
        let mut members = bucket
            .into_iter()
            .map(|candidate| GroupMember::from(candidate.change))
            .collect::<Vec<_>>();
        members.sort_by(|left, right| left.address.cmp(&right.address));
        groups.push(LocatedGroup {
            position,
            group: ChangeGroup {
                display_address,
                members,
                has_unknown,
                node_id: None,
            },
        });
    }

    groups.sort_by_key(|located| located.position);
    let repeated = groups
        .iter()
        .filter(|located| located.group.is_repeated())
        .map(|located| located.group.members.len())
        .sum();

    PlanGrouping {
        groups: groups.into_iter().map(|located| located.group).collect(),
        repeated,
    }
}

pub(crate) fn grouping_candidate(
    change: &ResourceChange,
    schemas: Option<&ProviderSchemas>,
) -> Option<GroupingCandidate> {
    if !change.kind.is_standard_change() {
        return None;
    }

    let address = normalize_resource_address(&change.address)?;
    let diffs = diff_resource_attributes(change);
    let has_changed_unknown = diffs.iter().any(|attribute| {
        attribute.kind == AttributeChangeKind::Changed && attribute.after.is_unknown()
    });
    let mut attributes = Vec::new();

    for attribute in diffs
        .iter()
        .filter(|attribute| attribute.kind == AttributeChangeKind::Changed)
    {
        if !is_comparable_attribute(attribute, change, schemas, has_changed_unknown) {
            return None;
        }
        let after = attribute.after.grouping_value()?;
        let value_type =
            if has_changed_unknown && (attribute.after.is_unknown() || attribute.path.len() > 1) {
                let value_type = grouping_attribute_type(change, &attribute.path, schemas)
                    .or_else(|| dynamic_scalar_unknown_type(attribute, change, schemas))?;
                if let GroupingValue::Unknown(shape) = &after
                    && !unknown_shape_matches_type(shape, value_type)
                {
                    return None;
                }
                Some(value_type.clone())
            } else {
                None
            };
        attributes.push(AttributeSignature {
            path: attribute.path.clone(),
            before: attribute.before.grouping_value()?,
            after,
            value_type,
        });
    }

    if attributes.is_empty() {
        return None;
    }
    attributes.sort();

    Some(GroupingCandidate {
        display_address: address.display().to_owned(),
        key: GroupingKey {
            normalized_address: address.normalized().to_owned(),
            actions: change.actions.clone(),
            attributes,
        },
        has_unknown: resource_has_unknown(change),
    })
}

struct LocatedGroup {
    position: usize,
    group: ChangeGroup,
}

fn single_group(change: &ResourceChange) -> ChangeGroup {
    ChangeGroup {
        display_address: change.address.clone(),
        members: vec![GroupMember::from(change)],
        has_unknown: false,
        node_id: None,
    }
}

struct Candidate<'a> {
    position: usize,
    display_address: String,
    has_unknown: bool,
    change: &'a ResourceChange,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AttributeSignature {
    path: Vec<AttributePathSegment>,
    before: GroupingValue,
    after: GroupingValue,
    value_type: Option<AttributeType>,
}

fn is_comparable_attribute(
    attribute: &AttributeDiff,
    change: &ResourceChange,
    schemas: Option<&ProviderSchemas>,
    has_changed_unknown: bool,
) -> bool {
    if attribute.before.is_sensitive() || attribute.after.is_sensitive() {
        return false;
    }

    if !has_changed_unknown
        && (attribute.before.kind() == super::attribute_diff::AttributeValueKind::Null
            || attribute.after.kind() == super::attribute_diff::AttributeValueKind::Null)
    {
        return false;
    }

    if attribute.before.grouping_value().is_none() || attribute.after.grouping_value().is_none() {
        return false;
    }

    if has_changed_unknown {
        if attribute.after.is_unknown() {
            return grouping_attribute_type(change, &attribute.path, schemas).is_some()
                || dynamic_scalar_unknown_type(attribute, change, schemas).is_some();
        }
        if attribute.path.len() > 1 {
            return grouping_attribute_type(change, &attribute.path, schemas).is_some();
        }
        return matches!(attribute.path.as_slice(), [AttributePathSegment::Key(_)]);
    }

    match attribute.path.as_slice() {
        [AttributePathSegment::Key(_)] => true,
        [
            AttributePathSegment::Key(attribute_name),
            AttributePathSegment::Key(_),
        ] => is_simple_map_attribute(change, attribute_name, schemas),
        _ => false,
    }
}

fn dynamic_scalar_unknown_type<'a>(
    attribute: &AttributeDiff,
    change: &ResourceChange,
    schemas: Option<&'a ProviderSchemas>,
) -> Option<&'a AttributeType> {
    let [AttributePathSegment::Key(name)] = attribute.path.as_slice() else {
        return None;
    };
    if !attribute.after.is_unknown()
        || !matches!(
            attribute.after.grouping_value(),
            Some(GroupingValue::Unknown(UnknownShape::Bool(true)))
        )
        || !matches!(
            attribute.before.grouping_value(),
            Some(GroupingValue::Bool(_) | GroupingValue::Number(_) | GroupingValue::String(_))
        )
    {
        return None;
    }

    let attribute_type = schemas?.resource(change)?.attributes.get(name)?;
    matches!(attribute_type, AttributeType::Dynamic).then_some(attribute_type)
}

fn grouping_attribute_type<'a>(
    change: &ResourceChange,
    path: &[AttributePathSegment],
    schemas: Option<&'a ProviderSchemas>,
) -> Option<&'a AttributeType> {
    let [AttributePathSegment::Key(name), rest @ ..] = path else {
        return None;
    };
    let schema = schemas?.resource(change)?;
    let mut attribute_type = schema
        .attributes
        .get(name)
        .or_else(|| schema.block_types.get(name))?;
    for segment in rest {
        attribute_type = match (attribute_type, segment) {
            (AttributeType::Object(fields), AttributePathSegment::Key(key)) => fields.get(key)?,
            (AttributeType::Map(value), AttributePathSegment::Key(_)) => value,
            (AttributeType::Tuple(elements), AttributePathSegment::Index(index)) => {
                elements.get(*index)?
            }
            _ => return None,
        };
    }
    (!contains_dynamic_type(attribute_type)).then_some(attribute_type)
}

fn contains_dynamic_type(attribute_type: &AttributeType) -> bool {
    match attribute_type {
        AttributeType::Dynamic => true,
        AttributeType::List(element)
        | AttributeType::Set(element)
        | AttributeType::Map(element) => contains_dynamic_type(element),
        AttributeType::Tuple(elements) => elements.iter().any(contains_dynamic_type),
        AttributeType::Object(fields) => fields.values().any(contains_dynamic_type),
        AttributeType::Bool | AttributeType::Number | AttributeType::String => false,
    }
}

fn unknown_shape_matches_type(shape: &UnknownShape, attribute_type: &AttributeType) -> bool {
    match shape {
        UnknownShape::Bool(_) => true,
        UnknownShape::Array(markers) => match attribute_type {
            AttributeType::Tuple(elements) if elements.len() == markers.len() => markers
                .iter()
                .zip(elements)
                .all(|(marker, element)| unknown_shape_matches_type(marker, element)),
            _ => false,
        },
        UnknownShape::Object(markers) => match attribute_type {
            AttributeType::Object(fields) => markers.iter().all(|(key, marker)| {
                fields
                    .get(key)
                    .is_some_and(|field| unknown_shape_matches_type(marker, field))
            }),
            AttributeType::Map(value) => markers
                .values()
                .all(|marker| unknown_shape_matches_type(marker, value)),
            _ => false,
        },
    }
}

fn is_simple_map_attribute(
    change: &ResourceChange,
    attribute_name: &str,
    schemas: Option<&ProviderSchemas>,
) -> bool {
    schemas
        .and_then(|schemas| schemas.resource(change)?.attributes.get(attribute_name))
        .is_some_and(AttributeType::is_simple_map)
}

fn has_duplicate_addresses(bucket: &[Candidate<'_>]) -> bool {
    let mut addresses = BTreeSet::new();
    bucket
        .iter()
        .any(|candidate| !addresses.insert(candidate.change.address.as_str()))
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::app::plan::{PlanValue, ProviderSchema, ResourceMode, ResourceSchema};

    fn plan_value(value: Value) -> PlanValue {
        match value {
            Value::Null => PlanValue::Null,
            Value::Bool(value) => PlanValue::Bool(value),
            Value::Number(value) => PlanValue::Number(value.to_string()),
            Value::String(value) => PlanValue::String(value),
            Value::Array(values) => PlanValue::Array(values.into_iter().map(plan_value).collect()),
            Value::Object(values) => PlanValue::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, plan_value(value)))
                    .collect(),
            ),
        }
    }

    fn change(address: &str, before: Value, after: Value) -> ResourceChange {
        ResourceChange {
            address: address.to_owned(),
            provider: None,
            resource_type: None,
            mode: ResourceMode::Managed,
            actions: vec![PlanAction::Update],
            kind: ResourceChangeKind::Update,
            before: Some(plan_value(before)),
            after: Some(plan_value(after)),
            before_sensitive: Some(PlanValue::Bool(false)),
            after_sensitive: Some(PlanValue::Bool(false)),
            after_unknown: Some(PlanValue::Bool(false)),
            previous_address: None,
            importing: None,
        }
    }

    fn schema_for(
        change: &mut ResourceChange,
        attributes: BTreeMap<String, AttributeType>,
    ) -> ProviderSchemas {
        change.provider = Some("registry.example/provider".to_owned());
        change.resource_type = Some("example_resource".to_owned());
        ProviderSchemas {
            providers: BTreeMap::from([(
                "registry.example/provider".to_owned(),
                ProviderSchema {
                    resources: BTreeMap::from([(
                        "example_resource".to_owned(),
                        ResourceSchema {
                            attributes,
                            block_types: BTreeMap::new(),
                        },
                    )]),
                },
            )]),
        }
    }

    fn simple_map_schema() -> BTreeMap<String, AttributeType> {
        BTreeMap::from([(
            "labels".to_owned(),
            AttributeType::Map(Box::new(AttributeType::String)),
        )])
    }

    fn schema_for_changes(
        changes: &mut [ResourceChange],
        attributes: BTreeMap<String, AttributeType>,
    ) -> ProviderSchemas {
        let schemas = schema_for(&mut changes[0], attributes);
        let provider = changes[0].provider.clone();
        let resource_type = changes[0].resource_type.clone();
        for change in &mut changes[1..] {
            change.provider.clone_from(&provider);
            change.resource_type.clone_from(&resource_type);
        }
        schemas
    }

    fn unknown_output_change(address: &str, input_before: &str) -> ResourceChange {
        let mut change = change(
            address,
            json!({"input": input_before, "output": "old"}),
            json!({"input": "new", "output": null}),
        );
        change.after_unknown = Some(plan_value(json!({"output": true})));
        change
    }

    #[test]
    fn groups_count_and_for_each_keys_at_every_module_and_resource_level() {
        let changes = vec![
            change(
                r"module.network[0].aws_instance.web[0]",
                json!({"name": "old"}),
                json!({"name": "new"}),
            ),
            change(
                r#"module.network["blue.green"].aws_instance.web["blue"]"#,
                json!({"name": "old"}),
                json!({"name": "new"}),
            ),
        ];

        let grouping = group_resource_changes(&changes, None);

        assert_eq!(grouping.repeated, 2);
        assert_eq!(grouping.groups.len(), 1);
        assert_eq!(
            grouping.groups[0].display_address,
            "module.network[*].aws_instance.web[*]"
        );
        assert_eq!(
            grouping.groups[0]
                .members
                .iter()
                .map(|change| change.address.as_str())
                .collect::<Vec<_>>(),
            vec![
                r#"module.network["blue.green"].aws_instance.web["blue"]"#,
                r"module.network[0].aws_instance.web[0]",
            ]
        );
    }

    #[test]
    fn separates_key_changes_and_keeps_different_modules_or_resource_names_apart() {
        let mut replacement = change(
            "aws_instance.web[0]",
            json!({"name": "old"}),
            json!({"name": "new"}),
        );
        replacement.actions = vec![PlanAction::Delete, PlanAction::Create];
        replacement.kind = ResourceChangeKind::Replace;

        let changes = vec![
            change(
                "aws_instance.web[0]",
                json!({"name": "old"}),
                json!({"name": "new"}),
            ),
            replacement,
            change(
                "module.other.aws_instance.web[0]",
                json!({"name": "old"}),
                json!({"name": "new"}),
            ),
            change(
                "aws_instance.worker[0]",
                json!({"name": "old"}),
                json!({"name": "new"}),
            ),
        ];

        let grouping = group_resource_changes(&changes, None);

        assert_eq!(grouping.repeated, 0);
        assert_eq!(grouping.groups.len(), changes.len());
        assert!(grouping.groups.iter().all(|group| !group.is_repeated()));
    }

    #[test]
    fn ignores_unchanged_attributes_when_the_other_resource_does_not_have_them() {
        let changes = vec![
            change(
                "aws_instance.web[0]",
                json!({"name": "old", "unchanged": "same"}),
                json!({"name": "new", "unchanged": "same"}),
            ),
            change(
                "aws_instance.web[1]",
                json!({"name": "old"}),
                json!({"name": "new"}),
            ),
        ];

        let grouping = group_resource_changes(&changes, None);

        assert_eq!(grouping.repeated, 2);
        assert_eq!(grouping.groups[0].members.len(), 2);
    }

    #[test]
    fn groups_simple_map_elements_only_when_schema_proves_the_map_type() {
        let mut first = change(
            "aws_instance.web[0]",
            json!({"labels": {"environment": "old"}}),
            json!({"labels": {"environment": "new"}}),
        );
        let schemas = schema_for(&mut first, simple_map_schema());
        let mut second = change(
            "aws_instance.web[1]",
            json!({"labels": {"environment": "old"}}),
            json!({"labels": {"environment": "new"}}),
        );
        second.provider = first.provider.clone();
        second.resource_type = first.resource_type.clone();

        let grouping = group_resource_changes(&[first, second], Some(&schemas));

        assert_eq!(grouping.repeated, 2);
        assert_eq!(grouping.groups.len(), 1);
        assert_eq!(grouping.groups[0].display_address, "aws_instance.web[*]");
    }

    #[test]
    fn keeps_map_and_composite_changes_individual_when_schema_is_missing_or_shape_is_unsupported() {
        let mut map_change = change(
            "aws_instance.map[0]",
            json!({"labels": {"environment": "old"}}),
            json!({"labels": {"environment": "new"}}),
        );
        let schemas = schema_for(&mut map_change, simple_map_schema());
        let mut map_without_schema = map_change.clone();
        map_without_schema.address = "aws_instance.map[1]".to_owned();
        map_without_schema.provider = None;
        map_without_schema.resource_type = None;

        let mut list_change = change(
            "aws_instance.list[0]",
            json!({"ports": [80]}),
            json!({"ports": [443]}),
        );
        list_change.provider = map_change.provider.clone();
        list_change.resource_type = map_change.resource_type.clone();
        let mut object_change = change(
            "aws_instance.object[0]",
            json!({"settings": {"enabled": false}}),
            json!({"settings": {"enabled": true}}),
        );
        object_change.provider = map_change.provider.clone();
        object_change.resource_type = map_change.resource_type.clone();

        let grouping = group_resource_changes(
            &[map_change, map_without_schema, list_change, object_change],
            Some(&schemas),
        );

        assert_eq!(grouping.repeated, 0);
        assert_eq!(grouping.groups.len(), 4);
    }

    #[test]
    fn excludes_changed_unknown_or_sensitive_values_but_allows_unchanged_sensitive_values() {
        let mut unchanged_sensitive = change(
            "aws_instance.web[0]",
            json!({"name": "old", "token": "secret"}),
            json!({"name": "new", "token": "secret"}),
        );
        unchanged_sensitive.after_sensitive = Some(plan_value(json!({"token": true})));

        let mut changed_sensitive = change(
            "aws_instance.web[1]",
            json!({"name": "old", "token": "old-secret"}),
            json!({"name": "new", "token": "new-secret"}),
        );
        changed_sensitive.after_sensitive = Some(plan_value(json!({"token": true})));

        let mut changed_unknown = change(
            "aws_instance.web[2]",
            json!({"name": "old"}),
            json!({"name": "new"}),
        );
        changed_unknown.after_unknown = Some(plan_value(json!({"name": true})));

        let grouping = group_resource_changes(
            &[unchanged_sensitive, changed_sensitive, changed_unknown],
            None,
        );

        assert_eq!(grouping.repeated, 0);
        assert_eq!(grouping.groups.len(), 3);
        assert!(grouping.groups[0].members[0].address.ends_with("[0]"));
    }

    #[test]
    fn groups_same_known_changes_with_matching_unknown_attributes() {
        let mut changes = (0..200)
            .map(|index| unknown_output_change(&format!("aws_instance.server[{index}]"), "old"))
            .collect::<Vec<_>>();
        let schemas = schema_for_changes(
            &mut changes,
            BTreeMap::from([
                ("input".to_owned(), AttributeType::String),
                ("output".to_owned(), AttributeType::String),
            ]),
        );

        let grouping = group_resource_changes(&changes, Some(&schemas));

        assert_eq!(grouping.repeated, 200);
        assert_eq!(grouping.groups.len(), 1);
        assert_eq!(grouping.groups[0].display_address, "aws_instance.server[*]");
        assert!(grouping.groups[0].has_unknown);
    }

    #[test]
    fn groups_a_dynamic_attribute_when_its_whole_value_is_unknown_after_a_known_scalar() {
        let mut changes = (0..4)
            .map(|index| unknown_output_change(&format!("terraform_data.server[{index}]"), "old"))
            .collect::<Vec<_>>();
        let schemas = schema_for_changes(
            &mut changes,
            BTreeMap::from([
                ("input".to_owned(), AttributeType::String),
                ("output".to_owned(), AttributeType::Dynamic),
            ]),
        );

        let grouping = group_resource_changes(&changes, Some(&schemas));

        assert_eq!(grouping.repeated, 4);
        assert_eq!(grouping.groups.len(), 1);
        assert_eq!(
            grouping.groups[0].display_address,
            "terraform_data.server[*]"
        );
        assert!(grouping.groups[0].has_unknown);
    }

    #[test]
    fn keeps_dynamic_unknowns_individual_without_schema_or_with_partial_shape() {
        let mut whole_unknown = (0..2)
            .map(|index| unknown_output_change(&format!("terraform_data.server[{index}]"), "old"))
            .collect::<Vec<_>>();
        assert_eq!(group_resource_changes(&whole_unknown, None).repeated, 0);

        for change in &mut whole_unknown {
            change.before = Some(plan_value(json!({
                "input": "old",
                "output": {"value": "old"}
            })));
            change.after = Some(plan_value(json!({
                "input": "new",
                "output": {"value": null}
            })));
            change.after_unknown = Some(plan_value(json!({
                "output": {"value": true}
            })));
        }
        let schemas = schema_for_changes(
            &mut whole_unknown,
            BTreeMap::from([
                ("input".to_owned(), AttributeType::String),
                ("output".to_owned(), AttributeType::Dynamic),
            ]),
        );

        let grouping = group_resource_changes(&whole_unknown, Some(&schemas));

        assert_eq!(grouping.repeated, 0);
        assert_eq!(grouping.groups.len(), 2);
    }

    #[test]
    fn keeps_dynamic_unknowns_individual_when_before_is_null_or_complex() {
        let mut changes = vec![
            unknown_output_change("terraform_data.server[0]", "old"),
            unknown_output_change("terraform_data.server[1]", "old"),
        ];
        changes[0].before = Some(plan_value(json!({"input": "old", "output": null})));
        changes[1].before = Some(plan_value(json!({
            "input": "old",
            "output": {"value": "old"}
        })));
        let schemas = schema_for_changes(
            &mut changes,
            BTreeMap::from([
                ("input".to_owned(), AttributeType::String),
                ("output".to_owned(), AttributeType::Dynamic),
            ]),
        );

        let grouping = group_resource_changes(&changes, Some(&schemas));

        assert_eq!(grouping.repeated, 0);
        assert_eq!(grouping.groups.len(), 2);
    }

    #[test]
    fn unknown_grouping_keeps_known_values_paths_and_null_distinct() {
        let different_before = unknown_output_change("aws_instance.server[1]", "other");
        let different_path = {
            let mut change = change(
                "aws_instance.server[2]",
                json!({"input": "old"}),
                json!({"input": "new", "id": null}),
            );
            change.after_unknown = Some(plan_value(json!({"id": true})));
            change
        };
        let mut known_null = change(
            "aws_instance.server[3]",
            json!({"input": "old"}),
            json!({"input": "new", "output": null}),
        );
        known_null.after_unknown = Some(plan_value(json!({"output": false})));
        let mut changes = vec![
            unknown_output_change("aws_instance.server[0]", "old"),
            different_before,
            different_path,
            known_null,
        ];
        let schemas = schema_for_changes(
            &mut changes,
            BTreeMap::from([
                ("input".to_owned(), AttributeType::String),
                ("output".to_owned(), AttributeType::String),
                ("id".to_owned(), AttributeType::String),
            ]),
        );

        let grouping = group_resource_changes(&changes, Some(&schemas));

        assert_eq!(grouping.repeated, 0);
        assert_eq!(grouping.groups.len(), 4);
        assert!(grouping.groups.iter().all(|group| !group.is_repeated()));
    }

    #[test]
    fn groups_nested_unknown_map_and_object_fields_with_their_known_changes() {
        let settings = AttributeType::Object(BTreeMap::from([
            ("input".to_owned(), AttributeType::String),
            ("output".to_owned(), AttributeType::String),
        ]));
        let mut changes = (0..2)
            .map(|index| {
                let mut change = change(
                    &format!("aws_instance.server[{index}]"),
                    json!({"settings": {"input": "old", "output": "old"}, "labels": {}}),
                    json!({"settings": {"input": "new", "output": null}, "labels": {}}),
                );
                change.after_unknown = Some(plan_value(json!({
                    "settings": {"output": true},
                    "labels": {"zone": true}
                })));
                change
            })
            .collect::<Vec<_>>();
        let schemas = schema_for_changes(
            &mut changes,
            BTreeMap::from([
                ("settings".to_owned(), settings),
                (
                    "labels".to_owned(),
                    AttributeType::Map(Box::new(AttributeType::String)),
                ),
            ]),
        );

        let grouping = group_resource_changes(&changes, Some(&schemas));

        assert_eq!(grouping.repeated, 2);
        assert_eq!(grouping.groups.len(), 1);
        assert!(grouping.groups[0].has_unknown);
    }

    #[test]
    fn only_schema_fixed_tuple_positions_allow_unknown_collection_elements() {
        let tuple_type = AttributeType::Tuple(vec![AttributeType::String, AttributeType::String]);
        let mut tuple_changes = (0..2)
            .map(|index| {
                let mut change = change(
                    &format!("aws_instance.tuple[{index}]"),
                    json!({"items": ["old", "old"]}),
                    json!({"items": ["new", null]}),
                );
                change.after_unknown = Some(plan_value(json!({"items": [false, true]})));
                change
            })
            .collect::<Vec<_>>();
        let tuple_schemas = schema_for_changes(
            &mut tuple_changes,
            BTreeMap::from([("items".to_owned(), tuple_type)]),
        );
        let tuple_grouping = group_resource_changes(&tuple_changes, Some(&tuple_schemas));
        assert_eq!(tuple_grouping.repeated, 2);
        assert_eq!(tuple_grouping.groups.len(), 1);

        let mut tuple_changes = (0..2)
            .map(|index| {
                let mut change = change(
                    &format!("aws_instance.tuple[{index}]"),
                    json!({"items": ["old", "old"]}),
                    json!({"items": ["new", null]}),
                );
                change.after_unknown = Some(plan_value(json!({"items": [false, true]})));
                change
            })
            .collect::<Vec<_>>();
        tuple_changes[1].after = Some(plan_value(json!({"items": [null, "new"]})));
        tuple_changes[1].after_unknown = Some(plan_value(json!({"items": [true, false]})));
        let tuple_schemas = schema_for_changes(
            &mut tuple_changes,
            BTreeMap::from([(
                "items".to_owned(),
                AttributeType::Tuple(vec![AttributeType::String, AttributeType::String]),
            )]),
        );
        let tuple_grouping = group_resource_changes(&tuple_changes, Some(&tuple_schemas));
        assert_eq!(tuple_grouping.repeated, 0);
        assert_eq!(tuple_grouping.groups.len(), 2);

        for collection_type in [
            AttributeType::List(Box::new(AttributeType::String)),
            AttributeType::Set(Box::new(AttributeType::String)),
        ] {
            let mut changes = (0..2)
                .map(|index| {
                    let mut change = change(
                        &format!("aws_instance.collection[{index}]"),
                        json!({"items": ["old", "old"]}),
                        json!({"items": ["new", null]}),
                    );
                    change.after_unknown = Some(plan_value(json!({"items": [false, true]})));
                    change
                })
                .collect::<Vec<_>>();
            let schemas = schema_for_changes(
                &mut changes,
                BTreeMap::from([("items".to_owned(), collection_type)]),
            );

            let grouping = group_resource_changes(&changes, Some(&schemas));

            assert_eq!(grouping.repeated, 0);
            assert_eq!(grouping.groups.len(), 2);
        }
    }

    #[test]
    fn never_groups_unknown_sensitive_values_even_when_the_schema_proves_their_shape() {
        let mut changes = (0..2)
            .map(|index| {
                let mut change =
                    unknown_output_change(&format!("aws_instance.server[{index}]"), "old");
                change.after_sensitive = Some(plan_value(json!({"output": true})));
                change
            })
            .collect::<Vec<_>>();
        let schemas = schema_for_changes(
            &mut changes,
            BTreeMap::from([
                ("input".to_owned(), AttributeType::String),
                ("output".to_owned(), AttributeType::String),
            ]),
        );

        let grouping = group_resource_changes(&changes, Some(&schemas));

        assert_eq!(grouping.repeated, 0);
        assert_eq!(grouping.groups.len(), 2);
        assert!(grouping.groups.iter().all(|group| !group.is_repeated()));
    }

    #[test]
    fn groups_numbers_by_exact_decimal_value() {
        struct NumberGroupingCase {
            name: &'static str,
            inputs: &'static [(&'static str, &'static str)],
            expected_group_count: usize,
            expected_repeated: usize,
        }

        let cases = [
            NumberGroupingCase {
                name: "groups equivalent decimals and keeps distinct integers above 2^53 apart",
                inputs: &[
                    ("1", "2"),
                    ("1.0", "2.0"),
                    ("9007199254740992", "9007199254740993"),
                ],
                expected_group_count: 2,
                expected_repeated: 2,
            },
            NumberGroupingCase {
                name: "groups equivalent trailing zero and exponent forms",
                inputs: &[
                    ("100000000000e1", "100000000001e1"),
                    ("1e12", "1000000000010"),
                ],
                expected_group_count: 1,
                expected_repeated: 2,
            },
            NumberGroupingCase {
                name: "keeps identical numbers with an out-of-range exponent ungrouped",
                inputs: &[
                    ("1e170141183460469231731687303715884105728", "2"),
                    ("1e170141183460469231731687303715884105728", "2"),
                ],
                expected_group_count: 2,
                expected_repeated: 0,
            },
        ];

        for case in cases {
            let changes = case
                .inputs
                .iter()
                .enumerate()
                .map(|(index, (before, after))| {
                    numeric_change(&format!("aws_instance.web[{index}]"), before, after)
                })
                .collect::<Vec<_>>();
            let grouping = group_resource_changes(&changes, None);

            assert_eq!(
                grouping.groups.len(),
                case.expected_group_count,
                "{}",
                case.name
            );
            assert_eq!(grouping.repeated, case.expected_repeated, "{}", case.name);
        }
    }

    #[test]
    fn reports_repeated_members_from_the_unfiltered_plan_and_keeps_every_member_once() {
        let mut changes = (0..200)
            .map(|index| {
                change(
                    &format!("aws_instance.web[{index}]"),
                    json!({"name": "old"}),
                    json!({"name": "new"}),
                )
            })
            .collect::<Vec<_>>();
        changes.push(change(
            "aws_instance.web[200]",
            json!({"name": "old"}),
            json!({"name": "different"}),
        ));

        let grouping = group_resource_changes(&changes, None);

        assert_eq!(grouping.repeated, 200);
        assert_eq!(grouping.groups.len(), 2);
        assert_eq!(grouping.groups[0].members.len(), 200);
        assert_eq!(grouping.groups[1].members.len(), 1);
        let addresses = grouping
            .groups
            .iter()
            .flat_map(|group| group.members.iter().map(|change| change.address.as_str()))
            .collect::<BTreeSet<_>>();
        assert_eq!(addresses.len(), changes.len());
    }

    #[test]
    fn does_not_put_duplicate_full_addresses_in_a_repeated_group() {
        let changes = vec![
            change(
                "aws_instance.web[0]",
                json!({"name": "old"}),
                json!({"name": "new"}),
            ),
            change(
                "aws_instance.web[0]",
                json!({"name": "old"}),
                json!({"name": "new"}),
            ),
        ];

        let grouping = group_resource_changes(&changes, None);

        assert_eq!(grouping.repeated, 0);
        assert_eq!(grouping.groups.len(), 2);
        assert!(grouping.groups.iter().all(|group| group.members.len() == 1));
    }

    fn numeric_change(address: &str, before: &str, after: &str) -> ResourceChange {
        let mut change = change(address, json!({}), json!({}));
        change.before = Some(PlanValue::Object(BTreeMap::from([(
            "size".to_owned(),
            PlanValue::Number(before.to_owned()),
        )])));
        change.after = Some(PlanValue::Object(BTreeMap::from([(
            "size".to_owned(),
            PlanValue::Number(after.to_owned()),
        )])));
        change
    }
}
