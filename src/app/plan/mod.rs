use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Debug, Formatter};

mod attribute_diff;
pub(crate) mod comparison;
pub(crate) mod grouping;
mod number;
mod relations;
mod relations_graph;
use grouping::PlanGrouping;
pub(crate) mod path;
pub(crate) use relations::{
    ConfigurationRelationStatus, PlanRelations, RelationEndpoint, RelationEvidence, RelationSource,
    RelationUnresolvedReason, StateRelationStatus,
};
pub(crate) use relations_graph::{
    RelationGraph, RelationGraphGroup, RelationGraphLink, RelationGraphLinkKind, RelationNode,
    RelationNodeId, RelationNodeInput,
};
// Screens read prepared graphs; only app builds them, once per review or comparison selection.
pub(in crate::app) use relations_graph::build_relation_graph;

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum PlanValue {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

impl Debug for PlanValue {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReplacePathSegment {
    Attribute(String),
    Index(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceMode {
    Managed,
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum PlanAction {
    Create,
    Read,
    Update,
    Delete,
    NoOp,
    Unknown(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceChangeKind {
    Create,
    Update,
    Replace,
    Delete,
    NoOp,
    Read,
    Move,
    Import,
    Unknown,
    Unsupported,
}

impl ResourceChangeKind {
    #[must_use]
    pub(crate) const fn is_standard_change(self) -> bool {
        matches!(
            self,
            Self::Create | Self::Update | Self::Replace | Self::Delete
        )
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ResourceChange {
    pub(crate) address: String,
    pub(crate) provider: Option<String>,
    pub(crate) resource_type: Option<String>,
    pub(crate) resource_name: Option<String>,
    pub(crate) mode: ResourceMode,
    pub(crate) actions: Vec<PlanAction>,
    pub(crate) kind: ResourceChangeKind,
    pub(crate) before: Option<PlanValue>,
    pub(crate) after: Option<PlanValue>,
    pub(crate) before_sensitive: Option<PlanValue>,
    pub(crate) after_sensitive: Option<PlanValue>,
    pub(crate) after_unknown: Option<PlanValue>,
    pub(crate) replace_paths: Option<Vec<Vec<ReplacePathSegment>>>,
    pub(crate) action_reason: Option<String>,
    pub(crate) previous_address: Option<String>,
    pub(crate) importing: Option<PlanValue>,
}

impl Debug for ResourceChange {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResourceChange")
            .field("address", &self.address)
            .field("provider", &self.provider)
            .field("resource_type", &self.resource_type)
            .field("resource_name", &self.resource_name)
            .field("mode", &self.mode)
            .field("actions", &self.actions)
            .field("kind", &self.kind)
            .field("before", &self.before.as_ref().map(|_| "<redacted>"))
            .field("after", &self.after.as_ref().map(|_| "<redacted>"))
            .field(
                "before_sensitive",
                &self.before_sensitive.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "after_sensitive",
                &self.after_sensitive.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "after_unknown",
                &self.after_unknown.as_ref().map(|_| "<redacted>"),
            )
            .field("replace_paths", &self.replace_paths)
            .field("action_reason", &self.action_reason)
            .field("previous_address", &self.previous_address)
            .field("importing", &self.importing.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct PlanSummary {
    pub(crate) creates: usize,
    pub(crate) updates: usize,
    pub(crate) replaces: usize,
    pub(crate) deletes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnsupportedChangeScope {
    Resource,
    DeferredResource,
    ActionInvocation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnsupportedChangeKind {
    Read,
    Move,
    Import,
    UnknownAction,
    UnsupportedActions,
    Deferred,
    ActionInvocation,
    DeferredActionInvocation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnsupportedChange {
    pub(crate) scope: UnsupportedChangeScope,
    pub(crate) address: String,
    pub(crate) actions: Vec<PlanAction>,
    pub(crate) kind: UnsupportedChangeKind,
    pub(crate) reason: Option<String>,
    pub(crate) action_type: Option<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct OutputChange {
    pub(crate) address: String,
    pub(crate) actions: Vec<PlanAction>,
    pub(crate) before: Option<PlanValue>,
    pub(crate) after: Option<PlanValue>,
    pub(crate) before_sensitive: Option<PlanValue>,
    pub(crate) after_sensitive: Option<PlanValue>,
    pub(crate) after_unknown: Option<PlanValue>,
}

impl std::fmt::Debug for OutputChange {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutputChange")
            .field("address", &self.address)
            .field("actions", &self.actions)
            .field("before", &self.before.as_ref().map(|_| "<redacted>"))
            .field("after", &self.after.as_ref().map(|_| "<redacted>"))
            .field(
                "before_sensitive",
                &self.before_sensitive.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "after_sensitive",
                &self.after_sensitive.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "after_unknown",
                &self.after_unknown.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) resource_changes: Vec<ResourceChange>,
    pub(crate) value_addresses: BTreeSet<String>,
    pub(crate) unsupported_changes: Vec<UnsupportedChange>,
    pub(crate) output_changes: Vec<OutputChange>,
    pub(crate) drifted_resources: Vec<String>,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Plan")
            .field("resource_changes", &self.resource_changes)
            .field("value_addresses", &self.value_addresses)
            .field("unsupported_changes", &self.unsupported_changes)
            .field("output_changes", &self.output_changes)
            .field("drifted_resources", &self.drifted_resources)
            .finish()
    }
}

impl Plan {
    #[must_use]
    pub(crate) fn summary(&self) -> PlanSummary {
        let mut summary = PlanSummary::default();
        for change in &self.resource_changes {
            match change.kind {
                ResourceChangeKind::Create => summary.creates += 1,
                ResourceChangeKind::Update => summary.updates += 1,
                ResourceChangeKind::Replace => summary.replaces += 1,
                ResourceChangeKind::Delete => summary.deletes += 1,
                ResourceChangeKind::NoOp
                | ResourceChangeKind::Read
                | ResourceChangeKind::Move
                | ResourceChangeKind::Import
                | ResourceChangeKind::Unknown
                | ResourceChangeKind::Unsupported => {}
            }
        }
        summary
    }

    #[must_use]
    pub(in crate::app) fn grouped_changes(
        &self,
        schemas: Option<&ProviderSchemas>,
    ) -> PlanGrouping {
        grouping::group_resource_changes(&self.resource_changes, schemas)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum AttributeType {
    Bool,
    Number,
    String,
    List(Box<Self>),
    Set(Box<Self>),
    Map(Box<Self>),
    Tuple(Vec<Self>),
    Object(BTreeMap<String, Self>),
    Dynamic,
}

impl AttributeType {
    #[must_use]
    pub(crate) const fn is_simple_value(&self) -> bool {
        matches!(self, Self::Bool | Self::Number | Self::String)
    }

    #[must_use]
    pub(crate) fn is_simple_map(&self) -> bool {
        matches!(self, Self::Map(value) if value.is_simple_value())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceSchema {
    pub(crate) attributes: BTreeMap<String, AttributeType>,
    pub(crate) block_types: BTreeMap<String, AttributeType>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderSchema {
    pub(crate) resources: BTreeMap<String, ResourceSchema>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderSchemas {
    pub(crate) providers: BTreeMap<String, ProviderSchema>,
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::collections::BTreeSet;

    use super::{OutputChange, Plan, PlanAction, ResourceChange, ResourceChangeKind, ResourceMode};

    impl Plan {
        pub(crate) const fn empty() -> Self {
            Self {
                resource_changes: Vec::new(),
                value_addresses: BTreeSet::new(),
                unsupported_changes: Vec::new(),
                output_changes: Vec::new(),
                drifted_resources: Vec::new(),
            }
        }
    }

    pub(crate) fn output_change(address: &str, action: PlanAction) -> OutputChange {
        OutputChange {
            address: address.to_owned(),
            actions: vec![action],
            before: None,
            after: None,
            before_sensitive: None,
            after_sensitive: None,
            after_unknown: None,
        }
    }

    /// A resource change whose actions match `kind`, without values or move/import markers.
    pub(crate) fn resource_change(address: &str, kind: ResourceChangeKind) -> ResourceChange {
        let actions = match kind {
            ResourceChangeKind::Create => vec![PlanAction::Create],
            ResourceChangeKind::Update => vec![PlanAction::Update],
            ResourceChangeKind::Replace => vec![PlanAction::Delete, PlanAction::Create],
            ResourceChangeKind::Delete => vec![PlanAction::Delete],
            ResourceChangeKind::Read => vec![PlanAction::Read],
            ResourceChangeKind::NoOp
            | ResourceChangeKind::Move
            | ResourceChangeKind::Import
            | ResourceChangeKind::Unknown
            | ResourceChangeKind::Unsupported => vec![PlanAction::NoOp],
        };
        ResourceChange {
            address: address.to_owned(),
            provider: None,
            resource_type: None,
            resource_name: None,
            mode: ResourceMode::Managed,
            actions,
            kind,
            before: None,
            after: None,
            before_sensitive: None,
            after_sensitive: None,
            after_unknown: None,
            replace_paths: None,
            action_reason: None,
            previous_address: None,
            importing: None,
        }
    }
}
