#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "detailed plan parsing remains for dormant Git attribution tests"
    )
)]

use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::app::plan::{
    OutputChange, Plan, PlanAction, PlanRelations, PlanValue, ReplacePathSegment, ResourceChange,
    ResourceChangeKind, ResourceMode, UnsupportedChange, UnsupportedChangeKind,
    UnsupportedChangeScope,
};
use crate::app::review::PlanMetadata;

const SUPPORTED_FORMAT_MAJOR: u64 = 1;

use super::PlanParseError;

/// Parses bytes containing the JSON document emitted by `terraform show -json`.
///
/// # Errors
///
/// Returns an error when the bytes are not valid JSON, the document does not
/// have the required plan shape, or it uses an unsupported format major
/// version.
pub(crate) fn parse_plan_json_bytes(input: &[u8]) -> Result<Plan, PlanParseError> {
    let document =
        serde_json::from_slice::<Value>(input).map_err(|_| PlanParseError::InvalidJson)?;
    parse_plan_document(&document)
}

pub(super) fn parse_plan_json_with_metadata(
    input: &[u8],
    detailed_exit_has_changes: bool,
) -> Result<(Plan, PlanMetadata, PlanRelations), PlanParseError> {
    let document =
        serde_json::from_slice::<Value>(input).map_err(|_| PlanParseError::InvalidJson)?;
    let root = document
        .as_object()
        .ok_or(PlanParseError::RootMustBeObject)?;
    let plan = parse_plan_document(&document)?;
    let metadata = super::metadata::metadata_from_document(root, detailed_exit_has_changes);
    let planned_addresses = parse_value_addresses(root, false)?;
    let deleted_addresses = plan
        .resource_changes
        .iter()
        .filter(|change| change.kind == ResourceChangeKind::Delete)
        .map(|change| change.address.clone())
        .collect();
    let relations =
        super::relations::parse_configuration(&document, &planned_addresses, &deleted_addresses);
    Ok((plan, metadata, relations))
}

pub(super) fn parse_plan_document(document: &Value) -> Result<Plan, PlanParseError> {
    let root = document
        .as_object()
        .ok_or(PlanParseError::RootMustBeObject)?;
    parse_format_version(root)?;
    let resources = optional_array(root, "resource_changes")?;

    let mut resource_changes = Vec::new();
    let mut unsupported_changes = Vec::new();

    for resource in resources {
        parse_resource_change(resource, &mut resource_changes, &mut unsupported_changes)?;
    }

    if let Some(deferred_changes) = root.get("deferred_changes") {
        parse_deferred_changes(deferred_changes, &mut unsupported_changes)?;
    }

    let output_changes = if let Some(output_changes) = root.get("output_changes") {
        parse_output_changes(output_changes)?
    } else {
        Vec::new()
    };

    if let Some(action_invocations) = root.get("action_invocations") {
        parse_action_invocations(action_invocations, &mut unsupported_changes)?;
    }

    if let Some(deferred_action_invocations) = root.get("deferred_action_invocations") {
        parse_deferred_action_invocations(deferred_action_invocations, &mut unsupported_changes)?;
    }

    let drifted_resources = match root.get("resource_drift") {
        Some(resource_drift) => parse_resource_drift(resource_drift)?,
        None => Vec::new(),
    };

    Ok(Plan {
        resource_changes,
        value_addresses: parse_value_addresses(root, true)?,
        unsupported_changes,
        output_changes,
        drifted_resources,
    })
}

fn parse_value_addresses(
    root: &Map<String, Value>,
    include_prior_state: bool,
) -> Result<BTreeSet<String>, PlanParseError> {
    let mut addresses = BTreeSet::new();
    let prior = if include_prior_state {
        optional_object(root, "prior_state")?
            .map(|state| optional_object(state, "values"))
            .transpose()?
            .flatten()
    } else {
        None
    };
    for values in [prior, optional_object(root, "planned_values")?]
        .into_iter()
        .flatten()
    {
        if let Some(module) = optional_object(values, "root_module")? {
            collect_value_addresses(module, &mut addresses)?;
        }
    }
    Ok(addresses)
}

fn collect_value_addresses(
    module: &Map<String, Value>,
    addresses: &mut BTreeSet<String>,
) -> Result<(), PlanParseError> {
    for resource in optional_array(module, "resources")? {
        let resource = resource
            .as_object()
            .ok_or(PlanParseError::InvalidField("values resource"))?;
        addresses.insert(required_string(resource, "address")?.to_owned());
    }
    for child in optional_array(module, "child_modules")? {
        let child = child
            .as_object()
            .ok_or(PlanParseError::InvalidField("values child module"))?;
        collect_value_addresses(child, addresses)?;
    }
    Ok(())
}

fn optional_object<'a>(
    object: &'a Map<String, Value>,
    name: &'static str,
) -> Result<Option<&'a Map<String, Value>>, PlanParseError> {
    match object.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_object()
            .map(Some)
            .ok_or(PlanParseError::InvalidField(name)),
    }
}

fn parse_format_version(root: &Map<String, Value>) -> Result<(), PlanParseError> {
    let version = required_string(root, "format_version")?;
    let mut components = version.split('.');
    let major = components
        .next()
        .and_then(|component| component.parse::<u64>().ok())
        .ok_or(PlanParseError::InvalidField("format_version"))?;
    components
        .next()
        .and_then(|component| component.parse::<u64>().ok())
        .ok_or(PlanParseError::InvalidField("format_version"))?;

    if components.next().is_some() {
        return Err(PlanParseError::InvalidField("format_version"));
    }
    if major != SUPPORTED_FORMAT_MAJOR {
        return Err(PlanParseError::UnsupportedFormatMajor(major));
    }

    Ok(())
}

