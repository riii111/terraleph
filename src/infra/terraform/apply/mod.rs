use std::ffi::OsString;
use std::path::Path;

use crate::app::execution::{ApplyStatus, ExecutionEvent, ExecutionEventKind, Tool};
use crate::infra::CancellationToken;

use super::command::{
    ProcessRunner, ProcessStatus, TerraformCommand, TerraformExecutionError, run_command,
};
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApplyResult {
    status: ApplyStatus,
    summary_line: Option<String>,
}

impl ApplyResult {
    #[must_use]
    pub(crate) const fn status(&self) -> ApplyStatus {
        self.status
    }

    #[must_use]
    pub(crate) fn summary_line(&self) -> Option<&str> {
        self.summary_line.as_deref()
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "apply keeps the explicit execution and event boundaries"
)]
pub(crate) fn run_apply_with_arguments(
    tool: Tool,
    root: &Path,
    global_arguments: &[OsString],
    apply_arguments: &[OsString],
    plan_path: &Path,
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
    event_sink: &mut dyn FnMut(ExecutionEvent),
) -> Result<ApplyResult, TerraformExecutionError> {
    let mut arguments = global_arguments.to_vec();
    arguments.push(OsString::from("apply"));
    arguments.push(OsString::from("-json"));
    arguments.push(OsString::from("-input=false"));
    arguments.extend(apply_arguments.iter().cloned());
    arguments.push(plan_path.as_os_str().to_owned());
    let mut summary_line = None;
    let mut structured_event_sink = |event: ExecutionEvent| {
        if let ExecutionEventKind::Summary(summary) = &event.kind
            && summary.message.is_some()
        {
            summary_line.clone_from(&summary.message);
        }
        event_sink(event);
    };
    let output = run_command(
        tool,
        root,
        TerraformCommand::Apply,
        &arguments,
        cancellation,
        runner,
        Some(&mut structured_event_sink),
    )?;
    if output.interrupted {
        return Ok(ApplyResult {
            status: ApplyStatus::Interrupted,
            summary_line: None,
        });
    }
    if !matches!(output.status, ProcessStatus::Exited(0)) {
        return Ok(ApplyResult {
            status: ApplyStatus::Failed,
            summary_line: None,
        });
    }

    Ok(ApplyResult {
        status: ApplyStatus::Succeeded,
        summary_line: summary_line.or_else(|| Some("Apply complete.".to_owned())),
    })
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, io};

    use super::*;
    use crate::app::execution::{
        Diagnostic, DiagnosticSeverity, DiagnosticSource, EventStream, ResourceEventKind,
    };
    use crate::infra::terraform::test_support::{ProcessOutput, RunningProcess};

    struct FakeRunner {
        response: RefCell<Option<(ProcessStatus, ProcessOutput)>>,
        arguments: RefCell<Vec<OsString>>,
    }

    struct FakeProcess {
        status: ProcessStatus,
        output: ProcessOutput,
    }

    impl ProcessRunner for FakeRunner {
        fn start(
            &self,
            _tool: Tool,
            _root: &Path,
            arguments: &[OsString],
        ) -> io::Result<Box<dyn RunningProcess>> {
            self.arguments
                .borrow_mut()
                .extend(arguments.iter().cloned());
            let (status, output) = self
                .response
                .borrow_mut()
                .take()
                .ok_or_else(|| io::Error::other("fake process was already started"))?;
            Ok(Box::new(FakeProcess { status, output }))
        }
    }

    impl RunningProcess for FakeProcess {
        fn try_wait(&mut self) -> io::Result<Option<ProcessStatus>> {
            Ok(Some(self.status))
        }

        fn request_interrupt(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn wait(&mut self) -> io::Result<ProcessStatus> {
            Ok(self.status)
        }

        fn collect_output(self: Box<Self>) -> io::Result<ProcessOutput> {
            Ok(self.output)
        }
    }

    #[test]
    fn successful_apply_parses_structured_events_and_summary() {
        let runner = FakeRunner {
            response: RefCell::new(Some((
                ProcessStatus::Exited(0),
                ProcessOutput::new(
                    br#"{"type":"apply_start","hook":{"resource":{"addr":"terraform_data.api"},"action":"update"}}
{"type":"apply_complete","hook":{"resource":{"addr":"terraform_data.api"},"action":"update"}}
{"type":"change_summary","@message":"Apply complete! Resources: 1 added, 0 changed, 0 destroyed.","changes":{"add":1,"change":0,"remove":0,"operation":"apply"}}
"#.to_vec(),
                    b"warning: retained\n".to_vec(),
                ),
            ))),
            arguments: RefCell::new(Vec::new()),
        };
        let cancellation = CancellationToken::new();
        let mut events = Vec::new();

        let result = run_apply_with_arguments(
            Tool::Terraform,
            Path::new("/project"),
            &[],
            &[OsString::from("-no-color")],
            Path::new("/project/review.tfplan"),
            &cancellation,
            &runner,
            &mut |event| events.push(event),
        )
        .expect("apply should finish");

        assert_eq!(result.status(), ApplyStatus::Succeeded);
        assert_eq!(
            result.summary_line(),
            Some("Apply complete! Resources: 1 added, 0 changed, 0 destroyed.")
        );
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            ExecutionEventKind::Resource(event)
                if event.address == "terraform_data.api"
                    && event.kind == ResourceEventKind::ApplyStart
        )));
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            ExecutionEventKind::Diagnostic(Diagnostic {
                severity: DiagnosticSeverity::Unknown,
                source: DiagnosticSource::NonJson {
                    stream: EventStream::Stderr,
                },
                summary,
                ..
            }) if summary == "warning: retained"
        )));
        assert_eq!(
            runner.arguments.borrow().as_slice(),
            [
                OsString::from("apply"),
                OsString::from("-json"),
                OsString::from("-input=false"),
                OsString::from("-no-color"),
                OsString::from("/project/review.tfplan"),
            ]
        );
    }

    #[test]
    fn nonzero_apply_is_failed_and_cancellation_is_interrupted() {
        let failed_runner = FakeRunner {
            response: RefCell::new(Some((
                ProcessStatus::Exited(1),
                ProcessOutput::new(
                    Vec::new(),
                    br#"{"type":"diagnostic","@level":"error","diagnostic":{"severity":"error","summary":"apply failed","detail":"provider rejected the request","address":"terraform_data.api"}}
"#.to_vec(),
                ),
            ))),
            arguments: RefCell::new(Vec::new()),
        };
        let mut events = Vec::new();
        let failed = run_apply_with_arguments(
            Tool::Terraform,
            Path::new("/project"),
            &[],
            &[OsString::from("-no-color")],
            Path::new("/project/review.tfplan"),
            &CancellationToken::new(),
            &failed_runner,
            &mut |event| events.push(event),
        )
        .expect("failed apply should return a result");
        assert_eq!(failed.status(), ApplyStatus::Failed);
        assert!(events.iter().any(|event| matches!(
            &event.kind,
            ExecutionEventKind::Diagnostic(Diagnostic {
                summary,
                address: Some(address),
                ..
            }) if summary == "apply failed" && address == "terraform_data.api"
        )));

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let interrupted = run_apply_with_arguments(
            Tool::Terraform,
            Path::new("/project"),
            &[],
            &[OsString::from("-no-color")],
            Path::new("/project/review.tfplan"),
            &cancelled,
            &failed_runner,
            &mut |_| {},
        )
        .expect("pre-cancelled apply should return a result");
        assert_eq!(interrupted.status(), ApplyStatus::Interrupted);
    }

    #[test]
    fn successful_apply_without_a_summary_still_succeeds_with_a_fallback() {
        let runner = FakeRunner {
            response: RefCell::new(Some((
                ProcessStatus::Exited(0),
                ProcessOutput::new(b"Applying saved plan...\n".to_vec(), Vec::new()),
            ))),
            arguments: RefCell::new(Vec::new()),
        };

        let result = run_apply_with_arguments(
            Tool::Terraform,
            Path::new("/project"),
            &[],
            &[OsString::from("-no-color")],
            Path::new("/project/review.tfplan"),
            &CancellationToken::new(),
            &runner,
            &mut |_| {},
        )
        .expect("missing summary must not fail a successful apply");

        assert_eq!(result.status(), ApplyStatus::Succeeded);
        assert_eq!(result.summary_line(), Some("Apply complete."));
    }
}
