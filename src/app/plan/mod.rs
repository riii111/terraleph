use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{Debug, Formatter};

mod attribute_diff;
pub(crate) mod comparison;
pub(crate) mod grouping;
mod number;
pub(crate) mod path;
mod relations;
mod relations_graph;
pub(crate) use relations::{
    ConfigurationRelationStatus, PlanRelations, RelationEndpoint, RelationEvidence, RelationSource,
    RelationUnresolvedReason, StateRelationStatus,
};
pub(crate) use relations_graph::{
    RelationGraph, RelationGraphGroup, RelationGraphLink, RelationGraphLinkKind, RelationNode,
    RelationNodeId,
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

impl PlanValue {
    /// Whether a sensitivity or unknown marker marks this value or any value nested in it.
    fn marks_any(&self) -> bool {
        match self {
            Self::Bool(value) => *value,
            Self::Array(values) => values.iter().any(Self::marks_any),
            Self::Object(values) => values.values().any(Self::marks_any),
            Self::Null | Self::Number(_) | Self::String(_) => false,
        }
    }
}

impl Debug for PlanValue {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResourceChange {
    pub(crate) address: String,
    pub(crate) provider: Option<String>,
    pub(crate) resource_type: Option<String>,
    pub(crate) mode: ResourceMode,
    pub(crate) actions: Vec<PlanAction>,
    pub(crate) kind: ResourceChangeKind,
    pub(crate) before: Option<PlanValue>,
    pub(crate) after: Option<PlanValue>,
    pub(crate) before_sensitive: Option<PlanValue>,
    pub(crate) after_sensitive: Option<PlanValue>,
    pub(crate) after_unknown: Option<PlanValue>,
    pub(crate) previous_address: Option<String>,
    pub(crate) importing: Option<PlanValue>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutputChange {
    pub(crate) address: String,
    pub(crate) actions: Vec<PlanAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Plan {
    pub(crate) resource_changes: Vec<ResourceChange>,
    pub(crate) value_addresses: BTreeSet<String>,
    pub(crate) unsupported_changes: Vec<UnsupportedChange>,
    pub(crate) output_changes: Vec<OutputChange>,
    pub(crate) drifted_resources: Vec<String>,
}

impl Plan {
    #[must_use]
    pub(crate) const fn empty() -> Self {
        Self {
            resource_changes: Vec::new(),
            value_addresses: BTreeSet::new(),
            unsupported_changes: Vec::new(),
            output_changes: Vec::new(),
            drifted_resources: Vec::new(),
        }
    }

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

impl ProviderSchemas {
    fn resource(&self, change: &ResourceChange) -> Option<&ResourceSchema> {
        self.providers
            .get(change.provider.as_ref()?)?
            .resources
            .get(change.resource_type.as_ref()?)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{OutputChange, PlanAction, ResourceChange, ResourceChangeKind, ResourceMode};

    pub(crate) fn output_change(address: &str, action: PlanAction) -> OutputChange {
        OutputChange {
            address: address.to_owned(),
            actions: vec![action],
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
            mode: ResourceMode::Managed,
            actions,
            kind,
            before: None,
            after: None,
            before_sensitive: None,
            after_sensitive: None,
            after_unknown: None,
            previous_address: None,
            importing: None,
        }
    }
}