enum ActionClassification {
    NoOp,
    Supported(ResourceChangeKind),
    Unsupported(UnsupportedChangeKind),
}

fn classify_resource_kind(actions: &[PlanAction]) -> ResourceChangeKind {
    match classify_actions(actions) {
        ActionClassification::NoOp => ResourceChangeKind::NoOp,
        ActionClassification::Supported(kind) => kind,
        ActionClassification::Unsupported(UnsupportedChangeKind::Read) => ResourceChangeKind::Read,
        ActionClassification::Unsupported(UnsupportedChangeKind::Move) => ResourceChangeKind::Move,
        ActionClassification::Unsupported(UnsupportedChangeKind::Import) => {
            ResourceChangeKind::Import
        }
        ActionClassification::Unsupported(UnsupportedChangeKind::UnknownAction) => {
            ResourceChangeKind::Unknown
        }
        ActionClassification::Unsupported(_) => ResourceChangeKind::Unsupported,
    }
}

const fn unsupported_kind(kind: ResourceChangeKind) -> Option<UnsupportedChangeKind> {
    match kind {
        ResourceChangeKind::Read => Some(UnsupportedChangeKind::Read),
        ResourceChangeKind::Move => Some(UnsupportedChangeKind::Move),
        ResourceChangeKind::Import => Some(UnsupportedChangeKind::Import),
        ResourceChangeKind::Unknown => Some(UnsupportedChangeKind::UnknownAction),
        ResourceChangeKind::Unsupported => Some(UnsupportedChangeKind::UnsupportedActions),
        ResourceChangeKind::Create
        | ResourceChangeKind::Update
        | ResourceChangeKind::Replace
        | ResourceChangeKind::Delete
        | ResourceChangeKind::NoOp => None,
    }
}

fn parse_resource_change(
    resource: &Value,
    resource_changes: &mut Vec<ResourceChange>,
    unsupported_changes: &mut Vec<UnsupportedChange>,
) -> Result<(), PlanParseError> {
    let resource = resource
        .as_object()
        .ok_or(PlanParseError::InvalidField("resource_changes item"))?;
    let address = required_string(resource, "address")?.to_owned();
    let mode = parse_resource_mode(resource)?;
    let change = required_object(resource, "change")?;
    let actions = parse_actions(change, "resource change actions")?;
    let previous_address = parse_optional_string(resource, "previous_address")?;
    let importing = parse_importing(change)?;
    let kind = if importing
        .as_ref()
        .is_some_and(|value| matches!(value, PlanValue::Object(_)))
    {
        ResourceChangeKind::Import
    } else if previous_address.is_some() {
        ResourceChangeKind::Move
    } else {
        classify_resource_kind(&actions)
    };
    let resource_change = ResourceChange {
        address,
        provider: parse_optional_string(resource, "provider_name")?,
        resource_type: parse_optional_string(resource, "type")?,
        resource_name: parse_optional_string(resource, "name")?,
        mode,
        actions: actions.clone(),
        kind,
        before: optional_plan_value(change, "before"),
        after: optional_plan_value(change, "after"),
        before_sensitive: optional_plan_value(change, "before_sensitive"),
        after_sensitive: optional_plan_value(change, "after_sensitive"),
        after_unknown: optional_plan_value(change, "after_unknown"),
        replace_paths: parse_replace_paths(change)?,
        action_reason: parse_optional_string(resource, "action_reason")?,
        previous_address,
        importing,
    };

    resource_changes.push(resource_change);
    if let Some(kind) = unsupported_kind(kind) {
        unsupported_changes.push(UnsupportedChange {
            scope: UnsupportedChangeScope::Resource,
            address: required_string(resource, "address")?.to_owned(),
            actions,
            kind,
            reason: None,
            action_type: None,
        });
    }
    Ok(())
}

fn parse_deferred_changes(
    deferred_changes: &Value,
    unsupported_changes: &mut Vec<UnsupportedChange>,
) -> Result<(), PlanParseError> {
    if deferred_changes.is_null() {
        return Ok(());
    }

    let deferred_changes = deferred_changes
        .as_array()
        .ok_or(PlanParseError::InvalidField("deferred_changes"))?;

    for deferred_change in deferred_changes {
        let deferred_change = deferred_change
            .as_object()
            .ok_or(PlanParseError::InvalidField("deferred change"))?;
        let reason = required_string(deferred_change, "reason")?.to_owned();
        let resource = required_object(deferred_change, "resource_change")?;
        let address = required_string(resource, "address")?.to_owned();
        parse_resource_mode(resource)?;
        let change = required_object(resource, "change")?;
        let actions = parse_actions(change, "deferred resource change actions")?;

        unsupported_changes.push(UnsupportedChange {
            scope: UnsupportedChangeScope::DeferredResource,
            address,
            actions,
            kind: UnsupportedChangeKind::Deferred,
            reason: Some(reason),
            action_type: None,
        });
    }

    Ok(())
}

