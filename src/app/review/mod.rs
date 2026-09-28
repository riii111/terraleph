use std::{
    collections::BTreeMap,
    fmt::{Debug, Formatter},
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use super::{
    execution::{
        ApplyStatus, Diagnostic, ExecutionContext, ExecutionContextValue, ExecutionEvent,
        ExecutionTargetSpec, SensitiveValue,
    },
    plan::{Plan, PlanAction, PlanRelations, PlanSummary, ProviderSchemas, ResourceChangeKind},
};

#[cfg(test)]
pub(crate) mod git;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanBlockKind {
    Common,
    Resource,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanLineKind {
    Body,
    Intro,
    Note,
    /// The `# <address> will be ...` line that starts a resource block.
    ResourceHeader,
    /// A line inside a heredoc value. Only a `+`, `-`, or `~` at `marker_column`, two columns right
    /// of the opening attribute name or list element, marks a changed line; the rest is value text.
    /// The column is kept small because every plan line stores its kind; a column past
    /// `u16::MAX` is stored as `u16::MAX` and marks no line.
    HeredocBody {
        marker_column: u16,
    },
    Summary,
    OutputSection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanBlock {
    lines: Range<usize>,
    kind: PlanBlockKind,
    addresses: Vec<String>,
}

impl PlanBlock {
    #[must_use]
    pub(crate) const fn new(lines: Range<usize>, kind: PlanBlockKind) -> Self {
        Self {
            lines,
            kind,
            addresses: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) const fn with_addresses(
        lines: Range<usize>,
        kind: PlanBlockKind,
        addresses: Vec<String>,
    ) -> Self {
        Self {
            lines,
            kind,
            addresses,
        }
    }

    #[must_use]
    pub(crate) const fn lines(&self) -> &Range<usize> {
        &self.lines
    }

    pub(crate) const fn lines_mut(&mut self) -> &mut Range<usize> {
        &mut self.lines
    }

    #[must_use]
    pub(crate) const fn is_common(&self) -> bool {
        matches!(self.kind, PlanBlockKind::Common)
    }

    #[must_use]
    pub(crate) fn addresses(&self) -> &[String] {
        &self.addresses
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PlanDocument {
    // Shared so clones keep one text and a view can tell whether its derived work still belongs
    // to this document.
    text: Arc<str>,
    // Byte offsets of each '\n'-separated line, counted once instead of on every frame.
    line_starts: Vec<usize>,
    blocks: Vec<PlanBlock>,
    line_kinds: Vec<PlanLineKind>,
    address_blocks: BTreeMap<String, usize>,
}

struct FilteredLine<'a> {
    line_index: usize,
    text: &'a str,
}

pub(crate) struct FilteredPlan<'a> {
    lines: Vec<FilteredLine<'a>>,
    matching_resources: usize,
    matching_outputs: usize,
}

impl<'a> FilteredPlan<'a> {
    pub(crate) fn lines_with_indices(&self) -> impl Iterator<Item = (usize, &'a str)> + '_ {
        self.lines.iter().map(|line| (line.line_index, line.text))
    }

    #[must_use]
    pub(crate) const fn matching_resources(&self) -> usize {
        self.matching_resources
    }

    #[must_use]
    pub(crate) const fn matching_outputs(&self) -> usize {
        self.matching_outputs
    }
}

impl PlanDocument {
    #[must_use]
    pub(crate) fn with_blocks_and_line_kinds(
        text: String,
        blocks: Vec<PlanBlock>,
        line_kinds: Vec<PlanLineKind>,
    ) -> Self {
        let mut address_blocks = BTreeMap::new();
        for (index, block) in blocks.iter().enumerate() {
            for address in block.addresses() {
                address_blocks.entry(address.clone()).or_insert(index);
            }
        }
        let line_starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(index, _)| index + 1))
            .collect();
        Self {
            text: text.into(),
            line_starts,
            blocks,
            line_kinds,
            address_blocks,
        }
    }

    #[must_use]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Returns the number of '\n'-separated lines, so an empty text still has one line.
    #[must_use]
    pub(crate) const fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Returns one '\n'-separated line without its separator.
    ///
    /// # Panics
    ///
    /// Panics when `index` is not below [`Self::line_count`].
    #[must_use]
    pub(crate) fn line(&self, index: usize) -> &str {
        let start = self.line_starts[index];
        let end = self
            .line_starts
            .get(index + 1)
            .map_or(self.text.len(), |next| next - 1);
        &self.text[start..end]
    }

    #[must_use]
    pub(crate) fn key(&self) -> PlanDocumentKey {
        PlanDocumentKey(Arc::clone(&self.text))
    }

    #[must_use]
    pub(crate) fn filter(&self, query: &str) -> FilteredPlan<'_> {
        let mut filtered = Vec::new();
        let mut matching_resources = 0;
        let mut matching_outputs = 0;
        for block in &self.blocks {
            let matches = query.is_empty()
                || block.is_common()
                || block
                    .lines()
                    .clone()
                    .any(|line| self.line(line).contains(query));
            if !matches {
                continue;
            }
            match block.kind {
                PlanBlockKind::Resource => matching_resources += 1,
                PlanBlockKind::Output => matching_outputs += 1,
                PlanBlockKind::Common => {}
            }
            for line in block.lines().clone() {
                filtered.push(FilteredLine {
                    line_index: line,
                    text: self.line(line),
                });
            }
        }
        FilteredPlan {
            lines: filtered,
            matching_resources,
            matching_outputs,
        }
    }

    #[must_use]
    pub(crate) fn line_kind(&self, line: usize) -> PlanLineKind {
        self.line_kinds
            .get(line)
            .copied()
            .unwrap_or(PlanLineKind::Body)
    }

    #[must_use]
    pub(crate) fn block_for_address(&self, address: &str) -> Option<&PlanBlock> {
        self.address_blocks
            .get(address)
            .and_then(|index| self.blocks.get(*index))
    }
}

/// Identifies the text of one document for work derived from it. Clones of a document share the
/// key; holding it keeps the text alive, so a later document cannot reuse its allocation.
#[derive(Clone)]
pub(crate) struct PlanDocumentKey(Arc<str>);

impl PlanDocumentKey {
    #[must_use]
    pub(crate) fn is_for(&self, document: &PlanDocument) -> bool {
        Arc::ptr_eq(&self.0, &document.text)
    }
}

impl Debug for PlanDocument {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlanDocument")
            .field("text", &"<redacted>")
            .finish()
    }
}

/// Plan facts that the resource changes cannot reproduce.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PlanMetadata {
    applyable: bool,
    sensitive_values: Vec<SensitiveValue>,
}

