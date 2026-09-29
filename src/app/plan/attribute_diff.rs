use std::collections::BTreeSet;

use super::number::{CanonicalNumber, canonical_number};
use super::{PlanValue, ResourceChange, ResourceChangeKind};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AttributePathSegment {
    Key(String),
    Index(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttributeValueKind {
    Absent,
    Null,
    Unknown,
    Known,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttributeValue {
    kind: AttributeValueKind,
    original: Option<PlanValue>,
    unknown_marker: Option<PlanValue>,
    sensitive: bool,
}

impl AttributeValue {
    #[must_use]
    pub(crate) const fn kind(&self) -> AttributeValueKind {
        self.kind
    }

    #[must_use]
    pub(crate) const fn is_sensitive(&self) -> bool {
        self.sensitive
    }

    #[must_use]
    pub(crate) const fn is_unknown(&self) -> bool {
        matches!(self.kind, AttributeValueKind::Unknown)
    }

    #[must_use]
    pub(crate) fn grouping_value(&self) -> Option<GroupingValue> {
        match (&self.kind, self.original.as_ref()) {
            (AttributeValueKind::Absent, None) => Some(GroupingValue::Absent),
            (AttributeValueKind::Null, Some(PlanValue::Null)) => Some(GroupingValue::Null),
            (AttributeValueKind::Unknown, _) => self
                .unknown_marker
                .as_ref()
                .and_then(UnknownShape::from_marker)
                .map(GroupingValue::Unknown),
            (AttributeValueKind::Known, Some(PlanValue::Bool(value))) => {
                Some(GroupingValue::Bool(*value))
            }
            (AttributeValueKind::Known, Some(PlanValue::Number(value))) => {
                canonical_number(value).map(GroupingValue::Number)
            }
            (AttributeValueKind::Known, Some(PlanValue::String(value))) => {
                Some(GroupingValue::String(value.clone()))
            }
            _ => None,
        }
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum GroupingValue {
    Absent,
    Null,
    Bool(bool),
    Number(CanonicalNumber),
    String(String),
    Unknown(UnknownShape),
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum UnknownShape {
    Bool(bool),
    Array(Vec<Self>),
    Object(std::collections::BTreeMap<String, Self>),
}

impl UnknownShape {
    fn from_marker(marker: &PlanValue) -> Option<Self> {
        match marker {
            PlanValue::Bool(value) => Some(Self::Bool(*value)),
            PlanValue::Array(values) => values
                .iter()
                .map(Self::from_marker)
                .collect::<Option<Vec<_>>>()
                .map(Self::Array),
            PlanValue::Object(values) => values
                .iter()
                .map(|(key, value)| Some((key.clone(), Self::from_marker(value)?)))
                .collect::<Option<std::collections::BTreeMap<_, _>>>()
                .map(Self::Object),
            PlanValue::Null | PlanValue::Number(_) | PlanValue::String(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttributeChangeKind {
    Changed,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttributeDiff {
    pub(crate) path: Vec<AttributePathSegment>,
    pub(crate) before: AttributeValue,
    pub(crate) after: AttributeValue,
    pub(crate) kind: AttributeChangeKind,
}

pub(crate) fn diff_resource_attributes(change: &ResourceChange) -> Vec<AttributeDiff> {
    let mut attributes = Vec::new();
    collect_diffs(
        &mut attributes,
        Vec::new(),
        DiffInput {
            before: root_value(change, AttributeSide::Before),
            after: root_value(change, AttributeSide::After),
            before_sensitive: change.before_sensitive.as_ref(),
            after_sensitive: change.after_sensitive.as_ref(),
            after_unknown: change.after_unknown.as_ref(),
        },
        false,
        false,
    );
    attributes
}

#[derive(Clone, Copy)]
enum AttributeSide {
    Before,
    After,
}

const fn root_value(change: &ResourceChange, side: AttributeSide) -> Option<&PlanValue> {
    let value = match side {
        AttributeSide::Before => change.before.as_ref(),
        AttributeSide::After => change.after.as_ref(),
    };

    match (change.kind, side, value) {
        (ResourceChangeKind::Create, AttributeSide::Before, Some(PlanValue::Null))
        | (ResourceChangeKind::Delete, AttributeSide::After, Some(PlanValue::Null)) => None,
        _ => value,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContainerKind {
    Object,
    Array,
}

#[derive(Clone, Copy)]
struct DiffInput<'a> {
    before: Option<&'a PlanValue>,
    after: Option<&'a PlanValue>,
    before_sensitive: Option<&'a PlanValue>,
    after_sensitive: Option<&'a PlanValue>,
    after_unknown: Option<&'a PlanValue>,
}

impl DiffInput<'_> {
    fn child(self, segment: &AttributePathSegment) -> Self {
        Self {
            before: child_value(self.before, segment),
            after: child_value(self.after, segment),
            before_sensitive: child_value(self.before_sensitive, segment),
            after_sensitive: child_value(self.after_sensitive, segment),
            after_unknown: child_value(self.after_unknown, segment),
        }
    }
}

fn collect_diffs(
    attributes: &mut Vec<AttributeDiff>,
    path: Vec<AttributePathSegment>,
    input: DiffInput<'_>,
    inherited_before_sensitive: bool,
    inherited_after_sensitive: bool,
) {
    let before_is_sensitive = inherited_before_sensitive || marker_is_true(input.before_sensitive);
    let after_is_sensitive = inherited_after_sensitive || marker_is_true(input.after_sensitive);
    let atomic = marker_is_true(input.before_sensitive)
        || marker_is_true(input.after_sensitive)
        || marker_is_true(input.after_unknown)
        || omitted_complex_unknown(input)
        || preserves_atomic_transition(input);
    let children = match container_kind(input) {
        Some(kind) if !atomic => child_segments(kind, input),
        _ => Vec::new(),
    };
    if children.is_empty() {
        push_diff(
            attributes,
            path,
            input,
            before_is_sensitive,
            after_is_sensitive,
        );
        return;
    }
    for segment in children {
        let mut child_path = path.clone();
        child_path.push(segment.clone());
        collect_diffs(
            attributes,
            child_path,
            input.child(&segment),
            before_is_sensitive,
            after_is_sensitive,
        );
    }
}

fn preserves_atomic_transition(input: DiffInput<'_>) -> bool {
    match (input.before, input.after) {
        (Some(before), Some(after)) => {
            let before_kind = value_container_kind(before);
            let after_kind = value_container_kind(after);
            before_kind != after_kind && (before_kind.is_some() || after_kind.is_some())
        }
        (Some(before), None) => {
            match (
                value_container_kind(before),
                metadata_container_kind(input.after_unknown),
            ) {
                (Some(before_kind), Some(after_kind)) => before_kind != after_kind,
                (None, Some(_)) => true,
                _ => false,
            }
        }
        (None, Some(after)) => {
            value_container_kind(after).is_none()
                && metadata_container_kind(input.after_unknown).is_some()
        }
        (None, None) => false,
    }
}

fn omitted_complex_unknown(input: DiffInput<'_>) -> bool {
    input.after.is_none()
        && input
            .before
            .is_none_or(|before| value_container_kind(before).is_none())
        && metadata_container_kind(input.after_unknown).is_some()
        && marker_contains_true(input.after_unknown)
}

fn push_diff(
    attributes: &mut Vec<AttributeDiff>,
    path: Vec<AttributePathSegment>,
    input: DiffInput<'_>,
    inherited_before_sensitive: bool,
    inherited_after_sensitive: bool,
) {
    let before_value = attribute_value(
        input.before,
        input.before_sensitive,
        None,
        inherited_before_sensitive,
    );
    let after_value = attribute_value(
        input.after,
        input.after_sensitive,
        input.after_unknown,
        inherited_after_sensitive,
    );
    let kind = if same_attribute_value(&before_value, &after_value) {
        AttributeChangeKind::Unchanged
    } else {
        AttributeChangeKind::Changed
    };

    attributes.push(AttributeDiff {
        path,
        before: before_value,
        after: after_value,
        kind,
    });
}

fn attribute_value(
    value: Option<&PlanValue>,
    sensitive_marker: Option<&PlanValue>,
    unknown_marker: Option<&PlanValue>,
    inherited_sensitive: bool,
) -> AttributeValue {
    let is_unknown = marker_is_true(unknown_marker)
        || (value.is_none()
            && metadata_container_kind(unknown_marker).is_some()
            && marker_contains_true(unknown_marker));
    let is_sensitive = inherited_sensitive || marker_contains_true(sensitive_marker);
    let kind = if is_unknown {
        AttributeValueKind::Unknown
    } else {
        match value {
            None => AttributeValueKind::Absent,
            Some(PlanValue::Null) => AttributeValueKind::Null,
            Some(_) => AttributeValueKind::Known,
        }
    };
    AttributeValue {
        kind,
        original: value.cloned(),
        unknown_marker: unknown_marker.filter(|marker| marker.marks_any()).cloned(),
        sensitive: is_sensitive,
    }
}

fn same_attribute_value(before: &AttributeValue, after: &AttributeValue) -> bool {
    before.kind == after.kind
        && same_plan_value(before.original.as_ref(), after.original.as_ref())
        && before.unknown_marker == after.unknown_marker
}

fn same_plan_value(before: Option<&PlanValue>, after: Option<&PlanValue>) -> bool {
    match (before, after) {
        (Some(PlanValue::Number(before)), Some(PlanValue::Number(after))) => {
            match (canonical_number(before), canonical_number(after)) {
                (Some(before), Some(after)) => before == after,
                _ => before == after,
            }
        }
        _ => before == after,
    }
}

fn container_kind(input: DiffInput<'_>) -> Option<ContainerKind> {
    match (input.before, input.after) {
        (Some(before), Some(after)) => {
            matching_container_kinds(value_container_kind(before), value_container_kind(after))
        }
        (Some(before), None) => value_container_kind(before),
        (None, Some(after)) => value_container_kind(after),
        (None, None) => matching_container_kinds(
            metadata_container_kind(input.before_sensitive),
            matching_container_kinds(
                metadata_container_kind(input.after_sensitive),
                metadata_container_kind(input.after_unknown),
            ),
        ),
    }
}

fn matching_container_kinds(
    first: Option<ContainerKind>,
    second: Option<ContainerKind>,
) -> Option<ContainerKind> {
    match (first, second) {
        (Some(first), Some(second)) if first != second => None,
        (Some(kind), _) | (_, Some(kind)) => Some(kind),
        (None, None) => None,
    }
}

fn child_segments(kind: ContainerKind, input: DiffInput<'_>) -> Vec<AttributePathSegment> {
    match kind {
        ContainerKind::Object => {
            let mut keys = BTreeSet::new();
            add_object_keys(&mut keys, input.before);
            add_object_keys(&mut keys, input.after);
            add_object_keys(&mut keys, input.before_sensitive);
            add_object_keys(&mut keys, input.after_sensitive);
            add_object_keys(&mut keys, input.after_unknown);
            keys.into_iter().map(AttributePathSegment::Key).collect()
        }
        ContainerKind::Array => {
            let length = [
                input.before,
                input.after,
                input.before_sensitive,
                input.after_sensitive,
                input.after_unknown,
            ]
            .into_iter()
            .filter_map(value_array_length)
            .max()
            .unwrap_or(0);
            (0..length).map(AttributePathSegment::Index).collect()
        }
    }
}

fn add_object_keys(keys: &mut BTreeSet<String>, value: Option<&PlanValue>) {
    if let Some(PlanValue::Object(values)) = value {
        keys.extend(values.keys().cloned());
    }
}

fn child_value<'a>(
    value: Option<&'a PlanValue>,
    segment: &AttributePathSegment,
) -> Option<&'a PlanValue> {
    match (value, segment) {
        (Some(PlanValue::Object(values)), AttributePathSegment::Key(key)) => values.get(key),
        (Some(PlanValue::Array(values)), AttributePathSegment::Index(index)) => values.get(*index),
        _ => None,
    }
}

const fn marker_is_true(value: Option<&PlanValue>) -> bool {
    matches!(value, Some(PlanValue::Bool(true)))
}

fn marker_contains_true(value: Option<&PlanValue>) -> bool {
    value.is_some_and(PlanValue::marks_any)
}

const fn value_container_kind(value: &PlanValue) -> Option<ContainerKind> {
    match value {
        PlanValue::Object(_) => Some(ContainerKind::Object),
        PlanValue::Array(_) => Some(ContainerKind::Array),
        PlanValue::Null | PlanValue::Bool(_) | PlanValue::Number(_) | PlanValue::String(_) => None,
    }
}

fn metadata_container_kind(value: Option<&PlanValue>) -> Option<ContainerKind> {
    value.and_then(value_container_kind)
}

const fn value_array_length(value: Option<&PlanValue>) -> Option<usize> {
    match value {
        Some(PlanValue::Array(values)) => Some(values.len()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::{Value, json};

    use super::*;
    use crate::app::plan::{PlanAction, ResourceMode};

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

    struct ChangeFixture {
        before: Value,
        after: Value,
        before_sensitive: Value,
        after_sensitive: Value,
        after_unknown: Value,
    }

    fn change(fixture: ChangeFixture) -> ResourceChange {
        ResourceChange {
            address: "aws_instance.example".to_owned(),
            provider: None,
            resource_type: None,
            mode: ResourceMode::Managed,
            actions: vec![PlanAction::Update],
            kind: ResourceChangeKind::Update,
            before: Some(plan_value(fixture.before)),
            after: Some(plan_value(fixture.after)),
            before_sensitive: Some(plan_value(fixture.before_sensitive)),
            after_sensitive: Some(plan_value(fixture.after_sensitive)),
            after_unknown: Some(plan_value(fixture.after_unknown)),
            previous_address: None,
            importing: None,
        }
    }

    fn path(segments: &[AttributePathSegment]) -> Vec<AttributePathSegment> {
        segments.to_vec()
    }

    fn attribute<'a>(
        diffs: &'a [AttributeDiff],
        path: &[AttributePathSegment],
    ) -> &'a AttributeDiff {
        diffs
            .iter()
            .find(|attribute| attribute.path == path)
            .expect("attribute path should exist")
    }

    #[test]
    fn compares_nested_objects_and_arrays_by_key_and_index() {
        let change = change(ChangeFixture {
            before: json!({
                "name": "old",
                "tags": {"keep": "same", "remove": "gone"},
                "ports": [80, 443],
                "removed_ports": [8080, 8443]
            }),
            after: json!({
                "name": "new",
                "tags": {"add": "new", "keep": "same"},
                "ports": [80, 8443, 9443],
                "removed_ports": [8080]
            }),
            before_sensitive: json!(false),
            after_sensitive: json!(false),
            after_unknown: json!(false),
        });

        let diffs = diff_resource_attributes(&change);

        assert_eq!(
            diffs
                .iter()
                .map(|attribute| (&attribute.path, attribute.kind))
                .collect::<Vec<_>>(),
            vec![
                (
                    &path(&[AttributePathSegment::Key("name".to_owned())]),
                    AttributeChangeKind::Changed
                ),
                (
                    &path(&[
                        AttributePathSegment::Key("ports".to_owned()),
                        AttributePathSegment::Index(0)
                    ]),
                    AttributeChangeKind::Unchanged
                ),
                (
                    &path(&[
                        AttributePathSegment::Key("ports".to_owned()),
                        AttributePathSegment::Index(1)
                    ]),
                    AttributeChangeKind::Changed
                ),
                (
                    &path(&[
                        AttributePathSegment::Key("ports".to_owned()),
                        AttributePathSegment::Index(2)
                    ]),
                    AttributeChangeKind::Changed
                ),
                (
                    &path(&[
                        AttributePathSegment::Key("removed_ports".to_owned()),
                        AttributePathSegment::Index(0)
                    ]),
                    AttributeChangeKind::Unchanged
                ),
                (
                    &path(&[
                        AttributePathSegment::Key("removed_ports".to_owned()),
                        AttributePathSegment::Index(1)
                    ]),
                    AttributeChangeKind::Changed
                ),
                (
                    &path(&[
                        AttributePathSegment::Key("tags".to_owned()),
                        AttributePathSegment::Key("add".to_owned())
                    ]),
                    AttributeChangeKind::Changed
                ),
                (
                    &path(&[
                        AttributePathSegment::Key("tags".to_owned()),
                        AttributePathSegment::Key("keep".to_owned())
                    ]),
                    AttributeChangeKind::Unchanged
                ),
                (
                    &path(&[
                        AttributePathSegment::Key("tags".to_owned()),
                        AttributePathSegment::Key("remove".to_owned())
                    ]),
                    AttributeChangeKind::Changed
                ),
            ]
        );
    }

    #[test]
    fn distinguishes_null_from_absent() {
        let change = change(ChangeFixture {
            before: json!({"null_value": null}),
            after: json!({"null_value": null, "new_value": null}),
            before_sensitive: json!(false),
            after_sensitive: json!(false),
            after_unknown: json!(false),
        });

        let diffs = diff_resource_attributes(&change);
        let null_value = attribute(
            &diffs,
            &[AttributePathSegment::Key("null_value".to_owned())],
        );
        let new_value = attribute(&diffs, &[AttributePathSegment::Key("new_value".to_owned())]);

        assert_eq!(null_value.kind, AttributeChangeKind::Unchanged);
        assert_eq!(null_value.before.kind(), AttributeValueKind::Null);
        assert_eq!(null_value.after.kind(), AttributeValueKind::Null);
        assert_eq!(new_value.before.kind(), AttributeValueKind::Absent);
        assert_eq!(new_value.after.kind(), AttributeValueKind::Null);
        assert!(matches!(
            new_value.before.grouping_value(),
            Some(GroupingValue::Absent)
        ));
        assert!(matches!(
            new_value.after.grouping_value(),
            Some(GroupingValue::Null)
        ));
    }

    #[test]
    fn applies_sensitive_markers_to_each_side_and_inherits_parent_masks() {
        let change = change(ChangeFixture {
            before: json!({"credentials": {"user": "alice", "token": "old"}}),
            after: json!({"credentials": {"user": "bob", "token": "new"}}),
            before_sensitive: json!({"credentials": true}),
            after_sensitive: json!({"credentials": {"token": true}}),
            after_unknown: json!(false),
        });

        let diffs = diff_resource_attributes(&change);

        assert_eq!(diffs.len(), 1);
        let credentials = attribute(
            &diffs,
            &[AttributePathSegment::Key("credentials".to_owned())],
        );

        assert_eq!(credentials.before.kind(), AttributeValueKind::Known);
        assert_eq!(credentials.after.kind(), AttributeValueKind::Known);
        assert!(credentials.before.is_sensitive() && credentials.after.is_sensitive());
    }

    #[test]
    fn counts_unknown_nested_values_in_parent_sensitive_changes() {
        let change = change(ChangeFixture {
            before: json!({"secrets": [null]}),
            after: json!({"secrets": [null]}),
            before_sensitive: json!({"secrets": true}),
            after_sensitive: json!({"secrets": true}),
            after_unknown: json!({"secrets": [true]}),
        });

        let diffs = diff_resource_attributes(&change);
        let secrets = attribute(&diffs, &[AttributePathSegment::Key("secrets".to_owned())]);

        assert_eq!(secrets.kind, AttributeChangeKind::Changed);
        assert_eq!(diffs.len(), 1);
    }

    #[test]
    fn marks_one_sided_sensitive_values_only_on_that_side() {
        let change = change(ChangeFixture {
            before: json!({"public": "old"}),
            after: json!({"public": "new"}),
            before_sensitive: json!(false),
            after_sensitive: json!({"public": true}),
            after_unknown: json!(false),
        });

        let attribute = &diff_resource_attributes(&change)[0];

        assert!(!attribute.before.is_sensitive());
        assert!(attribute.after.is_sensitive());
    }

    #[test]
    fn compares_numbers_by_value_and_by_text_outside_the_normalizable_range() {
        let out_of_range = "1e170141183460469231731687303715884105728";
        let cases = [
            (
                "equivalent notation",
                "1.50",
                "15e-1",
                AttributeChangeKind::Unchanged,
            ),
            (
                "integers above 2^53",
                "9007199254740992",
                "9007199254740993",
                AttributeChangeKind::Changed,
            ),
            (
                "identical out-of-range text",
                out_of_range,
                out_of_range,
                AttributeChangeKind::Unchanged,
            ),
            (
                "equivalent out-of-range notation",
                out_of_range,
                "10e170141183460469231731687303715884105727",
                AttributeChangeKind::Changed,
            ),
        ];

        for (name, before, after, expected) in cases {
            let mut change = change(ChangeFixture {
                before: json!({}),
                after: json!({}),
                before_sensitive: json!(false),
                after_sensitive: json!(false),
                after_unknown: json!(false),
            });
            change.before = Some(PlanValue::Object(BTreeMap::from([(
                "size".to_owned(),
                PlanValue::Number(before.to_owned()),
            )])));
            change.after = Some(PlanValue::Object(BTreeMap::from([(
                "size".to_owned(),
                PlanValue::Number(after.to_owned()),
            )])));

            let diffs = diff_resource_attributes(&change);
            let size = attribute(&diffs, &[AttributePathSegment::Key("size".to_owned())]);

            assert_eq!(size.kind, expected, "{name}");
        }
    }

    #[test]
    fn represents_unknown_values_and_unknown_missing_attributes() {
        let change = change(ChangeFixture {
            before: json!({"known": "old", "null_value": null}),
            after: json!({"known": "new", "null_value": null}),
            before_sensitive: json!(false),
            after_sensitive: json!({"known": true}),
            after_unknown: json!({"future": true}),
        });

        let diffs = diff_resource_attributes(&change);
        let known = attribute(&diffs, &[AttributePathSegment::Key("known".to_owned())]);
        let future = attribute(&diffs, &[AttributePathSegment::Key("future".to_owned())]);

        assert_eq!(known.after.kind(), AttributeValueKind::Known);
        assert!(known.after.is_sensitive());
        assert!(!known.after.is_unknown());
        assert_eq!(future.before.kind(), AttributeValueKind::Absent);
        assert_eq!(future.after.kind(), AttributeValueKind::Unknown);
        assert!(future.after.is_unknown());
        assert!(!future.after.is_sensitive());
        assert!(matches!(
            future.after.grouping_value(),
            Some(GroupingValue::Unknown(UnknownShape::Bool(true)))
        ));
    }

    #[test]
    fn marks_sensitive_unknown_values_as_both_unknown_and_sensitive() {
        let change = change(ChangeFixture {
            before: json!({"token": "old"}),
            after: json!({"token": "planned"}),
            before_sensitive: json!(false),
            after_sensitive: json!({"token": true}),
            after_unknown: json!({"token": true}),
        });

        let attribute = &diff_resource_attributes(&change)[0];

        assert_eq!(attribute.after.kind(), AttributeValueKind::Unknown);
        assert!(attribute.after.is_sensitive());
    }

    #[test]
    fn treats_resource_root_null_as_absent_only_for_create_and_delete() {
        let mut create = change(ChangeFixture {
            before: json!(null),
            after: json!({"id": "created", "name": "example"}),
            before_sensitive: json!(false),
            after_sensitive: json!(false),
            after_unknown: json!(false),
        });
        create.kind = ResourceChangeKind::Create;

        let create_diffs = diff_resource_attributes(&create);
        let created_id = attribute(&create_diffs, &[AttributePathSegment::Key("id".to_owned())]);
        assert_eq!(created_id.before.kind(), AttributeValueKind::Absent);
        assert_eq!(created_id.after.kind(), AttributeValueKind::Known);
        assert_eq!(create_diffs.len(), 2);

        let mut delete = change(ChangeFixture {
            before: json!({"id": "deleted", "name": "example"}),
            after: json!(null),
            before_sensitive: json!(false),
            after_sensitive: json!(false),
            after_unknown: json!(false),
        });
        delete.kind = ResourceChangeKind::Delete;

        let delete_diffs = diff_resource_attributes(&delete);
        let deleted_id = attribute(&delete_diffs, &[AttributePathSegment::Key("id".to_owned())]);
        assert_eq!(deleted_id.before.kind(), AttributeValueKind::Known);
        assert_eq!(deleted_id.after.kind(), AttributeValueKind::Absent);
        assert_eq!(delete_diffs.len(), 2);
    }

    #[test]
    fn keeps_scalar_and_null_values_at_the_parent_when_shape_changes() {
        let change = change(ChangeFixture {
            before: json!({"settings": null, "name": "old"}),
            after: json!({"settings": {"enabled": true}, "name": "new"}),
            before_sensitive: json!(false),
            after_sensitive: json!(false),
            after_unknown: json!(false),
        });

        let diffs = diff_resource_attributes(&change);
        let settings = attribute(&diffs, &[AttributePathSegment::Key("settings".to_owned())]);

        assert_eq!(settings.before.kind(), AttributeValueKind::Null);
        assert_eq!(settings.after.kind(), AttributeValueKind::Known);
        assert_eq!(
            settings.after.original.as_ref(),
            Some(&plan_value(json!({"enabled": true})))
        );
        assert!(!diffs.iter().any(|attribute| {
            attribute.path
                == [
                    AttributePathSegment::Key("settings".to_owned()),
                    AttributePathSegment::Key("enabled".to_owned()),
                ]
        }));
    }

    #[test]
    fn omitted_complex_unknown_shapes_remain_at_the_parent() {
        struct Case {
            name: &'static str,
            before: Value,
            after_unknown: Value,
            expected_before_kind: AttributeValueKind,
            expected_shape: UnknownShape,
            expected_count: usize,
        }

        for case in [
            Case {
                name: "object_marker",
                before: json!(null),
                after_unknown: json!({"config": {"token": true}}),
                expected_before_kind: AttributeValueKind::Null,
                expected_shape: UnknownShape::Object(
                    [("token".to_owned(), UnknownShape::Bool(true))].into(),
                ),
                expected_count: 1,
            },
            Case {
                name: "array_marker",
                before: json!({"old": true}),
                after_unknown: json!({"config": [true]}),
                expected_before_kind: AttributeValueKind::Known,
                expected_shape: UnknownShape::Array(vec![UnknownShape::Bool(true)]),
                expected_count: 1,
            },
        ] {
            let mut change = change(ChangeFixture {
                before: json!({"config": case.before}),
                after: json!(null),
                before_sensitive: json!(false),
                after_sensitive: json!(false),
                after_unknown: case.after_unknown,
            });
            change.after = None;

            let diffs = diff_resource_attributes(&change);
            let config = attribute(&diffs, &[AttributePathSegment::Key("config".to_owned())]);

            assert_eq!(
                config.before.kind(),
                case.expected_before_kind,
                "case: {}",
                case.name
            );
            assert_eq!(
                config.after.kind(),
                AttributeValueKind::Unknown,
                "case: {}",
                case.name
            );
            assert!(
                config.after.grouping_value() == Some(GroupingValue::Unknown(case.expected_shape)),
                "case: {}",
                case.name
            );
            assert_eq!(
                config.kind,
                AttributeChangeKind::Changed,
                "case: {}",
                case.name
            );
            assert_eq!(diffs.len(), case.expected_count, "case: {}", case.name);
            assert_eq!(
                config.path,
                [AttributePathSegment::Key("config".to_owned())],
                "case: {}",
                case.name
            );
        }
    }

    #[test]
    fn debug_output_does_not_include_original_attribute_values() {
        let change = change(ChangeFixture {
            before: json!({"token": "synthetic-secret"}),
            after: json!({"token": "synthetic-secret-after"}),
            before_sensitive: json!(false),
            after_sensitive: json!({"token": true}),
            after_unknown: json!(false),
        });

        let debug = format!("{:?}", diff_resource_attributes(&change));

        assert!(!debug.contains("synthetic-secret"));
        assert!(!debug.contains("synthetic-secret-after"));
        assert!(debug.contains("sensitive: true"));
    }

    #[test]
    fn handles_empty_containers_as_single_attributes() {
        let change = change(ChangeFixture {
            before: json!({"object": {}, "array": []}),
            after: json!({"object": {}, "array": []}),
            before_sensitive: json!(false),
            after_sensitive: json!(false),
            after_unknown: json!(false),
        });

        let diffs = diff_resource_attributes(&change);

        assert_eq!(diffs.len(), 2);
        assert!(
            diffs
                .iter()
                .all(|attribute| attribute.kind == AttributeChangeKind::Unchanged)
        );
    }
}