fn parse_resource_drift(resource_drift: &Value) -> Result<Vec<String>, PlanParseError> {
    if resource_drift.is_null() {
        return Ok(Vec::new());
    }

    let resource_drift = resource_drift
        .as_array()
        .ok_or(PlanParseError::InvalidField("resource_drift"))?;

    let mut addresses = Vec::new();
    for resource in resource_drift {
        let resource = resource
            .as_object()
            .ok_or(PlanParseError::InvalidField("resource drift item"))?;
        let address = required_string(resource, "address")?.to_owned();
        parse_resource_mode(resource)?;
        let change = required_object(resource, "change")?;
        let actions = parse_actions(change, "resource drift actions")?;

        if !matches!(classify_actions(&actions), ActionClassification::NoOp) {
            addresses.push(address);
        }
    }

    Ok(addresses)
}

fn parse_output_changes(output_changes: &Value) -> Result<Vec<OutputChange>, PlanParseError> {
    if output_changes.is_null() {
        return Ok(Vec::new());
    }

    let output_changes = output_changes
        .as_object()
        .ok_or(PlanParseError::InvalidField("output_changes"))?;

    let mut changes = Vec::new();
    for (address, output) in output_changes {
        let output = output
            .as_object()
            .ok_or(PlanParseError::InvalidField("output change"))?;
        let change = output
            .get("change")
            .and_then(Value::as_object)
            .unwrap_or(output);
        let actions = match change.get("actions") {
            None | Some(Value::Null) => Vec::new(),
            Some(_) => parse_actions(change, "output change actions")?,
        };
        changes.push(OutputChange {
            address: address.clone(),
            actions,
            before: optional_plan_value(change, "before"),
            after: optional_plan_value(change, "after"),
            before_sensitive: optional_plan_value(change, "before_sensitive"),
            after_sensitive: optional_plan_value(change, "after_sensitive"),
            after_unknown: optional_plan_value(change, "after_unknown"),
        });
    }

    Ok(changes)
}

fn parse_action_invocations(
    action_invocations: &Value,
    unsupported_changes: &mut Vec<UnsupportedChange>,
) -> Result<(), PlanParseError> {
    if action_invocations.is_null() {
        return Ok(());
    }

    let action_invocations = action_invocations
        .as_array()
        .ok_or(PlanParseError::InvalidField("action_invocations"))?;

    for action_invocation in action_invocations {
        let action_invocation = action_invocation
            .as_object()
            .ok_or(PlanParseError::InvalidField("action invocation"))?;
        let (address, action_type) = parse_action_invocation_metadata(action_invocation)?;

        unsupported_changes.push(UnsupportedChange {
            scope: UnsupportedChangeScope::ActionInvocation,
            address,
            actions: Vec::new(),
            kind: UnsupportedChangeKind::ActionInvocation,
            reason: None,
            action_type: Some(action_type),
        });
    }

    Ok(())
}

fn parse_deferred_action_invocations(
    deferred_action_invocations: &Value,
    unsupported_changes: &mut Vec<UnsupportedChange>,
) -> Result<(), PlanParseError> {
    if deferred_action_invocations.is_null() {
        return Ok(());
    }

    let deferred_action_invocations = deferred_action_invocations
        .as_array()
        .ok_or(PlanParseError::InvalidField("deferred_action_invocations"))?;

    for deferred_action_invocation in deferred_action_invocations {
        let deferred_action_invocation = deferred_action_invocation
            .as_object()
            .ok_or(PlanParseError::InvalidField("deferred action invocation"))?;
        let reason = required_string(deferred_action_invocation, "reason")?.to_owned();
        let action_invocation = required_object(deferred_action_invocation, "action_invocation")?;
        let (address, action_type) = parse_action_invocation_metadata(action_invocation)?;

        unsupported_changes.push(UnsupportedChange {
            scope: UnsupportedChangeScope::ActionInvocation,
            address,
            actions: Vec::new(),
            kind: UnsupportedChangeKind::DeferredActionInvocation,
            reason: Some(reason),
            action_type: Some(action_type),
        });
    }

    Ok(())
}

fn parse_action_invocation_metadata(
    action_invocation: &Map<String, Value>,
) -> Result<(String, String), PlanParseError> {
    let address = required_string(action_invocation, "address")?.to_owned();
    let action_type = required_string(action_invocation, "type")?.to_owned();

    Ok((address, action_type))
}

fn parse_resource_mode(resource: &Map<String, Value>) -> Result<ResourceMode, PlanParseError> {
    match resource.get("mode") {
        None => Ok(ResourceMode::Managed),
        Some(value) => match value
            .as_str()
            .ok_or(PlanParseError::InvalidField("resource mode"))?
        {
            "managed" => Ok(ResourceMode::Managed),
            "data" => Ok(ResourceMode::Data),
            _ => Err(PlanParseError::InvalidField("resource mode")),
        },
    }
}