impl Debug for PlanMetadata {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PlanMetadata")
            .field("applyable", &self.applyable)
            .field("sensitive_values", &"<redacted>")
            .finish()
    }
}

impl PlanMetadata {
    #[must_use]
    pub(crate) const fn new(applyable: bool) -> Self {
        Self {
            applyable,
            sensitive_values: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) fn with_sensitive_values(mut self, sensitive_values: Vec<SensitiveValue>) -> Self {
        self.sensitive_values = sensitive_values;
        self
    }

    #[must_use]
    pub(crate) const fn applyable(&self) -> bool {
        self.applyable
    }

    #[must_use]
    pub(crate) fn sensitive_values(&self) -> &[SensitiveValue] {
        &self.sensitive_values
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanReview {
    root: PathBuf,
    workspace: String,
    context: ExecutionContext,
    document: PlanDocument,
    metadata: PlanMetadata,
    plan: Plan,
    relations: PlanRelations,
    provider_schemas: Option<ProviderSchemas>,
    diagnostics: Vec<Diagnostic>,
    search_query: String,
    apply_allowed: bool,
    apply_entry: bool,
    previous_durations: Vec<Option<Duration>>,
}

impl PlanReview {
    #[must_use]
    pub(crate) fn new(
        root: PathBuf,
        workspace: String,
        document: PlanDocument,
        plan: Plan,
        metadata: PlanMetadata,
        diagnostics: Vec<Diagnostic>,
    ) -> Self {
        let context = ExecutionContext::loading(&root).with_workspace(workspace.clone());
        Self {
            root,
            workspace,
            context,
            document,
            metadata,
            plan,
            relations: PlanRelations::not_collected(),
            provider_schemas: None,
            diagnostics,
            search_query: String::new(),
            apply_allowed: true,
            apply_entry: false,
            previous_durations: Vec::new(),
        }
    }

    #[must_use]
    pub(crate) const fn with_apply_allowed(mut self, allowed: bool) -> Self {
        self.apply_allowed = allowed;
        self
    }

    #[must_use]
    pub(crate) const fn with_apply_entry(mut self, apply_entry: bool) -> Self {
        self.apply_entry = apply_entry;
        self
    }

    #[must_use]
    pub(crate) fn with_previous_durations(
        mut self,
        previous_durations: Vec<Option<Duration>>,
    ) -> Self {
        self.previous_durations = previous_durations;
        self
    }

    #[must_use]
    pub(crate) const fn apply_allowed(&self) -> bool {
        self.apply_allowed
    }

    #[must_use]
    pub(crate) const fn apply_entry(&self) -> bool {
        self.apply_entry
    }

    #[must_use]
    pub(crate) fn previous_durations(&self) -> &[Option<Duration>] {
        &self.previous_durations
    }

    #[must_use]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub(crate) fn workspace(&self) -> &str {
        &self.workspace
    }

    #[must_use]
    pub(crate) const fn context(&self) -> &ExecutionContext {
        &self.context
    }

    #[must_use]
    pub(crate) fn with_context(mut self, context: ExecutionContext) -> Self {
        self.context = context;
        self
    }

    #[must_use]
    pub(crate) fn with_relations(mut self, relations: PlanRelations) -> Self {
        self.relations = relations;
        self
    }

    #[must_use]
    pub(crate) fn with_provider_schemas(mut self, schemas: Option<ProviderSchemas>) -> Self {
        self.provider_schemas = schemas;
        self
    }

    #[must_use]
    pub(crate) fn confirmation_input(&self) -> String {
        let named = self.has_destructive_changes() || self.context.is_production() == Some(true);
        if named {
            match self.context.display_name() {
                ExecutionContextValue::Known(name) => name.clone(),
                ExecutionContextValue::Loading => String::new(),
            }
        } else {
            "yes".to_owned()
        }
    }

    #[must_use]
    pub(crate) const fn document(&self) -> &PlanDocument {
        &self.document
    }

    #[must_use]
    pub(crate) const fn metadata(&self) -> &PlanMetadata {
        &self.metadata
    }

    #[must_use]
    pub(crate) const fn plan(&self) -> &Plan {
        &self.plan
    }

    #[must_use]
    pub(crate) fn summary(&self) -> PlanSummary {
        self.plan.summary()
    }

    /// Outputs are counted separately, so callers can add both counts without double counting.
    #[must_use]
    pub(crate) fn nonstandard_changes(&self) -> usize {
        let planned_drift = if self.drift_is_planned() {
            self.plan.drifted_resources.len()
        } else {
            0
        };
        self.plan.unsupported_changes.len() + planned_drift
    }

    /// Terraform lists every output in a plan, including unchanged ones.
    #[must_use]
    pub(crate) fn changed_outputs(&self) -> usize {
        self.plan
            .output_changes
            .iter()
            .filter(|output| output.actions != [PlanAction::NoOp])
            .count()
    }

    #[must_use]
    pub(crate) fn has_changes(&self) -> bool {
        self.has_changes_besides_drift() || self.drift_is_planned()
    }

    #[must_use]
    pub(crate) fn noted_drift(&self) -> usize {
        if self.drift_is_planned() {
            0
        } else {
            self.plan.drifted_resources.len()
        }
    }

    /// The plan JSON carries no planning mode, so drift is judged from applyability only when
    /// nothing else is planned.
    fn drift_is_planned(&self) -> bool {
        !self.plan.drifted_resources.is_empty()
            && self.metadata.applyable()
            && !self.has_changes_besides_drift()
    }

    fn has_changes_besides_drift(&self) -> bool {
        self.plan
            .resource_changes
            .iter()
            .any(|change| change.kind.is_standard_change())
            || !self.plan.unsupported_changes.is_empty()
            || self.changed_outputs() > 0
    }

    /// Previous durations are looked up and paired with targets by this order.
    #[must_use]
    pub(crate) fn apply_targets(&self) -> Vec<ExecutionTargetSpec> {
        self.plan
            .resource_changes
            .iter()
            .filter(|change| {
                change.kind.is_standard_change()
                    && change.previous_address.is_none()
                    && change.importing.is_none()
            })
            .map(|change| ExecutionTargetSpec {
                address: change.address.clone(),
                actions: change.actions.clone(),
            })
            .collect()
    }

    pub(crate) fn destructive_addresses(&self) -> impl Iterator<Item = &str> {
        self.addresses_of(ResourceChangeKind::Delete)
    }

    pub(crate) fn replacement_addresses(&self) -> impl Iterator<Item = &str> {
        self.addresses_of(ResourceChangeKind::Replace)
    }

    fn addresses_of(&self, kind: ResourceChangeKind) -> impl Iterator<Item = &str> {
        self.plan
            .resource_changes
            .iter()
            .filter(move |change| change.kind == kind)
            .map(|change| change.address.as_str())
    }

    #[must_use]
    pub(crate) fn has_destructive_changes(&self) -> bool {
        self.destructive_addresses().next().is_some()
            || self.replacement_addresses().next().is_some()
    }

    #[must_use]
    pub(crate) const fn relations(&self) -> &PlanRelations {
        &self.relations
    }

    #[must_use]
    pub(crate) const fn provider_schemas(&self) -> Option<&ProviderSchemas> {
        self.provider_schemas.as_ref()
    }

    #[must_use]
    pub(crate) fn with_diagnostics(mut self, diagnostics: Vec<Diagnostic>) -> Self {
        self.diagnostics = diagnostics;
        self
    }

    pub(crate) fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub(crate) fn search_query(&self) -> &str {
        &self.search_query
    }

    pub(crate) fn set_search_query(&mut self, query: String) {
        self.search_query = query;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "review completion carries the complete plan for the UI"
)]
pub(crate) enum PlanReviewMessage {
    Event(ExecutionEvent),
    Completed(PlanReview),
    Failed {
        message: String,
        interrupted: bool,
    },
    ApplyEvent(ExecutionEvent),
    ApplyCompleted {
        status: ApplyStatus,
        summary_line: Option<String>,
    },
    ApplyFailed {
        message: String,
    },
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{PlanBlock, PlanBlockKind, PlanDocument, PlanLineKind};

    pub(crate) fn plan_document_with_blocks(text: String, blocks: Vec<PlanBlock>) -> PlanDocument {
        let line_kinds = vec![PlanLineKind::Body; text.split('\n').count()];
        PlanDocument::with_blocks_and_line_kinds(text, blocks, line_kinds)
    }

    pub(crate) fn plan_document(text: String) -> PlanDocument {
        let end = text.split('\n').count();
        plan_document_with_blocks(text, vec![PlanBlock::new(0..end, PlanBlockKind::Common)])
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{plan_document, plan_document_with_blocks};
    use super::*;

    #[test]
    fn debug_output_never_contains_plan_text() {
        let document = plan_document("password = secret".to_owned());

        let debug = format!("{document:?}");

        assert!(!debug.contains("secret"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn document_reads_the_same_lines_as_splitting_its_text() {
        for text in ["", "single", "first\r\n\n  last\n", "\n\n"] {
            let document = plan_document(text.to_owned());

            assert_eq!(
                (0..document.line_count())
                    .map(|line| document.line(line))
                    .collect::<Vec<_>>(),
                text.split('\n').collect::<Vec<_>>(),
                "{text:?}"
            );
        }
    }

    #[test]
    fn document_key_follows_clones_but_not_rebuilt_documents() {
        let document = plan_document("same text".to_owned());
        let key = document.key();
        let rebuilt = plan_document("same text".to_owned());
        let documents = [document.clone(), document];

        assert!(documents.iter().all(|document| key.is_for(document)));
        assert_eq!(rebuilt, documents[0]);
        assert!(!key.is_for(&rebuilt));
    }

    #[test]
    fn document_keeps_common_lines_and_matching_blocks_in_original_order() {
        let document = plan_document_with_blocks(
            "preamble\nresource api\napi value\nresource worker\nworker value\nsummary\n"
                .to_owned(),
            vec![
                PlanBlock::new(0..1, PlanBlockKind::Common),
                PlanBlock::new(1..3, PlanBlockKind::Resource),
                PlanBlock::new(3..5, PlanBlockKind::Resource),
                PlanBlock::new(5..7, PlanBlockKind::Common),
            ],
        );

        let filtered = document.filter("worker");

        assert_eq!(
            filtered.lines_with_indices().collect::<Vec<_>>(),
            [
                (0, "preamble"),
                (3, "resource worker"),
                (4, "worker value"),
                (5, "summary"),
                (6, ""),
            ]
        );
        assert_eq!(filtered.matching_resources(), 1);
        assert_eq!(filtered.matching_outputs(), 0);
    }

    #[test]
    fn filter_counts_each_resource_and_output_block_once() {
        let document = plan_document_with_blocks(
            "common api\nresource api api api\nresource worker\noutput endpoint\nunknown endpoint\n"
                .to_owned(),
            vec![
                PlanBlock::new(0..1, PlanBlockKind::Common),
                PlanBlock::new(1..2, PlanBlockKind::Resource),
                PlanBlock::new(2..3, PlanBlockKind::Resource),
                PlanBlock::new(3..4, PlanBlockKind::Output),
                PlanBlock::new(4..6, PlanBlockKind::Common),
            ],
        );

        let resource = document.filter("api");
        assert_eq!(
            resource
                .lines_with_indices()
                .map(|(_, line)| line)
                .collect::<Vec<_>>(),
            ["common api", "resource api api api", "unknown endpoint", ""]
        );
        assert_eq!(resource.matching_resources(), 1);
        assert_eq!(resource.matching_outputs(), 0);

        let output = document.filter("endpoint");
        assert_eq!(
            output
                .lines_with_indices()
                .map(|(_, line)| line)
                .collect::<Vec<_>>(),
            ["common api", "output endpoint", "unknown endpoint", ""]
        );
        assert_eq!(output.matching_resources(), 0);
        assert_eq!(output.matching_outputs(), 1);

        let mixed = document.filter("e");
        assert_eq!(mixed.matching_resources(), 2);
        assert_eq!(mixed.matching_outputs(), 1);
        assert_eq!(mixed.matching_resources() + mixed.matching_outputs(), 3);

        let empty = document.filter("");
        assert_eq!(empty.matching_resources(), 2);
        assert_eq!(empty.matching_outputs(), 1);
    }

    #[test]
    fn filter_keeps_common_text_when_no_searchable_block_matches() {
        let document = plan_document_with_blocks(
            "diagnostic only\nresource api\noutput endpoint\nunknown boundary text\n".to_owned(),
            vec![
                PlanBlock::new(0..1, PlanBlockKind::Common),
                PlanBlock::new(1..2, PlanBlockKind::Resource),
                PlanBlock::new(2..3, PlanBlockKind::Output),
                PlanBlock::new(3..5, PlanBlockKind::Common),
            ],
        );

        let filtered = document.filter("missing");

        assert_eq!(
            filtered
                .lines_with_indices()
                .map(|(_, line)| line)
                .collect::<Vec<_>>(),
            ["diagnostic only", "unknown boundary text", ""]
        );
        assert_eq!(
            filtered.matching_resources() + filtered.matching_outputs(),
            0
        );
    }

    mod projection {
        use super::*;
        use crate::app::plan::{
            OutputChange, PlanValue, ResourceChange, UnsupportedChange, UnsupportedChangeKind,
            UnsupportedChangeScope,
            test_support::{output_change, resource_change},
        };

        fn review(plan: Plan, metadata: PlanMetadata) -> PlanReview {
            PlanReview::new(
                PathBuf::from("/project"),
                "default".to_owned(),
                plan_document(String::new()),
                plan,
                metadata,
                Vec::new(),
            )
        }

        fn unsupported_resource(address: &str, kind: UnsupportedChangeKind) -> UnsupportedChange {
            UnsupportedChange {
                scope: UnsupportedChangeScope::Resource,
                address: address.to_owned(),
                actions: vec![PlanAction::NoOp],
                kind,
                reason: None,
                action_type: None,
            }
        }

        fn sensitive_no_op_output() -> OutputChange {
            OutputChange {
                before: Some(PlanValue::String("synthetic-secret".to_owned())),
                after: Some(PlanValue::String("synthetic-secret".to_owned())),
                before_sensitive: Some(PlanValue::Bool(true)),
                after_sensitive: Some(PlanValue::Bool(true)),
                ..output_change("secret", PlanAction::NoOp)
            }
        }

        #[test]
        fn has_changes_counts_nonstandard_and_changed_outputs_but_not_no_ops() {
            struct ChangesCase {
                name: &'static str,
                plan: Plan,
                expected: bool,
            }

            for case in [
                ChangesCase {
                    name: "no_op_resource",
                    plan: Plan {
                        resource_changes: vec![resource_change(
                            "terraform_data.unchanged",
                            ResourceChangeKind::NoOp,
                        )],
                        ..Plan::empty()
                    },
                    expected: false,
                },
                ChangesCase {
                    name: "moved_resource",
                    plan: Plan {
                        resource_changes: vec![ResourceChange {
                            previous_address: Some("terraform_data.previous".to_owned()),
                            ..resource_change("terraform_data.moved", ResourceChangeKind::Move)
                        }],
                        unsupported_changes: vec![unsupported_resource(
                            "terraform_data.moved",
                            UnsupportedChangeKind::Move,
                        )],
                        ..Plan::empty()
                    },
                    expected: true,
                },
                ChangesCase {
                    name: "imported_resource",
                    plan: Plan {
                        resource_changes: vec![ResourceChange {
                            importing: Some(PlanValue::Object(BTreeMap::new())),
                            ..resource_change("terraform_data.imported", ResourceChangeKind::Import)
                        }],
                        unsupported_changes: vec![unsupported_resource(
                            "terraform_data.imported",
                            UnsupportedChangeKind::Import,
                        )],
                        ..Plan::empty()
                    },
                    expected: true,
                },
                ChangesCase {
                    name: "read_resource",
                    plan: Plan {
                        resource_changes: vec![resource_change(
                            "data.terraform_data.read",
                            ResourceChangeKind::Read,
                        )],
                        unsupported_changes: vec![unsupported_resource(
                            "data.terraform_data.read",
                            UnsupportedChangeKind::Read,
                        )],
                        ..Plan::empty()
                    },
                    expected: true,
                },
                ChangesCase {
                    name: "no_op_outputs",
                    plan: Plan {
                        output_changes: vec![
                            output_change("endpoint", PlanAction::NoOp),
                            sensitive_no_op_output(),
                        ],
                        ..Plan::empty()
                    },
                    expected: false,
                },
                ChangesCase {
                    name: "changed_output",
                    plan: Plan {
                        output_changes: vec![
                            output_change("endpoint", PlanAction::Update),
                            sensitive_no_op_output(),
                        ],
                        ..Plan::empty()
                    },
                    expected: true,
                },
            ] {
                let review = review(case.plan, PlanMetadata::new(true));

                assert_eq!(review.has_changes(), case.expected, "case: {}", case.name);
                assert_eq!(
                    review.summary(),
                    PlanSummary::default(),
                    "case: {}",
                    case.name
                );
            }
        }

        #[test]
        fn drift_counts_only_when_alone_applyable() {
            struct DriftCase {
                name: &'static str,
                plan: Plan,
                applyable: bool,
                has_changes: bool,
                nonstandard_changes: usize,
                noted_drift: usize,
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
                    has_changes: false,
                    nonstandard_changes: 0,
                    noted_drift: 1,
                },
                DriftCase {
                    name: "refresh_only_drift_only",
                    plan: drift(Plan::empty()),
                    applyable: true,
                    has_changes: true,
                    nonstandard_changes: 1,
                    noted_drift: 0,
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
                    has_changes: true,
                    nonstandard_changes: 0,
                    noted_drift: 1,
                },
                DriftCase {
                    name: "drift_and_changed_output",
                    plan: drift(Plan {
                        output_changes: vec![output_change("endpoint", PlanAction::Update)],
                        ..Plan::empty()
                    }),
                    applyable: true,
                    has_changes: true,
                    nonstandard_changes: 0,
                    noted_drift: 1,
                },
            ] {
                let review = review(case.plan, PlanMetadata::new(case.applyable));

                assert_eq!(
                    review.has_changes(),
                    case.has_changes,
                    "case: {}",
                    case.name
                );
                assert_eq!(
                    review.nonstandard_changes(),
                    case.nonstandard_changes,
                    "case: {}",
                    case.name
                );
                assert_eq!(
                    review.noted_drift(),
                    case.noted_drift,
                    "case: {}",
                    case.name
                );
            }
        }

        #[test]
        fn changed_outputs_count_created_updated_and_deleted_outputs_only() {
            let review = review(
                Plan {
                    output_changes: vec![
                        output_change("created", PlanAction::Create),
                        output_change("updated", PlanAction::Update),
                        output_change("deleted", PlanAction::Delete),
                        output_change("unchanged", PlanAction::NoOp),
                        sensitive_no_op_output(),
                    ],
                    ..Plan::empty()
                },
                PlanMetadata::new(true),
            );

            assert_eq!(review.changed_outputs(), 3);
        }

        #[test]
        fn apply_targets_keep_plan_order_and_skip_moved_imported_and_nonstandard_resources() {
            let review = review(
                Plan {
                    resource_changes: vec![
                        resource_change("terraform_data.update", ResourceChangeKind::Update),
                        ResourceChange {
                            previous_address: Some("terraform_data.previous".to_owned()),
                            ..resource_change("terraform_data.moved", ResourceChangeKind::Create)
                        },
                        ResourceChange {
                            importing: Some(PlanValue::Null),
                            ..resource_change("terraform_data.imported", ResourceChangeKind::Create)
                        },
                        resource_change("terraform_data.unchanged", ResourceChangeKind::NoOp),
                        resource_change("data.terraform_data.read", ResourceChangeKind::Read),
                        resource_change("terraform_data.replace", ResourceChangeKind::Replace),
                        resource_change("terraform_data.create", ResourceChangeKind::Create),
                    ],
                    ..Plan::empty()
                },
                PlanMetadata::new(true),
            );

            assert_eq!(
                review.apply_targets(),
                [
                    ExecutionTargetSpec {
                        address: "terraform_data.update".to_owned(),
                        actions: vec![PlanAction::Update],
                    },
                    ExecutionTargetSpec {
                        address: "terraform_data.replace".to_owned(),
                        actions: vec![PlanAction::Delete, PlanAction::Create],
                    },
                    ExecutionTargetSpec {
                        address: "terraform_data.create".to_owned(),
                        actions: vec![PlanAction::Create],
                    },
                ]
            );
        }

        #[test]
        fn destructive_lists_separate_deletes_from_replacements_and_name_the_target() {
            let review = review(
                Plan {
                    resource_changes: vec![
                        resource_change("terraform_data.replace", ResourceChangeKind::Replace),
                        resource_change("terraform_data.update", ResourceChangeKind::Update),
                        resource_change("terraform_data.destroy", ResourceChangeKind::Delete),
                        ResourceChange {
                            previous_address: Some("terraform_data.previous".to_owned()),
                            ..resource_change("terraform_data.moved", ResourceChangeKind::Move)
                        },
                    ],
                    ..Plan::empty()
                },
                PlanMetadata::new(true),
            );

            assert_eq!(
                review.destructive_addresses().collect::<Vec<_>>(),
                ["terraform_data.destroy"]
            );
            assert_eq!(
                review.replacement_addresses().collect::<Vec<_>>(),
                ["terraform_data.replace"]
            );
            assert_eq!(review.confirmation_input(), "project");
        }

        #[test]
        fn non_destructive_plan_confirms_with_yes() {
            let review = review(
                Plan {
                    resource_changes: vec![
                        resource_change("terraform_data.create", ResourceChangeKind::Create),
                        resource_change("terraform_data.update", ResourceChangeKind::Update),
                    ],
                    ..Plan::empty()
                },
                PlanMetadata::new(true),
            );

            assert_eq!(review.confirmation_input(), "yes");
        }
    }
}
