use std::time::{Duration, Instant};

use super::copy;

mod context;
mod event;
pub(crate) mod history;
mod log_index;
mod progress;

pub(crate) use context::{
    ExecutionContext, ExecutionContextValue, Tool, VariableSources, directory_display_name,
};
pub(crate) use event::{
    Diagnostic, DiagnosticPoint, DiagnosticPosition, DiagnosticSeverity, DiagnosticSource,
    EventStream, ExecutionEvent, ExecutionEventKind, ExecutionLogLine, ExecutionPhase,
    ExecutionSummary, ExecutionTargetSpec, ProcessExitStatus, ProcessTermination, ResourceAction,
    ResourceEvent, ResourceEventKind, SensitiveValue,
};
pub(crate) use history::{HistoryKey, SuccessfulTarget};
pub(crate) use log_index::LogLineIndex;
pub(crate) use progress::{ExecutionProgress, ExecutionTargetState, ExecutionTargetStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionStage {
    Initializing,
    Reading,
    Applying,
    ApplySucceeded,
    ApplyFailed,
    ApplyInterrupted,
    Failed,
}

impl ExecutionStage {
    #[must_use]
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::Initializing => "Initializing",
            Self::Reading => "Reading",
            Self::Applying => "Applying",
            Self::ApplySucceeded => "Apply complete",
            Self::ApplyFailed => "Apply failed",
            Self::ApplyInterrupted => "Apply interrupted",
            Self::Failed => "Failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApplyStatus {
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExecutionAction {
    RequestCancellation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionState {
    stage: ExecutionStage,
    active_phase: ExecutionStage,
    context: ExecutionContext,
    started_at: Instant,
    finished_at: Option<Instant>,
    progress: ExecutionProgress,
    cancellation_requested: bool,
    failure_message: Option<String>,
    copy_feedback: copy::CopyFeedback,
    result: Option<ExecutionResult>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExecutionResult {
    phase: ExecutionStage,
    termination: ProcessTermination,
    summary_line: Option<String>,
    first_error_line: Option<usize>,
}

impl ExecutionResult {
    #[must_use]
    pub(crate) const fn phase(&self) -> ExecutionStage {
        self.phase
    }

    #[must_use]
    pub(crate) const fn termination(&self) -> ProcessTermination {
        self.termination
    }

    #[must_use]
    pub(crate) fn summary_line(&self) -> Option<&str> {
        self.summary_line.as_deref()
    }

    #[must_use]
    pub(crate) const fn first_error_line(&self) -> Option<usize> {
        self.first_error_line
    }
}

impl ExecutionState {
    #[must_use]
    pub(crate) fn with_context(started_at: Instant, context: ExecutionContext) -> Self {
        Self::at_stage(started_at, context, ExecutionStage::Initializing)
    }

    #[must_use]
    pub(crate) fn applying_with_previous(
        started_at: Instant,
        context: ExecutionContext,
        targets: Vec<ExecutionTargetSpec>,
        sensitive_values: Vec<SensitiveValue>,
        previous_durations: &[Option<Duration>],
    ) -> Self {
        Self::at_stage_with_progress(
            started_at,
            context,
            ExecutionStage::Applying,
            ExecutionProgress::with_previous(targets, sensitive_values, previous_durations),
        )
    }

    #[must_use]
    fn at_stage(started_at: Instant, context: ExecutionContext, stage: ExecutionStage) -> Self {
        Self::at_stage_with_progress(started_at, context, stage, ExecutionProgress::default())
    }

    #[must_use]
    fn at_stage_with_progress(
        started_at: Instant,
        context: ExecutionContext,
        stage: ExecutionStage,
        progress: ExecutionProgress,
    ) -> Self {
        Self {
            stage,
            active_phase: stage,
            context,
            started_at,
            finished_at: None,
            progress,
            cancellation_requested: false,
            failure_message: None,
            copy_feedback: copy::CopyFeedback::default(),
            result: None,
        }
    }

    pub(crate) const fn apply(&mut self, action: ExecutionAction) {
        match action {
            ExecutionAction::RequestCancellation => self.cancellation_requested = true,
        }
    }

    pub(crate) fn record(&mut self, event: ExecutionEvent) {
        if self.result.is_some() {
            return;
        }
        match &event.kind {
            ExecutionEventKind::Phase(ExecutionPhase::Reading) => {
                self.stage = ExecutionStage::Reading;
                self.active_phase = ExecutionStage::Reading;
            }
            ExecutionEventKind::Workspace(workspace) => {
                self.context = self.context.clone().with_workspace(workspace.clone());
            }
            ExecutionEventKind::Terminated(termination)
                if !termination.interrupted
                    && !matches!(termination.status, ProcessExitStatus::Exited(0)) =>
            {
                self.finished_at.get_or_insert(event.received_at);
                self.stage = ExecutionStage::Failed;
            }
            _ => {}
        }
        self.progress.record(event);
    }

    pub(crate) fn fail(&mut self, message: String, received_at: Instant) {
        let phase = self.active_phase;
        self.stage = ExecutionStage::Failed;
        self.finished_at.get_or_insert(received_at);
        self.failure_message = Some(message.clone());
        self.record(ExecutionEvent {
            received_at,
            kind: ExecutionEventKind::Diagnostic(Diagnostic {
                severity: DiagnosticSeverity::Error,
                summary: message,
                detail: None,
                address: None,
                position: None,
                source: DiagnosticSource::Terraform,
            }),
        });
        let termination = self.progress.termination().unwrap_or(ProcessTermination {
            status: ProcessExitStatus::Exited(1),
            interrupted: false,
        });
        let first_error_line = self.progress.first_error_line();
        self.result = Some(ExecutionResult {
            phase,
            termination,
            summary_line: None,
            first_error_line,
        });
    }

    pub(crate) fn finish_apply(
        &mut self,
        status: ApplyStatus,
        summary_line: Option<String>,
        message: Option<String>,
        received_at: Instant,
    ) {
        if let Some(message) = message {
            self.record(ExecutionEvent {
                received_at,
                kind: ExecutionEventKind::Diagnostic(Diagnostic {
                    severity: DiagnosticSeverity::Error,
                    summary: message,
                    detail: None,
                    address: None,
                    position: None,
                    source: DiagnosticSource::Terraform,
                }),
            });
        }
        let downgraded_success = status == ApplyStatus::Succeeded
            && self
                .progress
                .targets()
                .iter()
                .any(|target| target.status() != ExecutionTargetStatus::Completed);
        let status = if downgraded_success {
            ApplyStatus::Failed
        } else {
            status
        };
        self.finished_at.get_or_insert(received_at);
        self.stage = match status {
            ApplyStatus::Succeeded => ExecutionStage::ApplySucceeded,
            ApplyStatus::Failed => ExecutionStage::ApplyFailed,
            ApplyStatus::Interrupted => ExecutionStage::ApplyInterrupted,
        };
        let termination = self
            .progress
            .termination()
            .unwrap_or_else(|| ProcessTermination {
                status: match status {
                    ApplyStatus::Succeeded => ProcessExitStatus::Exited(0),
                    ApplyStatus::Failed => ProcessExitStatus::Exited(1),
                    ApplyStatus::Interrupted => ProcessExitStatus::Signaled,
                },
                interrupted: status == ApplyStatus::Interrupted,
            });
        self.progress.finish(termination);
        self.result = Some(ExecutionResult {
            phase: ExecutionStage::Applying,
            termination,
            summary_line: if downgraded_success {
                None
            } else {
                summary_line
                    .map(|summary| copy::sanitize_text(&summary, self.progress.sensitive_values()))
            },
            first_error_line: self.progress.first_error_line(),
        });
    }

    #[must_use]
    pub(crate) fn copy_effect(&self, target: copy::CopyTarget) -> Option<copy::CopyEffect> {
        match target {
            copy::CopyTarget::Diagnostic => Some(copy::diagnostic_effect(
                self.progress.diagnostics(),
                self.failure_message.as_deref(),
                self.progress.sensitive_values(),
            )),
            copy::CopyTarget::Execution if self.result().is_some() => {
                Some(copy::execution_effect(self))
            }
            copy::CopyTarget::Plan | copy::CopyTarget::Execution => None,
        }
    }

    #[must_use]
    pub(crate) const fn copy_feedback(&self) -> &copy::CopyFeedback {
        &self.copy_feedback
    }

    pub(crate) const fn copy_feedback_mut(&mut self) -> &mut copy::CopyFeedback {
        &mut self.copy_feedback
    }

    #[must_use]
    pub(crate) const fn stage(&self) -> ExecutionStage {
        self.stage
    }

    #[must_use]
    pub(crate) const fn context(&self) -> &ExecutionContext {
        &self.context
    }

    #[must_use]
    pub(crate) const fn progress(&self) -> &ExecutionProgress {
        &self.progress
    }

    #[must_use]
    pub(crate) fn successful_history(&self) -> Vec<SuccessfulTarget> {
        self.progress.successful_history(&self.context)
    }

    #[must_use]
    pub(crate) fn elapsed_at(&self, now: Instant) -> Duration {
        self.finished_at
            .unwrap_or(now)
            .saturating_duration_since(self.started_at)
    }

    #[must_use]
    pub(crate) fn waiting_at(&self, now: Instant) -> Duration {
        let last_event = self.progress.last_event_at().unwrap_or(self.started_at);
        now.saturating_duration_since(last_event)
    }

    #[must_use]
    pub(crate) const fn is_cancelling(&self) -> bool {
        self.cancellation_requested && self.progress.termination().is_none()
    }

    #[must_use]
    pub(crate) const fn cancellation_requested(&self) -> bool {
        self.cancellation_requested
    }

    #[must_use]
    pub(crate) fn is_apply(&self) -> bool {
        matches!(
            self.stage,
            ExecutionStage::Applying
                | ExecutionStage::ApplySucceeded
                | ExecutionStage::ApplyFailed
                | ExecutionStage::ApplyInterrupted
        ) || self
            .result
            .as_ref()
            .is_some_and(|result| result.phase() == ExecutionStage::Applying)
    }

    #[must_use]
    pub(crate) const fn result(&self) -> Option<&ExecutionResult> {
        self.result.as_ref()
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{
        Diagnostic, DiagnosticSeverity, DiagnosticSource, EventStream, ExecutionEventKind,
    };

    // Log text reaches the progress as informational messages, or as the diagnostics that
    // non-JSON output lines become, so tests add it the same way.
    pub(crate) fn log_event(stream: EventStream, text: impl Into<String>) -> ExecutionEventKind {
        let text = text.into();
        match stream {
            EventStream::Stdout => ExecutionEventKind::Informational {
                event_type: "log".to_owned(),
                message: Some(text),
            },
            EventStream::Stderr => ExecutionEventKind::Diagnostic(Diagnostic {
                severity: DiagnosticSeverity::Unknown,
                summary: text,
                detail: None,
                address: None,
                position: None,
                source: DiagnosticSource::NonJson { stream },
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::app::execution::test_support::log_event;
    use crate::app::plan::PlanAction;

    use super::*;

    impl ExecutionState {
        pub(crate) fn new(started_at: Instant) -> Self {
            Self::with_context(started_at, ExecutionContext::loading("loading..."))
        }

        pub(crate) fn applying(started_at: Instant, context: ExecutionContext) -> Self {
            Self::applying_with_previous(started_at, context, Vec::new(), Vec::new(), &[])
        }
    }

    fn event(received_at: Instant, kind: ExecutionEventKind) -> ExecutionEvent {
        ExecutionEvent { received_at, kind }
    }

    #[test]
    fn apply_result_summary_is_sanitized_before_rendering() {
        let started_at = Instant::now();
        let mut state = ExecutionState::applying_with_previous(
            started_at,
            ExecutionContext::loading("loading..."),
            Vec::new(),
            vec![SensitiveValue::Text("secret-value".to_owned())],
            &[],
        );

        state.finish_apply(
            ApplyStatus::Succeeded,
            Some("Apply complete: secret-value".to_owned()),
            None,
            started_at,
        );

        assert_eq!(
            state.result().and_then(ExecutionResult::summary_line),
            Some("Apply complete: (sensitive value)")
        );
    }

    #[test]
    fn successful_process_with_incomplete_targets_is_not_reported_as_success() {
        let started_at = Instant::now();
        let mut state = ExecutionState::applying_with_previous(
            started_at,
            ExecutionContext::loading("loading..."),
            vec![ExecutionTargetSpec {
                address: "terraform_data.api".to_owned(),
                actions: vec![PlanAction::Update],
            }],
            Vec::new(),
            &[],
        );

        state.finish_apply(
            ApplyStatus::Succeeded,
            Some("Apply complete.".to_owned()),
            None,
            started_at,
        );

        assert_eq!(state.stage(), ExecutionStage::ApplyFailed);
        assert_eq!(state.result().and_then(ExecutionResult::summary_line), None);
    }

    #[test]
    fn failed_apply_keeps_completed_targets_available_for_history() {
        let started_at = Instant::now();
        let mut state = ExecutionState::applying_with_previous(
            started_at,
            ExecutionContext::loading("/repo").with_workspace("default"),
            vec![
                ExecutionTargetSpec {
                    address: "terraform_data.completed".to_owned(),
                    actions: vec![PlanAction::Update],
                },
                ExecutionTargetSpec {
                    address: "terraform_data.failed".to_owned(),
                    actions: vec![PlanAction::Update],
                },
            ],
            Vec::new(),
            &[],
        );
        state.record(event(
            started_at + Duration::from_secs(1),
            ExecutionEventKind::Resource(ResourceEvent {
                address: "terraform_data.completed".to_owned(),
                kind: ResourceEventKind::ApplyStart,
                action: Some(ResourceAction::Update),
                message: None,
            }),
        ));
        state.record(event(
            started_at + Duration::from_secs(5),
            ExecutionEventKind::Resource(ResourceEvent {
                address: "terraform_data.completed".to_owned(),
                kind: ResourceEventKind::ApplyComplete,
                action: Some(ResourceAction::Update),
                message: None,
            }),
        ));
        state.record(event(
            started_at + Duration::from_secs(6),
            ExecutionEventKind::Resource(ResourceEvent {
                address: "terraform_data.failed".to_owned(),
                kind: ResourceEventKind::ApplyErrored,
                action: Some(ResourceAction::Update),
                message: None,
            }),
        ));
        state.finish_apply(
            ApplyStatus::Failed,
            None,
            None,
            started_at + Duration::from_secs(7),
        );

        let history = state.successful_history();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].duration, Duration::from_secs(4));
        assert_eq!(state.stage(), ExecutionStage::ApplyFailed);
    }

    #[test]
    fn reports_elapsed_and_waiting_time_from_injected_timestamps() {
        let started_at = Instant::now();
        let event_at = started_at + Duration::from_secs(2);
        let mut state = ExecutionState::new(started_at);

        assert_eq!(
            state.elapsed_at(started_at + Duration::from_secs(5)),
            Duration::from_secs(5)
        );
        assert_eq!(
            state.waiting_at(started_at + Duration::from_secs(5)),
            Duration::from_secs(5)
        );

        state.record(event(
            event_at,
            ExecutionEventKind::Informational {
                event_type: "log".to_owned(),
                message: None,
            },
        ));

        assert_eq!(
            state.waiting_at(started_at + Duration::from_secs(5)),
            Duration::from_secs(3)
        );
    }

    #[test]
    fn successful_termination_does_not_stop_elapsed_time_before_later_phases() {
        let started_at = Instant::now();
        let termination_at = started_at + Duration::from_secs(2);
        let mut state = ExecutionState::new(started_at);

        state.record(event(
            termination_at,
            ExecutionEventKind::Terminated(ProcessTermination {
                status: ProcessExitStatus::Exited(0),
                interrupted: false,
            }),
        ));
        state.record(event(
            started_at + Duration::from_secs(3),
            ExecutionEventKind::Phase(ExecutionPhase::Reading),
        ));

        assert_eq!(
            state.elapsed_at(started_at + Duration::from_secs(5)),
            Duration::from_secs(5)
        );
    }

    #[test]
    fn failure_elapsed_stays_at_termination_after_a_late_diagnostic() {
        let started_at = Instant::now();
        let termination_at = started_at + Duration::from_secs(2);
        let diagnostic_at = started_at + Duration::from_secs(4);
        let mut state = ExecutionState::new(started_at);

        state.record(event(
            termination_at,
            ExecutionEventKind::Terminated(ProcessTermination {
                status: ProcessExitStatus::Exited(1),
                interrupted: false,
            }),
        ));

        assert_eq!(
            state.elapsed_at(started_at + Duration::from_secs(3)),
            Duration::from_secs(2)
        );

        state.fail("Terraform failed".to_owned(), diagnostic_at);

        assert_eq!(
            state.elapsed_at(started_at + Duration::from_secs(10)),
            Duration::from_secs(2)
        );
    }

    #[test]
    fn cancellation_stays_visible_until_a_termination_event_arrives() {
        let started_at = Instant::now();
        let mut state = ExecutionState::new(started_at);

        state.apply(ExecutionAction::RequestCancellation);
        assert!(state.is_cancelling());

        state.record(event(
            started_at + Duration::from_secs(1),
            ExecutionEventKind::Terminated(ProcessTermination {
                status: ProcessExitStatus::Exited(130),
                interrupted: true,
            }),
        ));
        assert!(!state.is_cancelling());
        assert_eq!(state.stage(), ExecutionStage::Initializing);
    }

    #[test]
    fn failed_copy_effect_contains_the_diagnostic() {
        let started_at = Instant::now();
        let mut state = ExecutionState::new(started_at);
        state.fail("Terraform failed".to_owned(), started_at);

        let diagnostic = state
            .copy_effect(copy::CopyTarget::Diagnostic)
            .expect("diagnostic copy should be available");
        assert_eq!(diagnostic.target(), copy::CopyTarget::Diagnostic);
        assert!(diagnostic.text().contains("Terraform failed"));
    }

    #[test]
    fn nonzero_termination_enters_failed_stage() {
        let started_at = Instant::now();
        let mut state = ExecutionState::new(started_at);
        let termination_at = started_at + Duration::from_secs(1);
        let termination = ProcessTermination {
            status: ProcessExitStatus::Exited(1),
            interrupted: false,
        };

        state.record(event(
            termination_at,
            ExecutionEventKind::Terminated(termination),
        ));

        assert_eq!(state.stage(), ExecutionStage::Failed);
        assert_eq!(state.progress().termination(), Some(termination));
        assert_eq!(state.progress().last_event_at(), Some(termination_at));
    }

    #[test]
    fn failure_result_keeps_the_running_phase_and_first_terraform_error() {
        let started_at = Instant::now();
        let mut state = ExecutionState::new(started_at);
        state.record(event(
            started_at,
            ExecutionEventKind::Phase(ExecutionPhase::Reading),
        ));
        state.record(event(
            started_at,
            ExecutionEventKind::Diagnostic(Diagnostic {
                severity: DiagnosticSeverity::Warning,
                summary: "Provider warning".to_owned(),
                detail: Some("Warning detail".to_owned()),
                address: None,
                position: None,
                source: DiagnosticSource::Terraform,
            }),
        ));
        state.record(event(
            started_at,
            ExecutionEventKind::Diagnostic(Diagnostic {
                severity: DiagnosticSeverity::Error,
                summary: "Invalid configuration".to_owned(),
                detail: None,
                address: None,
                position: None,
                source: DiagnosticSource::Terraform,
            }),
        ));
        state.record(event(
            started_at,
            ExecutionEventKind::Terminated(ProcessTermination {
                status: ProcessExitStatus::Exited(1),
                interrupted: false,
            }),
        ));
        state.fail("Terraform plan failed".to_owned(), started_at);

        let result = state.result().expect("failure result should exist");
        assert_eq!(result.phase(), ExecutionStage::Reading);
        assert_eq!(result.first_error_line(), Some(2));
        assert_eq!(
            state.progress().log(),
            [
                ExecutionLogLine {
                    stream: EventStream::Stderr,
                    text: "Provider warning\nWarning detail".to_owned(),
                },
                ExecutionLogLine {
                    stream: EventStream::Stderr,
                    text: "Invalid configuration".to_owned(),
                },
                ExecutionLogLine {
                    stream: EventStream::Stderr,
                    text: "Terraform plan failed".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn late_event_after_result_does_not_change_log_error_position_or_last_event() {
        let started_at = Instant::now();
        let finished_at = started_at + Duration::from_secs(1);
        let mut state = ExecutionState::new(started_at);
        state.record(event(
            started_at,
            log_event(EventStream::Stdout, "before failure".to_owned()),
        ));
        state.fail("Terraform failed".to_owned(), finished_at);

        let log = state.progress().log().to_owned();
        let first_error_line = state
            .result()
            .expect("failure result should exist")
            .first_error_line();

        state.record(event(
            finished_at + Duration::from_secs(1),
            ExecutionEventKind::Diagnostic(Diagnostic {
                severity: DiagnosticSeverity::Error,
                summary: "Late error".to_owned(),
                detail: None,
                address: None,
                position: None,
                source: DiagnosticSource::Terraform,
            }),
        ));

        assert_eq!(state.progress().log(), log.as_slice());
        assert_eq!(
            state
                .result()
                .expect("failure result should still exist")
                .first_error_line(),
            first_error_line
        );
        assert_eq!(state.progress().last_event_at(), Some(finished_at));
    }

    #[test]
    fn review_events_update_workspace_and_execution_phase() {
        let started_at = Instant::now();
        let mut state = ExecutionState::new(started_at);

        state.record(event(
            started_at,
            ExecutionEventKind::Workspace("default".to_owned()),
        ));
        assert_eq!(
            state.context().workspace(),
            &ExecutionContextValue::Known("default".to_owned())
        );

        state.record(event(
            started_at,
            ExecutionEventKind::Phase(ExecutionPhase::Reading),
        ));
        assert_eq!(state.stage(), ExecutionStage::Reading);
    }
}