pub(super) fn parse_actions(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Vec<PlanAction>, PlanParseError> {
    let actions = object
        .get("actions")
        .ok_or(PlanParseError::MissingField(field))?
        .as_array()
        .ok_or(PlanParseError::InvalidField(field))?;

    if actions.is_empty() {
        return Err(PlanParseError::InvalidField(field));
    }

    actions
        .iter()
        .map(|action| {
            let action = action.as_str().ok_or(PlanParseError::InvalidField(field))?;
            Ok(match action {
                "create" => PlanAction::Create,
                "read" => PlanAction::Read,
                "update" => PlanAction::Update,
                "delete" => PlanAction::Delete,
                "no-op" => PlanAction::NoOp,
                action => PlanAction::Unknown(action.to_owned()),
            })
        })
        .collect()
}

fn classify_actions(actions: &[PlanAction]) -> ActionClassification {
    match actions {
        [PlanAction::NoOp] => ActionClassification::NoOp,
        [PlanAction::Create] => ActionClassification::Supported(ResourceChangeKind::Create),
        [PlanAction::Update] => ActionClassification::Supported(ResourceChangeKind::Update),
        [PlanAction::Delete] => ActionClassification::Supported(ResourceChangeKind::Delete),
        [PlanAction::Create, PlanAction::Delete] | [PlanAction::Delete, PlanAction::Create] => {
            ActionClassification::Supported(ResourceChangeKind::Replace)
        }
        [PlanAction::Read] => ActionClassification::Unsupported(UnsupportedChangeKind::Read),
        [PlanAction::Unknown(action)] if action == "move" => {
            ActionClassification::Unsupported(UnsupportedChangeKind::Move)
        }
        [PlanAction::Unknown(action)] if action == "import" => {
            ActionClassification::Unsupported(UnsupportedChangeKind::Import)
        }
        actions
            if actions
                .iter()
                .any(|action| matches!(action, PlanAction::Unknown(_))) =>
        {
            ActionClassification::Unsupported(UnsupportedChangeKind::UnknownAction)
        }
        _ => ActionClassification::Unsupported(UnsupportedChangeKind::UnsupportedActions),
    }
}

fn parse_replace_paths(
    change: &Map<String, Value>,
) -> Result<Option<Vec<Vec<ReplacePathSegment>>>, PlanParseError> {
    let Some(value) = change.get("replace_paths") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let paths = value
        .as_array()
        .ok_or(PlanParseError::InvalidField("replace_paths"))?;

    paths
        .iter()
        .map(|path| {
            path.as_array()
                .ok_or(PlanParseError::InvalidField("replace_paths"))?
                .iter()
                .map(|segment| match segment {
                    Value::String(segment) => Ok(ReplacePathSegment::Attribute(segment.clone())),
                    Value::Number(number) => number
                        .as_u64()
                        .map(ReplacePathSegment::Index)
                        .ok_or(PlanParseError::InvalidField("replace_paths")),
                    _ => Err(PlanParseError::InvalidField("replace_paths")),
                })
                .collect()
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn optional_array<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a [Value], PlanParseError> {
    object.get(field).map_or(Ok(&[]), |value| {
        value
            .as_array()
            .map(Vec::as_slice)
            .ok_or(PlanParseError::InvalidField(field))
    })
}

fn required_object<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a Map<String, Value>, PlanParseError> {
    object
        .get(field)
        .ok_or(PlanParseError::MissingField(field))?
        .as_object()
        .ok_or(PlanParseError::InvalidField(field))
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a str, PlanParseError> {
    object
        .get(field)
        .ok_or(PlanParseError::MissingField(field))?
        .as_str()
        .ok_or(PlanParseError::InvalidField(field))
}

fn optional_plan_value(object: &Map<String, Value>, field: &str) -> Option<PlanValue> {
    object.get(field).map(plan_value)
}

fn parse_importing(object: &Map<String, Value>) -> Result<Option<PlanValue>, PlanParseError> {
    match object.get("importing") {
        None => Ok(None),
        Some(value @ (Value::Null | Value::Object(_))) => Ok(Some(plan_value(value))),
        Some(_) => Err(PlanParseError::InvalidField("importing")),
    }
}

fn plan_value(value: &Value) -> PlanValue {
    match value {
        Value::Null => PlanValue::Null,
        Value::Bool(value) => PlanValue::Bool(*value),
        Value::Number(value) => PlanValue::Number(value.to_string()),
        Value::String(value) => PlanValue::String(value.clone()),
        Value::Array(values) => PlanValue::Array(values.iter().map(plan_value).collect()),
        Value::Object(values) => PlanValue::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), plan_value(value)))
                .collect::<BTreeMap<_, _>>(),
        ),
    }
}

fn parse_optional_string(
    object: &Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, PlanParseError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_owned)
            .ok_or(PlanParseError::InvalidField(field))
            .map(Some),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::app::plan::PlanSummary;

    impl PlanSummary {
        pub(crate) fn total(self) -> usize {
            self.creates + self.updates + self.replaces + self.deletes
        }
    }

    fn parse_plan_json(input: &str) -> Result<Plan, PlanParseError> {
        let document =
            serde_json::from_str::<Value>(input).map_err(|_| PlanParseError::InvalidJson)?;
        parse_plan_document(&document)
    }

    fn plan_with_resources(resources: Value) -> String {
        let mut document = Map::new();
        document.insert("format_version".to_owned(), json!("1.2"));
        document.insert("terraform_version".to_owned(), json!("1.9.0"));
        document.insert("resource_changes".to_owned(), resources);
        document.insert("output_changes".to_owned(), json!({}));

        serde_json::to_string(&Value::Object(document)).expect("synthetic plan should serialize")
    }

    fn resource(address: &str, mode: &str, actions: Value) -> Value {
        let mut change = Map::new();
        change.insert("actions".to_owned(), actions);
        change.insert("before".to_owned(), Value::Null);
        change.insert("after".to_owned(), json!({"id": address}));
        change.insert("before_sensitive".to_owned(), json!(false));
        change.insert("after_sensitive".to_owned(), json!({"id": false}));
        change.insert("after_unknown".to_owned(), json!({"id": true}));
        change.insert("replace_paths".to_owned(), json!([["id"]]));

        let mut resource = Map::new();
        resource.insert("address".to_owned(), json!(address));
        resource.insert("mode".to_owned(), json!(mode));
        resource.insert("type".to_owned(), json!("synthetic_resource"));
        resource.insert("name".to_owned(), json!("example"));
        resource.insert("change".to_owned(), Value::Object(change));

        Value::Object(resource)
    }

    #[test]
    fn reads_supported_resource_changes_and_summarizes_each_kind() {
        let input = plan_with_resources(json!([
            resource("aws_vpc.main", "managed", json!(["create"])),
            resource("aws_subnet.private", "managed", json!(["update"])),
            resource("aws_instance.api", "managed", json!(["create", "delete"])),
            resource("aws_instance.worker", "data", json!(["delete"])),
            resource("aws_instance.queue", "managed", json!(["delete", "create"]))
        ]));

        let plan = parse_plan_json(&input).expect("plan should parse");

        assert_eq!(plan.resource_changes.len(), 5);
        assert_eq!(plan.resource_changes[0].kind, ResourceChangeKind::Create);
        assert_eq!(plan.resource_changes[1].kind, ResourceChangeKind::Update);
        assert_eq!(plan.resource_changes[2].kind, ResourceChangeKind::Replace);
        assert_eq!(
            plan.resource_changes[2].actions,
            vec![PlanAction::Create, PlanAction::Delete]
        );
        assert_eq!(plan.resource_changes[3].kind, ResourceChangeKind::Delete);
        assert_eq!(plan.resource_changes[3].mode, ResourceMode::Data);
        assert_eq!(plan.resource_changes[4].kind, ResourceChangeKind::Replace);
        assert_eq!(
            plan.resource_changes[4].actions,
            vec![PlanAction::Delete, PlanAction::Create]
        );
        assert_eq!(plan.summary().creates, 1);
        assert_eq!(plan.summary().updates, 1);
        assert_eq!(plan.summary().replaces, 2);
        assert_eq!(plan.summary().deletes, 1);
        assert_eq!(plan.summary().total(), 5);
    }

    #[test]
    fn preserves_change_values_and_replacement_paths_for_follow_up_diffing() {
        let input = plan_with_resources(json!([resource(
            "aws_instance.api",
            "managed",
            json!(["update"])
        )]));

        let plan = parse_plan_json(&input).expect("plan should parse");
        let change = &plan.resource_changes[0];

        assert_eq!(change.before, Some(PlanValue::Null));
        assert_eq!(
            change.after,
            Some(plan_value(&json!({"id": "aws_instance.api"})))
        );
        assert_eq!(change.before_sensitive, Some(PlanValue::Bool(false)));
        assert_eq!(
            change.after_sensitive,
            Some(plan_value(&json!({"id": false})))
        );
        assert_eq!(change.after_unknown, Some(plan_value(&json!({"id": true}))));
        assert_eq!(
            change.replace_paths,
            Some(vec![vec![ReplacePathSegment::Attribute("id".to_owned())]])
        );
    }

    #[test]
    fn reads_action_reason_from_resource_change_metadata() {
        let mut resource = resource("aws_instance.api", "managed", json!(["delete", "create"]));
        resource["action_reason"] = json!("replace_because_cannot_update");
        resource["change"]["action_reason"] = json!("wrong-level");

        let plan =
            parse_plan_json(&plan_with_resources(json!([resource]))).expect("plan should parse");

        assert_eq!(
            plan.resource_changes[0].action_reason,
            Some("replace_because_cannot_update".to_owned())
        );
    }

    #[test]
    fn redacts_attribute_values_from_plan_debug_output() {
        let mut resource = resource("aws_instance.api", "managed", json!(["update"]));
        resource["change"]["after"] = json!("synthetic-secret");

        let plan =
            parse_plan_json(&plan_with_resources(json!([resource]))).expect("plan should parse");
        let debug = format!("{plan:?}");

        assert!(!debug.contains("synthetic-secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn previous_address_keeps_delete_and_replace_actions_out_of_the_summary() {
        let mut moved_delete = resource("aws_instance.moved_delete", "managed", json!(["delete"]));
        moved_delete["previous_address"] = json!("aws_instance.old_delete");
        let mut moved_replace = resource(
            "aws_instance.moved_replace",
            "managed",
            json!(["create", "delete"]),
        );
        moved_replace["previous_address"] = json!("aws_instance.old_replace");

        let plan = parse_plan_json(&plan_with_resources(json!([moved_delete, moved_replace])))
            .expect("plan should parse");

        assert_eq!(
            plan.resource_changes
                .iter()
                .map(|change| change.kind)
                .collect::<Vec<_>>(),
            [ResourceChangeKind::Move, ResourceChangeKind::Move]
        );
        assert_eq!(plan.summary().total(), 0);
    }

    #[test]
    fn retains_move_and_import_markers_as_unsupported_changes() {
        let mut moved = resource("aws_instance.renamed", "managed", json!(["no-op"]));
        moved["previous_address"] = json!("aws_instance.old_name");

        let mut imported = resource("aws_instance.imported", "managed", json!(["create"]));
        imported["change"]["importing"] = json!({"identity": {"account": "synthetic"}});

        let mut empty_import = resource("aws_instance.empty_import", "managed", json!(["create"]));
        empty_import["change"]["importing"] = json!({});

        let plan = parse_plan_json(&plan_with_resources(json!([moved, imported, empty_import])))
            .expect("plan should parse");

        assert!(
            !plan
                .resource_changes
                .iter()
                .any(|change| change.kind.is_standard_change())
        );
        assert_eq!(plan.unsupported_changes.len(), 3);
        assert_eq!(
            plan.unsupported_changes[0].kind,
            UnsupportedChangeKind::Move
        );
        assert_eq!(
            plan.unsupported_changes[1].kind,
            UnsupportedChangeKind::Import
        );
        assert_eq!(
            plan.unsupported_changes[2].kind,
            UnsupportedChangeKind::Import
        );
        assert_eq!(plan.resource_changes.len(), 3);
        assert_eq!(plan.resource_changes[0].kind, ResourceChangeKind::Move);
        assert_eq!(plan.resource_changes[1].kind, ResourceChangeKind::Import);
        assert_eq!(plan.resource_changes[2].kind, ResourceChangeKind::Import);
        assert_eq!(
            plan.resource_changes[0].previous_address.as_deref(),
            Some("aws_instance.old_name")
        );
        assert!(plan.resource_changes[1].importing.is_some());
    }

    #[test]
    fn retains_provider_identity_and_distinguishes_missing_null_unknown_and_sensitive_values() {
        let input = json!({
            "format_version": "1.2",
            "resource_changes": [{
                "address": "module.api[\"blue\"].example.server[0]",
                "provider_name": "registry.terraform.io/hashicorp/example",
                "type": "example_server",
                "name": "server",
                "mode": "managed",
                "change": {
                    "actions": ["update"],
                    "before": {"missing": null, "null_value": null, "secret": "old"},
                    "after": {"null_value": null, "secret": "new"},
                    "before_sensitive": {"secret": true},
                    "after_sensitive": {"secret": true},
                    "after_unknown": {"future": true}
                }
            }]
        });

        let plan = parse_plan_json(&input.to_string()).expect("plan should parse");
        let change = &plan.resource_changes[0];
        assert_eq!(
            change.provider.as_deref(),
            Some("registry.terraform.io/hashicorp/example")
        );
        assert_eq!(change.resource_type.as_deref(), Some("example_server"));
        assert_eq!(change.resource_name.as_deref(), Some("server"));
        assert_eq!(change.address, "module.api[\"blue\"].example.server[0]");
        assert!(change.before.is_some());
        assert!(change.after.is_some());
        assert!(change.after_unknown.is_some());
        assert!(change.before_sensitive.is_some());

        let debug = format!("{plan:?}");
        assert!(!debug.contains("old"));
        assert!(!debug.contains("new"));
    }

    #[test]
    fn retains_noop_resource_and_output_change_values_for_existence_review() {
        let input = json!({
            "format_version": "1.2",
            "resource_changes": [{
                "address": "terraform_data.existing",
                "provider_name": "registry.terraform.io/hashicorp/null",
                "type": "terraform_data",
                "name": "existing",
                "mode": "managed",
                "change": {
                    "actions": ["no-op"],
                    "before": {"id": "known"},
                    "after": {"id": "known"},
                    "after_unknown": {}
                }
            }],
            "output_changes": {
                "endpoint": {
                    "change": {
                        "actions": ["update"],
                        "before": null,
                        "after": "synthetic-endpoint"
                    }
                }
            }
        });

        let plan = parse_plan_json(&input.to_string()).expect("plan should parse");
        assert_eq!(plan.resource_changes.len(), 1);
        assert_eq!(plan.resource_changes[0].kind, ResourceChangeKind::NoOp);
        assert_eq!(plan.output_changes.len(), 1);
        assert_eq!(plan.output_changes[0].actions, vec![PlanAction::Update]);
        assert_eq!(plan.output_changes[0].before, Some(PlanValue::Null));
        assert_eq!(
            plan.output_changes[0].after,
            Some(PlanValue::String("synthetic-endpoint".to_owned()))
        );
    }

    #[test]
    fn retains_unsupported_resource_changes_apart_from_output_changes() {
        let mut document = json!({
            "format_version": "1.0",
            "resource_changes": [
                resource("aws_instance.create", "managed", json!(["create"])),
                resource("aws_instance.read", "managed", json!(["read"])),
                resource("aws_instance.update", "managed", json!(["update"])),
                resource("aws_instance.move", "managed", json!(["move"])),
                resource("aws_instance.import", "managed", json!(["import"])),
                resource("aws_instance.unknown", "managed", json!(["future-action"]))
            ],
            "output_changes": {
                "public_ip": {"actions": ["update"], "before": null, "after": "synthetic"}
            }
        });
        document["extra_future_field"] = json!({"accepted": true});

        let plan = parse_plan_json(&document.to_string()).expect("plan should parse");

        assert_eq!(plan.resource_changes.len(), 6);
        assert_eq!(
            plan.resource_changes
                .iter()
                .map(|change| change.kind)
                .collect::<Vec<_>>(),
            vec![
                ResourceChangeKind::Create,
                ResourceChangeKind::Read,
                ResourceChangeKind::Update,
                ResourceChangeKind::Move,
                ResourceChangeKind::Import,
                ResourceChangeKind::Unknown,
            ]
        );
        assert_eq!(plan.unsupported_changes.len(), 4);
        assert_eq!(
            plan.unsupported_changes[0].kind,
            UnsupportedChangeKind::Read
        );
        assert_eq!(
            plan.unsupported_changes[1].kind,
            UnsupportedChangeKind::Move
        );
        assert_eq!(
            plan.unsupported_changes[2].kind,
            UnsupportedChangeKind::Import
        );
        assert_eq!(
            plan.unsupported_changes[3].kind,
            UnsupportedChangeKind::UnknownAction
        );
        assert_eq!(plan.output_changes.len(), 1);
        assert_eq!(plan.output_changes[0].address, "public_ip");
    }

    #[test]
    fn retains_deferred_resources_and_action_invocations_as_unsupported_changes() {
        let input = json!({
            "format_version": "1.2",
            "resource_changes": [],
            "deferred_changes": [{
                "reason": "resource_config_unknown",
                "resource_change": resource(
                    "aws_instance.deferred",
                    "managed",
                    json!(["create"])
                )
            }],
            "action_invocations": [{
                "address": "aws_instance.api.action",
                "type": "notify",
                "name": "notify"
            }],
            "deferred_action_invocations": [{
                "reason": "deferred_prereq",
                "action_invocation": {
                    "address": "aws_instance.deferred_action",
                    "type": "notify",
                    "name": "notify"
                }
            }]
        });

        let plan = parse_plan_json(&input.to_string()).expect("extended plan should parse");

        assert!(plan.resource_changes.is_empty());
        assert_eq!(plan.unsupported_changes.len(), 3);

        let deferred = &plan.unsupported_changes[0];
        assert_eq!(deferred.scope, UnsupportedChangeScope::DeferredResource);
        assert_eq!(deferred.kind, UnsupportedChangeKind::Deferred);
        assert_eq!(deferred.address, "aws_instance.deferred");
        assert_eq!(deferred.actions, vec![PlanAction::Create]);
        assert_eq!(deferred.reason.as_deref(), Some("resource_config_unknown"));
        assert_eq!(deferred.action_type, None);

        let action = &plan.unsupported_changes[1];
        assert_eq!(action.scope, UnsupportedChangeScope::ActionInvocation);
        assert_eq!(action.kind, UnsupportedChangeKind::ActionInvocation);
        assert_eq!(action.address, "aws_instance.api.action");
        assert!(action.actions.is_empty());
        assert_eq!(action.reason, None);
        assert_eq!(action.action_type.as_deref(), Some("notify"));

        let deferred_action = &plan.unsupported_changes[2];
        assert_eq!(
            deferred_action.kind,
            UnsupportedChangeKind::DeferredActionInvocation
        );
        assert_eq!(deferred_action.address, "aws_instance.deferred_action");
        assert_eq!(deferred_action.reason.as_deref(), Some("deferred_prereq"));
        assert_eq!(deferred_action.action_type.as_deref(), Some("notify"));
    }

    #[test]
    fn accepts_omitted_resource_changes_for_an_output_only_plan() {
        let input = json!({
            "format_version": "1.2",
            "output_changes": {
                "public_ip": {"actions": ["update"], "before": null, "after": "synthetic"}
            }
        });

        let plan = parse_plan_json(&input.to_string()).expect("output-only plan should parse");

        assert!(plan.resource_changes.is_empty());
        assert!(plan.unsupported_changes.is_empty());
        assert_eq!(plan.output_changes.len(), 1);
        assert_eq!(plan.output_changes[0].actions, vec![PlanAction::Update]);
    }

    #[test]
    fn separates_changed_drift_from_unsupported_changes() {
        let input = json!({
            "format_version": "1.2",
            "resource_changes": [],
            "resource_drift": [
                resource("aws_instance.drifted", "managed", json!(["update"])),
                resource("aws_instance.deleted", "managed", json!(["delete"])),
                resource("aws_instance.unchanged", "managed", json!(["no-op"]))
            ],
            "output_changes": null
        });

        let plan = parse_plan_json(&input.to_string()).expect("plan should parse");

        assert_eq!(
            plan.drifted_resources,
            ["aws_instance.drifted", "aws_instance.deleted"]
        );
        assert!(plan.unsupported_changes.is_empty());
    }

    #[test]
    fn preserves_numeric_replacement_path_steps() {
        let mut resource = resource("aws_instance.api", "managed", json!(["replace"]));
        resource["change"]["actions"] = json!(["delete", "create"]);
        resource["change"]["replace_paths"] = json!([["disks", 0, "size"]]);

        let plan =
            parse_plan_json(&plan_with_resources(json!([resource]))).expect("plan should parse");

        assert_eq!(
            plan.resource_changes[0].replace_paths,
            Some(vec![vec![
                ReplacePathSegment::Attribute("disks".to_owned()),
                ReplacePathSegment::Index(0),
                ReplacePathSegment::Attribute("size".to_owned()),
            ]])
        );
    }

    #[test]
    fn preserves_arbitrary_precision_json_numbers() {
        let cases = [
            (
                "large_integer",
                "123456789012345678901234567891",
                "123456789012345678901234567891",
            ),
            (
                "precise_decimal",
                "0.123456789012345678901234567890",
                "0.123456789012345678901234567890",
            ),
            ("large_exponent", "1e400", "1e+400"),
        ];

        for (name, source, expected) in cases {
            let mut resource = resource(
                &format!("terraform_data.{name}"),
                "managed",
                json!(["update"]),
            );
            resource["change"]["after"] = serde_json::from_str(source)
                .unwrap_or_else(|error| panic!("case {name}: number should parse: {error}"));

            let plan = parse_plan_json(&plan_with_resources(json!([resource])))
                .unwrap_or_else(|error| panic!("case {name}: plan should parse: {error}"));
            let Some(PlanValue::Number(actual)) = &plan.resource_changes[0].after else {
                panic!("case {name}: number should remain a number");
            };
            assert_eq!(actual, expected, "case: {name}");
        }
    }

    #[test]
    fn accepts_empty_plan_and_plan_with_only_noop_resources() {
        let empty =
            parse_plan_json(&plan_with_resources(json!([]))).expect("empty plan should parse");
        assert!(empty.resource_changes.is_empty());
        assert!(empty.unsupported_changes.is_empty());
        assert!(empty.output_changes.is_empty());

        let noops = parse_plan_json(&plan_with_resources(json!([resource(
            "aws_instance.noop",
            "managed",
            json!(["no-op"])
        )])))
        .expect("no-op plan should parse");
        assert_eq!(noops.summary().total(), 0);
        assert!(
            noops
                .resource_changes
                .iter()
                .all(|change| !change.kind.is_standard_change())
        );
        assert!(noops.unsupported_changes.is_empty());
    }

    #[test]
    fn accepts_null_optional_sections_from_terraform_show() {
        let mut resource = resource("terraform_data.api", "managed", json!(["update"]));
        resource["change"]["replace_paths"] = Value::Null;

        let input = json!({
            "format_version": "1.2",
            "resource_changes": [resource],
            "output_changes": null
        });

        let plan = parse_plan_json(&input.to_string()).expect("Terraform plan should parse");

        assert_eq!(plan.resource_changes.len(), 1);
        assert_eq!(plan.resource_changes[0].replace_paths, None);
        assert!(plan.unsupported_changes.is_empty());
    }

    #[test]
    fn reports_schema_and_version_errors_without_attribute_values() {
        let missing_actions = plan_with_resources(json!([{
            "address": "aws_instance.secret",
            "mode": "managed",
            "change": {"after": "synthetic-secret"}
        }]));
        let error = parse_plan_json(&missing_actions).expect_err("missing actions should fail");
        assert_eq!(
            error,
            PlanParseError::MissingField("resource change actions")
        );
        assert!(!error.to_string().contains("synthetic-secret"));

        let invalid_version = serde_json::json!({
            "format_version": "2.0",
            "resource_changes": []
        });
        assert_eq!(
            parse_plan_json(&invalid_version.to_string()),
            Err(PlanParseError::UnsupportedFormatMajor(2))
        );

        assert_eq!(
            parse_plan_json("not json"),
            Err(PlanParseError::InvalidJson)
        );
        let empty = parse_plan_json(r#"{"format_version":"1.0"}"#)
            .expect("resource_changes may be omitted for an empty plan");
        assert!(empty.resource_changes.is_empty());
        assert!(empty.unsupported_changes.is_empty());
        assert!(empty.output_changes.is_empty());
        assert_eq!(
            parse_plan_json(r#"{"resource_changes":[]}"#),
            Err(PlanParseError::MissingField("format_version"))
        );
    }
    mod presence {
        use super::*;

        #[test]
        fn indexes_prior_and_planned_modules_without_retaining_values() {
            let input = json!({
                "format_version": "1.2",
                "prior_state": {"values": {"root_module": {
                    "resources": [{"address": "test_resource.prior", "values": {"token": "synthetic-secret"}}],
                    "child_modules": [{"resources": [{"address": "module.child[0].test_resource.item"}]}]
                }}},
                "planned_values": {"root_module": {
                    "resources": [{"address": "test_resource.planned"}],
                    "child_modules": [{"child_modules": [{"resources": [{"address": "module.outer.module.inner.test_resource.item"}]}]}]
                }}
            });

            let plan = parse_plan_document(&input).unwrap();

            assert_eq!(
                plan.value_addresses,
                BTreeSet::from([
                    "test_resource.prior".to_owned(),
                    "test_resource.planned".to_owned(),
                    "module.child[0].test_resource.item".to_owned(),
                    "module.outer.module.inner.test_resource.item".to_owned(),
                ])
            );
            assert!(plan.resource_changes.is_empty());
            assert!(!format!("{plan:?}").contains("synthetic-secret"));
        }

        #[rstest::rstest]
        #[case::invalid_prior(json!({"prior_state": "synthetic-secret"}))]
        #[case::invalid_values(json!({"planned_values": []}))]
        #[case::invalid_module(json!({"planned_values": {"root_module": false}}))]
        #[case::invalid_children(json!({"planned_values": {"root_module": {"child_modules": [false]}}}))]
        #[case::missing_address(json!({"planned_values": {"root_module": {"resources": [{"values": "synthetic-secret"}]}}}))]
        fn rejects_broken_presence_data_without_exposing_values(#[case] mut input: Value) {
            input["format_version"] = json!("1.2");

            let error = parse_plan_document(&input).unwrap_err();

            assert!(!format!("{error:?}").contains("synthetic-secret"));
        }
    }
}
