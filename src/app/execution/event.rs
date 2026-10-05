use std::fmt::{Debug, Formatter};
use std::time::Instant;

use crate::app::plan::PlanAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventStream {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResourceEventKind {
    RefreshStart,
    RefreshComplete,
    ApplyStart,
    ApplyProgress,
    ApplyComplete,
    ApplyErrored,
    ProvisionStart,
    ProvisionProgress,
    ProvisionComplete,
    ProvisionErrored,
    ImportStart,
    ImportComplete,
    EphemeralStart,
    EphemeralProgress,
    EphemeralComplete,
    EphemeralErrored,
    ResourceDrift,
    PlannedChange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourceAction {
    Create,
    Read,
    Update,
    Delete,
    Replace,
    Unknown(String),
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ResourceEvent {
    pub(crate) address: String,
    pub(crate) kind: ResourceEventKind,
    pub(crate) action: Option<ResourceAction>,
    pub(crate) message: Option<String>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SensitiveValue {
    Text(String),
    Number(String),
    Bool(bool),
}

impl Debug for SensitiveValue {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ExecutionTargetSpec {
    pub(crate) address: String,
    pub(crate) actions: Vec<PlanAction>,
}

impl Debug for ExecutionTargetSpec {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionTargetSpec")
            .field("address", &self.address)
            .field("actions", &self.actions)
            .finish()
    }
}

impl Debug for ResourceEvent {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResourceEvent")
            .field("address", &self.address)
            .field("kind", &self.kind)
            .field("action", &self.action)
            .field("message", &self.message.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct ExecutionSummary {
    pub(crate) adds: Option<usize>,
    pub(crate) changes: Option<usize>,
    pub(crate) removes: Option<usize>,
    pub(crate) operation: Option<String>,
    pub(crate) message: Option<String>,
}

impl Debug for ExecutionSummary {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionSummary")
            .field("adds", &self.adds)
            .field("changes", &self.changes)
            .field("removes", &self.removes)
            .field("operation", &self.operation)
            .field("message", &self.message.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticPoint {
    pub(crate) line: u64,
    pub(crate) column: u64,
    pub(crate) byte: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DiagnosticPosition {
    pub(crate) filename: String,
    pub(crate) start: DiagnosticPoint,
    pub(crate) end: DiagnosticPoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiagnosticSource {
    Terraform,
    UnknownEvent {
        stream: EventStream,
        event_type: Option<String>,
    },
    NonJson {
        stream: EventStream,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Diagnostic {
    pub(crate) severity: DiagnosticSeverity,
    pub(crate) summary: String,
    pub(crate) detail: Option<String>,
    pub(crate) address: Option<String>,
    pub(crate) position: Option<DiagnosticPosition>,
    pub(crate) source: DiagnosticSource,
}

impl Debug for Diagnostic {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Diagnostic")
            .field("severity", &self.severity)
            .field("summary", &"<redacted>")
            .field("detail", &self.detail.as_ref().map(|_| "<redacted>"))
            .field("address", &self.address)
            .field("position", &self.position)
            .field("source", &self.source)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProcessExitStatus {
    Exited(i32),
    Signaled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessTermination {
    pub(crate) status: ProcessExitStatus,
    pub(crate) interrupted: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum ExecutionEventKind {
    Resource(ResourceEvent),
    Summary(ExecutionSummary),
    Diagnostic(Diagnostic),
    Workspace(String),
    Informational {
        event_type: String,
        message: Option<String>,
    },
    Terminated(ProcessTermination),
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ExecutionLogLine {
    pub(crate) stream: EventStream,
    pub(crate) text: String,
}

impl Debug for ExecutionLogLine {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExecutionLogLine")
            .field("stream", &self.stream)
            .field("text", &"<redacted>")
            .finish()
    }
}

impl Debug for ExecutionEventKind {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resource(event) => formatter.debug_tuple("Resource").field(event).finish(),
            Self::Summary(summary) => formatter.debug_tuple("Summary").field(summary).finish(),
            Self::Diagnostic(diagnostic) => formatter
                .debug_tuple("Diagnostic")
                .field(diagnostic)
                .finish(),
            Self::Workspace(_) => formatter.write_str("Workspace(<redacted>)"),
            Self::Informational { event_type, .. } => formatter
                .debug_struct("Informational")
                .field("event_type", event_type)
                .field("message", &"<redacted>")
                .finish(),
            Self::Terminated(termination) => formatter
                .debug_tuple("Terminated")
                .field(termination)
                .finish(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionEvent {
    pub(crate) received_at: Instant,
    pub(crate) kind: ExecutionEventKind,
}

// Progress of preparing one environment's saved plan, sent while init, plan, and the review read
// run so the environment shows its current step instead of waiting for the final result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PreparationEvent {
    Initializing(InitializationReason),
    Planning,
    Reading,
    LockFile(LockFileChange),
    Output(ExecutionEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparationStage {
    Initializing,
    Planning,
    Reading,
}

impl PreparationStage {
    #[must_use]
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Initializing => "Initializing",
            Self::Planning => "Planning",
            Self::Reading => "Reading",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InitializationReason {
    NotInitialized,
    BackendNotInitialized,
    LockFileUnreadable,
    ProviderNotInstalled { provider: String },
    ModuleNotInstalled { name: String },
    // Only the fixed summary prefix that triggered the retry is kept, never Terraform's text.
    PlanRequested { summary: &'static str },
}

impl InitializationReason {
    #[must_use]
    pub(crate) fn message(&self) -> String {
        match self {
            Self::NotInitialized => "the directory has not been initialized".to_owned(),
            Self::BackendNotInitialized => "the backend has not been initialized".to_owned(),
            Self::LockFileUnreadable => "the dependency lock file could not be read".to_owned(),
            Self::ProviderNotInstalled { provider } => {
                format!("provider {provider} is not installed")
            }
            Self::ModuleNotInstalled { name } => format!("module \"{name}\" is not installed"),
            Self::PlanRequested { summary } => format!("plan reported \"{summary}\""),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LockFileChange {
    Created,
    Updated,
}

impl LockFileChange {
    #[must_use]
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::Created => "Init created the dependency lock file .terraform.lock.hcl.",
            Self::Updated => "Init updated the dependency lock file .terraform.lock.hcl.",
        }
    }
}
