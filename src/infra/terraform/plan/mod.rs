mod temporary;

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
};

pub(crate) use temporary::remove_orphaned_plans;

use crate::app::execution::{
    Diagnostic, ExecutionContext, ExecutionEvent, ExecutionEventKind, ExecutionPhase, Tool,
};
use crate::app::{plan::StateRelationStatus, review::PlanReview};
use crate::infra::CancellationToken;

use super::{
    command::{
        ProcessRunner, ProcessStatus, TerraformCommand, TerraformExecutionError, run_passthrough,
    },
    read_provider_schema_with_arguments,
    show::read_review_with_arguments,
    state::read_state_with_arguments,
    workspace::read_workspace_with_arguments,
};

pub(crate) struct SavedPlan {
    path: Option<PathBuf>,
    ownership: SavedPlanOwnership,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SavedPlanOwnership {
    Terraleph,
    User,
}

impl SavedPlan {
    fn create() -> io::Result<Self> {
        temporary::create_plan_path().map(|path| Self {
            path: Some(path),
            ownership: SavedPlanOwnership::Terraleph,
        })
    }

    pub(crate) const fn user_owned(path: PathBuf) -> Self {
        Self {
            path: Some(path),
            ownership: SavedPlanOwnership::User,
        }
    }

    #[must_use]
    pub(crate) fn path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("saved plan path should exist until cleanup")
    }

    pub(crate) fn cleanup(mut self) -> io::Result<()> {
        self.remove()
    }

    fn remove(&mut self) -> io::Result<()> {
        if self.ownership == SavedPlanOwnership::User {
            return Ok(());
        }
        let Some(path) = self.path.take() else {
            return Ok(());
        };
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

pub(crate) fn run_passthrough_plan(
    executable: &Path,
    launch_root: &Path,
    global_arguments: &[OsString],
    plan_arguments: &[OsString],
) -> io::Result<ProcessStatus> {
    let mut arguments = global_arguments.to_vec();
    arguments.push(OsString::from(TerraformCommand::Plan.to_string()));
    arguments.extend(plan_arguments.iter().cloned());
    run_passthrough(executable, launch_root, &arguments)
}

pub(crate) fn run_environment_plan(
    tool: Tool,
    root: &Path,
    plan_arguments: &[OsString],
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<bool, TerraformExecutionError> {
    use super::command::{interrupted_error, non_zero_error, run_command};
    let mut initialized = super::init::needed(root);
    if initialized {
        initialize_environment(tool, root, cancellation, runner, diagnostics)?;
    }
    let mut arguments = vec![OsString::from("plan")];
    arguments.extend_from_slice(plan_arguments);
    arguments.extend(["-json", "-input=false", "-detailed-exitcode"].map(OsString::from));
    loop {
        let mut attempt_diagnostics = Vec::new();
        let process = run_command(
            tool,
            root,
            TerraformCommand::Plan,
            &arguments,
            cancellation,
            runner,
            Some(&mut |event| {
                if let ExecutionEventKind::Diagnostic(diagnostic) = event.kind {
                    attempt_diagnostics.push(diagnostic);
                }
            }),
        )?;
        let reinit = attempt_diagnostics.iter().any(requires_init);
        if process.interrupted {
            diagnostics.extend(attempt_diagnostics);
            return Err(interrupted_error(tool, TerraformCommand::Plan));
        }
        if process.status.is_plan_success() {
            diagnostics.extend(attempt_diagnostics);
            return Ok(process.status == ProcessStatus::Exited(2));
        }
        if !initialized && reinit {
            initialized = true;
            initialize_environment(tool, root, cancellation, runner, diagnostics)?;
        } else {
            diagnostics.extend(attempt_diagnostics);
            return Err(non_zero_error(tool, TerraformCommand::Plan, process.status));
        }
    }
}

pub(crate) fn saved_plan_for_plan(
    execution_root: &Path,
    plan_arguments: &[OsString],
) -> io::Result<(SavedPlan, Vec<OsString>)> {
    let mut arguments = plan_arguments.to_vec();
    let mut output_path = None;
    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].to_string_lossy();
        let option = argument
            .strip_prefix("--")
            .or_else(|| argument.strip_prefix('-'))
            .unwrap_or_default();
        if let Some(value) = option.strip_prefix("out=") {
            let path = resolve_output_path(execution_root, value);
            arguments[index] = inline_output_argument(&path);
            output_path = Some(path);
            index += 1;
            continue;
        }
        if option == "out" {
            let value = arguments.get(index + 1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "-out requires a path")
            })?;
            let path = resolve_output_path(execution_root, &value.to_string_lossy());
            arguments[index] = OsString::from("-out");
            path.as_os_str().clone_into(&mut arguments[index + 1]);
            output_path = Some(path);
            index += 2;
            continue;
        }
        index += 1;
    }

    if let Some(path) = output_path {
        return Ok((SavedPlan::user_owned(path), arguments));
    }

    let saved_plan = SavedPlan::create()?;
    arguments.push(inline_output_argument(saved_plan.path()));
    Ok((saved_plan, arguments))
}

fn inline_output_argument(path: &Path) -> OsString {
    let mut argument = OsString::from("-out=");
    argument.push(path);
    argument
}

fn resolve_output_path(root: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the review worker receives the explicit execution and UI boundaries"
)]
pub(crate) fn read_saved_plan_review(
    tool: Tool,
    display_root: &Path,
    launch_root: &Path,
    global_arguments: &[OsString],
    plan_path: &Path,
    plan_changed: bool,
    apply_entry: bool,
    initial_context: ExecutionContext,
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
    event_sink: &mut dyn FnMut(ExecutionEvent),
    phase_sink: &mut dyn FnMut(ExecutionPhase),
) -> Result<PlanReview, TerraformExecutionError> {
    phase_sink(ExecutionPhase::Reading);
    let version = super::version::read_version_with_arguments(
        tool,
        launch_root,
        global_arguments,
        cancellation,
        runner,
    )?;
    let workspace =
        read_workspace_with_arguments(tool, launch_root, global_arguments, cancellation, runner)?;
    event_sink(ExecutionEvent {
        received_at: std::time::Instant::now(),
        kind: ExecutionEventKind::Workspace(workspace.clone()),
    });
    let (document, metadata, plan, mut relations) = read_review_with_arguments(
        tool,
        launch_root,
        global_arguments,
        plan_path,
        plan_changed,
        cancellation,
        runner,
    )?;
    if relations.state_status == StateRelationStatus::NotCollected {
        relations = match read_state_with_arguments(
            tool,
            launch_root,
            global_arguments,
            cancellation,
            runner,
        ) {
            Ok(Some(state)) => relations.with_state(StateRelationStatus::Available, state),
            Err(error) if error.is_interrupted() => return Err(error),
            Ok(None) | Err(_) => relations.with_state(StateRelationStatus::Unavailable, Vec::new()),
        };
    }
    let provider_schemas = read_provider_schema_with_arguments(
        tool,
        launch_root,
        global_arguments,
        cancellation,
        runner,
    )?;
    let context = initial_context
        .with_tool_version(tool, version)
        .with_workspace(workspace.clone());
    let review = PlanReview::new(
        display_root.to_owned(),
        workspace,
        document,
        plan,
        metadata,
        Vec::new(),
    )
    .with_relations(relations)
    .with_provider_schemas(provider_schemas)
    .with_context(context)
    .with_apply_entry(apply_entry);
    Ok(review)
}

fn initialize_environment(
    tool: Tool,
    root: &Path,
    cancellation: &CancellationToken,
    runner: &dyn ProcessRunner,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), TerraformExecutionError> {
    let mut init_diagnostics = Vec::new();
    let result = super::init::run(tool, root, cancellation, runner, &mut |event| {
        if let ExecutionEventKind::Diagnostic(diagnostic) = event.kind {
            init_diagnostics.push(diagnostic);
        }
    });
    if result.is_err() {
        diagnostics.extend(init_diagnostics);
    }
    result
}

fn requires_init(diagnostic: &Diagnostic) -> bool {
    [
        "Backend initialization required",
        "Required plugins are not installed",
        "Inconsistent dependency lock file",
        "Module not installed",
        "Module source has changed",
    ]
    .iter()
    .any(|summary| diagnostic.summary.starts_with(summary))
}

impl Drop for SavedPlan {
    fn drop(&mut self) {
        let _ = self.remove();
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::fmt::{Display, Formatter};

    use crate::app::execution::Tool;
    use crate::app::plan::Plan;

    use super::super::command::{TerraformCommand, TerraformExecutionErrorKind, run_successful};
    use super::{
        CancellationToken, ExecutionEvent, ExecutionPhase, OsString, Path, ProcessRunner,
        SavedPlan, TerraformExecutionError,
    };

    #[derive(Debug)]
    pub(crate) enum PlanTestError {
        Terraform(TerraformExecutionError),
        TemporaryPlan { message: String },
        Cleanup { message: String },
    }

    impl From<TerraformExecutionError> for PlanTestError {
        fn from(error: TerraformExecutionError) -> Self {
            Self::Terraform(error)
        }
    }

    impl Display for PlanTestError {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Terraform(error) => Display::fmt(error, formatter),
                Self::TemporaryPlan { message } => {
                    write!(
                        formatter,
                        "failed to create a temporary Terraform plan: {message}"
                    )
                }
                Self::Cleanup { message } => write!(
                    formatter,
                    "failed to remove the temporary Terraform plan: {message}"
                ),
            }
        }
    }

    impl std::error::Error for PlanTestError {}

    impl PlanTestError {
        pub(crate) const fn kind(&self) -> &TerraformExecutionErrorKind {
            match self {
                Self::Terraform(error) => error.kind(),
                Self::TemporaryPlan { .. } | Self::Cleanup { .. } => {
                    panic!("non-Terraform errors have no Terraform error kind")
                }
            }
        }
    }

    pub(crate) fn run_plan(
        root: &Path,
        cancellation: &CancellationToken,
        runner: &dyn ProcessRunner,
        event_sink: &mut dyn FnMut(ExecutionEvent),
        phase_sink: &mut dyn FnMut(ExecutionPhase),
    ) -> Result<Plan, PlanTestError> {
        let saved_plan = SavedPlan::create().map_err(|error| PlanTestError::TemporaryPlan {
            message: error.to_string(),
        })?;
        let result = execute_plan(
            root,
            saved_plan.path(),
            cancellation,
            runner,
            event_sink,
            phase_sink,
        );

        finish_plan(saved_plan, result)
    }

    pub(crate) fn execute_plan(
        root: &Path,
        plan_path: &Path,
        cancellation: &CancellationToken,
        runner: &dyn ProcessRunner,
        event_sink: &mut dyn FnMut(ExecutionEvent),
        phase_sink: &mut dyn FnMut(ExecutionPhase),
    ) -> Result<Plan, TerraformExecutionError> {
        run_successful(
            Tool::Terraform,
            root,
            TerraformCommand::Plan,
            &plan_arguments(plan_path),
            cancellation,
            runner,
            Some(event_sink),
        )?;

        phase_sink(ExecutionPhase::Reading);
        super::super::show::test_support::read_plan(root, plan_path, cancellation, runner)
    }

    pub(crate) fn finish_plan(
        saved_plan: SavedPlan,
        result: Result<Plan, TerraformExecutionError>,
    ) -> Result<Plan, PlanTestError> {
        match (saved_plan.cleanup(), result) {
            (_, Err(error)) => Err(PlanTestError::Terraform(error)),
            (Err(error), Ok(_)) => Err(PlanTestError::Cleanup {
                message: error.to_string(),
            }),
            (Ok(()), Ok(plan)) => Ok(plan),
        }
    }

    fn plan_arguments(plan_path: &Path) -> Vec<OsString> {
        let mut output = OsString::from("-out=");
        output.push(plan_path.as_os_str());
        vec![
            OsString::from("plan"),
            OsString::from("-input=false"),
            OsString::from("-json"),
            output,
        ]
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        collections::VecDeque,
        rc::Rc,
    };

    use serde_json::json;

    use crate::app::execution::{ResourceEvent, ResourceEventKind};

    use super::super::command::{
        ProcessOutput, ProcessOutputChunk, RunningProcess, TerraformExecutionErrorKind,
    };
    use super::test_support::{execute_plan, finish_plan, run_plan};
    use super::*;
    use crate::app::execution::{
        DiagnosticSource, EventStream, ProcessExitStatus, ProcessTermination,
    };
    use crate::app::plan::Plan;
    use std::{
        env,
        fs::OpenOptions,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    fn execute_plan_without_events(
        root: &Path,
        plan_path: &Path,
        cancellation: &CancellationToken,
        runner: &dyn ProcessRunner,
    ) -> Result<Plan, TerraformExecutionError> {
        execute_plan(
            root,
            plan_path,
            cancellation,
            runner,
            &mut |_| {},
            &mut |_| {},
        )
    }

    const PLAN_JSON: &[u8] = br#"{"format_version":"1.0"}"#;

    #[derive(Debug)]
    struct Invocation {
        root: PathBuf,
        arguments: Vec<OsString>,
    }

    struct FakeRunner {
        responses: RefCell<VecDeque<FakeResponse>>,
        invocations: RefCell<Vec<Invocation>>,
    }

    enum FakeResponse {
        LaunchError(&'static str),
        Exit {
            status: ProcessStatus,
            output: ProcessOutput,
        },
        Streaming {
            status: ProcessStatus,
            output: ProcessOutput,
            chunks: VecDeque<ProcessOutputChunk>,
        },
        Pending {
            cancellation: CancellationToken,
            output: ProcessOutput,
            interrupt_count: Rc<Cell<usize>>,
        },
        MakePlanPathDirectory,
    }

    struct FakeProcess {
        response: Option<FakeResponse>,
        plan_path: Option<PathBuf>,
    }

    impl FakeRunner {
        fn new(responses: impl IntoIterator<Item = FakeResponse>) -> Self {
            Self {
                responses: RefCell::new(responses.into_iter().collect()),
                invocations: RefCell::new(Vec::new()),
            }
        }
    }

    impl ProcessRunner for FakeRunner {
        fn start(
            &self,
            _tool: Tool,
            root: &Path,
            arguments: &[OsString],
        ) -> io::Result<Box<dyn RunningProcess>> {
            self.invocations.borrow_mut().push(Invocation {
                root: root.to_owned(),
                arguments: arguments.to_vec(),
            });
            let response = self
                .responses
                .borrow_mut()
                .pop_front()
                .ok_or_else(|| io::Error::other("fake response was not configured"))?;
            if let FakeResponse::LaunchError(message) = response {
                return Err(io::Error::other(message));
            }

            let plan_path = arguments.iter().find_map(|argument| {
                argument
                    .to_str()
                    .and_then(|argument| argument.strip_prefix("-out="))
                    .map(PathBuf::from)
            });
            Ok(Box::new(FakeProcess {
                response: Some(response),
                plan_path,
            }))
        }
    }

    impl RunningProcess for FakeProcess {
        fn poll_output(&mut self) -> io::Result<Vec<ProcessOutputChunk>> {
            match self.response.as_mut() {
                Some(FakeResponse::Streaming { chunks, .. }) => Ok(chunks.drain(..).collect()),
                _ => Ok(Vec::new()),
            }
        }

        fn try_wait(&mut self) -> io::Result<Option<ProcessStatus>> {
            match self.response.as_mut() {
                Some(FakeResponse::Exit { status, .. }) => Ok(Some(*status)),
                Some(FakeResponse::Streaming { status, chunks, .. }) => {
                    if chunks.is_empty() {
                        Ok(Some(*status))
                    } else {
                        Ok(None)
                    }
                }
                Some(FakeResponse::Pending { cancellation, .. }) => {
                    cancellation.cancel();
                    Ok(None)
                }
                Some(FakeResponse::MakePlanPathDirectory) => {
                    let path = self
                        .plan_path
                        .take()
                        .ok_or_else(|| io::Error::other("plan path was not passed"))?;
                    fs::remove_file(&path)?;
                    fs::create_dir(path)?;
                    Ok(Some(ProcessStatus::Exited(0)))
                }
                None => Err(io::Error::other("fake process was already consumed")),
                Some(FakeResponse::LaunchError(_)) => {
                    Err(io::Error::other("launch error cannot become a process"))
                }
            }
        }

        fn request_interrupt(&mut self) -> io::Result<()> {
            if let Some(FakeResponse::Pending {
                interrupt_count, ..
            }) = self.response.as_ref()
            {
                interrupt_count.set(interrupt_count.get() + 1);
            }
            Ok(())
        }

        fn wait(&mut self) -> io::Result<ProcessStatus> {
            Ok(ProcessStatus::Signaled)
        }

        fn collect_output(self: Box<Self>) -> io::Result<ProcessOutput> {
            match self.response {
                Some(
                    FakeResponse::Exit { output, .. }
                    | FakeResponse::Streaming { output, .. }
                    | FakeResponse::Pending { output, .. },
                ) => Ok(output),
                Some(FakeResponse::MakePlanPathDirectory) => Ok(ProcessOutput::empty()),
                Some(FakeResponse::LaunchError(_)) | None => {
                    Err(io::Error::other("fake process output was unavailable"))
                }
            }
        }
    }

    fn successful_process() -> FakeResponse {
        FakeResponse::Exit {
            status: ProcessStatus::Exited(0),
            output: ProcessOutput::new(Vec::new(), Vec::new()),
        }
    }

    fn show_process() -> FakeResponse {
        FakeResponse::Exit {
            status: ProcessStatus::Exited(0),
            output: ProcessOutput::new(PLAN_JSON.to_vec(), Vec::new()),
        }
    }

    fn streaming_process(
        status: ProcessStatus,
        stdout_chunks: impl IntoIterator<Item = Vec<u8>>,
        stderr_chunks: impl IntoIterator<Item = Vec<u8>>,
    ) -> FakeResponse {
        let stdout_chunks = stdout_chunks.into_iter().map(|bytes| ProcessOutputChunk {
            stream: EventStream::Stdout,
            bytes,
        });
        let stderr_chunks = stderr_chunks.into_iter().map(|bytes| ProcessOutputChunk {
            stream: EventStream::Stderr,
            bytes,
        });
        let chunks: VecDeque<_> = stdout_chunks.chain(stderr_chunks).collect();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        for chunk in &chunks {
            match chunk.stream {
                EventStream::Stdout => stdout.extend_from_slice(&chunk.bytes),
                EventStream::Stderr => stderr.extend_from_slice(&chunk.bytes),
            }
        }
        FakeResponse::Streaming {
            status,
            output: ProcessOutput::new(stdout, stderr),
            chunks,
        }
    }

    fn saved_plan_with_space() -> (SavedPlan, PathBuf) {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let directory = (0..100)
            .find_map(|attempt| {
                let directory = env::temp_dir().join(format!(
                    "terraleph test plan {} {timestamp} {attempt}",
                    std::process::id()
                ));
                match fs::create_dir(&directory) {
                    Ok(()) => Some(directory),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => None,
                    Err(error) => panic!("test directory should be created: {error}"),
                }
            })
            .expect("test directory should be unique");
        let path = directory.join("saved plan.tfplan");
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("test plan should be created");
        (
            SavedPlan {
                path: Some(path),
                ownership: SavedPlanOwnership::Terraleph,
            },
            directory,
        )
    }

    fn run_fake(
        runner: &FakeRunner,
        saved_plan: SavedPlan,
        cancellation: &CancellationToken,
    ) -> Result<Plan, test_support::PlanTestError> {
        let result = execute_plan_without_events(
            Path::new("/root with spaces"),
            saved_plan.path(),
            cancellation,
            runner,
        );
        finish_plan(saved_plan, result)
    }

    fn argument_strings(arguments: &[OsString]) -> Vec<String> {
        arguments
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn user_owned_output_path_is_resolved_and_survives_cleanup() {
        let root = env::temp_dir().join(format!(
            "terraleph-user-plan-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir(&root).expect("output root should be created");
        let expected = root.join("review.tfplan");
        let (saved_plan, arguments) =
            saved_plan_for_plan(&root, &[OsString::from("-out=review.tfplan")])
                .expect("user output path should be accepted");

        assert_eq!(saved_plan.path(), expected);
        assert_eq!(
            argument_strings(&arguments),
            [format!("-out={}", expected.display())]
        );
        fs::write(&expected, b"user-owned plan").expect("user plan should be created");
        saved_plan
            .cleanup()
            .expect("user-owned plan cleanup should be a no-op");
        assert!(expected.exists());
        fs::remove_file(expected).expect("user-owned plan should be removed by the test");
        fs::remove_dir(root).expect("output root should be removed");
    }

    #[cfg(unix)]
    #[test]
    fn inline_output_path_keeps_non_utf8_execution_root() {
        use std::os::unix::ffi::OsStringExt;
        let root = PathBuf::from(OsString::from_vec(b"/repo/infra-\xff".to_vec()));
        let expected = root.join("review.tfplan");

        let (saved_plan, arguments) =
            saved_plan_for_plan(&root, &[OsString::from("-out=review.tfplan")])
                .expect("user output path should be accepted");

        assert_eq!(saved_plan.path(), expected);
        let mut expected_argument = OsString::from("-out=");
        expected_argument.push(&expected);
        assert_eq!(arguments, [expected_argument]);
    }

    #[test]
    fn last_output_path_wins_across_environment_and_explicit_arguments() {
        let root = env::temp_dir().join(format!(
            "terraleph-repeated-plan-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir(&root).expect("output root should be created");
        let environment_path = root.join("environment.tfplan");
        let explicit_path = root.join("explicit.tfplan");
        let (saved_plan, arguments) = saved_plan_for_plan(
            &root,
            &[
                OsString::from(format!("-out={}", environment_path.display())),
                OsString::from("-out"),
                OsString::from("explicit.tfplan"),
            ],
        )
        .expect("repeated output paths should be accepted");

        assert_eq!(saved_plan.path(), explicit_path);
        assert_eq!(
            argument_strings(&arguments),
            vec![
                format!("-out={}", environment_path.display()),
                "-out".to_owned(),
                explicit_path.display().to_string(),
            ]
        );
        saved_plan
            .cleanup()
            .expect("user-owned plan cleanup should be a no-op");
        fs::remove_dir(root).expect("output root should be removed");
    }

    #[test]
    fn implicit_output_path_is_absolute_and_owned_by_terraleph() {
        let (saved_plan, arguments) = saved_plan_for_plan(
            Path::new("/root with spaces"),
            &[OsString::from("-refresh=false")],
        )
        .expect("temporary output path should be created");
        let path = saved_plan.path().to_owned();
        assert!(path.is_absolute());
        assert_eq!(
            argument_strings(&arguments),
            vec![
                "-refresh=false".to_owned(),
                format!("-out={}", path.display()),
            ]
        );
        assert!(path.exists());
        saved_plan
            .cleanup()
            .expect("temporary plan cleanup should succeed");
        assert!(!path.exists());
    }

    #[test]
    fn runs_plan_then_show_in_explicit_root_with_argument_boundaries() {
        let runner = FakeRunner::new([successful_process(), show_process()]);
        let cancellation = CancellationToken::new();
        let (saved_plan, directory) = saved_plan_with_space();
        let plan_path = saved_plan.path().to_owned();

        let result = run_fake(&runner, saved_plan, &cancellation)
            .expect("Terraform plan should be returned");

        assert!(result.resource_changes.is_empty());
        let invocations = runner.invocations.borrow();
        assert_eq!(invocations.len(), 2);
        assert_eq!(invocations[0].root, Path::new("/root with spaces"));
        assert_eq!(
            argument_strings(&invocations[0].arguments),
            vec![
                "plan".to_owned(),
                "-input=false".to_owned(),
                "-json".to_owned(),
                format!("-out={}", plan_path.display()),
            ]
        );
        assert_eq!(
            argument_strings(&invocations[1].arguments),
            vec![
                "show".to_owned(),
                "-json".to_owned(),
                plan_path.to_string_lossy().into_owned(),
            ]
        );
        assert!(!plan_path.exists(), "temporary plan should be removed");
        fs::remove_dir(directory).expect("test directory should be empty");
    }

    #[test]
    fn delivers_plan_events_before_process_termination_and_keeps_show_silent() {
        let refresh_start =
            br#"{"type":"refresh_start","hook":{"resource":{"addr":"aws_vpc.main"}}}
"#
            .to_vec();
        let refresh_complete =
            br#"{"type":"refresh_complete","hook":{"resource":{"addr":"aws_vpc.main"}}}
"#
            .to_vec();
        let runner = FakeRunner::new([
            streaming_process(
                ProcessStatus::Exited(0),
                [refresh_start, refresh_complete],
                [b"provider warning".to_vec()],
            ),
            show_process(),
        ]);
        let cancellation = CancellationToken::new();
        let (saved_plan, directory) = saved_plan_with_space();
        let mut events = Vec::new();

        let result = execute_plan(
            Path::new("/root"),
            saved_plan.path(),
            &cancellation,
            &runner,
            &mut |event| events.push(event),
            &mut |_| {},
        );
        let result = finish_plan(saved_plan, result).expect("plan should be returned");

        assert!(result.resource_changes.is_empty());
        assert!(matches!(
            events.first().map(|event| &event.kind),
            Some(ExecutionEventKind::Resource(ResourceEvent {
                address,
                kind: ResourceEventKind::RefreshStart,
                ..
            })) if address == "aws_vpc.main"
        ));
        assert!(matches!(
            events.get(1).map(|event| &event.kind),
            Some(ExecutionEventKind::Resource(ResourceEvent {
                kind: ResourceEventKind::RefreshComplete,
                ..
            }))
        ));
        assert!(matches!(
            events.get(2).map(|event| &event.kind),
            Some(ExecutionEventKind::Diagnostic(Diagnostic {
                source: DiagnosticSource::NonJson {
                    stream: EventStream::Stderr
                },
                ..
            }))
        ));
        assert!(matches!(
            events.last().map(|event| &event.kind),
            Some(ExecutionEventKind::Terminated(ProcessTermination {
                status: ProcessExitStatus::Exited(0),
                interrupted: false,
            }))
        ));
        assert_eq!(runner.invocations.borrow().len(), 2);
        fs::remove_dir(directory).expect("test directory should be empty");
    }

    #[test]
    fn distinguishes_plan_launch_failure_and_skips_show() {
        let runner = FakeRunner::new([FakeResponse::LaunchError("not found")]);
        let cancellation = CancellationToken::new();
        let (saved_plan, directory) = saved_plan_with_space();

        let error =
            run_fake(&runner, saved_plan, &cancellation).expect_err("plan launch should fail");

        assert!(matches!(
            error.kind(),
            TerraformExecutionErrorKind::Launch {
                command: TerraformCommand::Plan,
                ..
            }
        ));
        assert_eq!(runner.invocations.borrow().len(), 1);
        fs::remove_dir(directory).expect("test directory should be empty");
    }

    #[test]
    fn distinguishes_plan_nonzero_exit_and_keeps_output_out_of_error_text() {
        let runner = FakeRunner::new([FakeResponse::Exit {
            status: ProcessStatus::Exited(1),
            output: ProcessOutput::new(
                b"secret plan value".to_vec(),
                b"secret diagnostic".to_vec(),
            ),
        }]);
        let cancellation = CancellationToken::new();
        let (saved_plan, directory) = saved_plan_with_space();

        let error =
            run_fake(&runner, saved_plan, &cancellation).expect_err("non-zero plan should fail");

        assert!(matches!(
            error.kind(),
            TerraformExecutionErrorKind::NonZero { .. }
        ));
        assert!(!error.to_string().contains("secret"));
        assert!(!format!("{error:?}").contains("secret"));
        assert_eq!(runner.invocations.borrow().len(), 1);
        fs::remove_dir(directory).expect("test directory should be empty");
    }

    #[test]
    fn distinguishes_show_launch_failure_after_a_successful_plan() {
        let runner = FakeRunner::new([
            successful_process(),
            FakeResponse::LaunchError("show unavailable"),
        ]);
        let cancellation = CancellationToken::new();
        let (saved_plan, directory) = saved_plan_with_space();

        let error =
            run_fake(&runner, saved_plan, &cancellation).expect_err("show launch should fail");

        assert!(matches!(
            error.kind(),
            TerraformExecutionErrorKind::Launch {
                command: TerraformCommand::Show,
                ..
            }
        ));
        assert_eq!(runner.invocations.borrow().len(), 2);
        fs::remove_dir(directory).expect("test directory should be empty");
    }

    #[test]
    fn distinguishes_show_nonzero_exit() {
        let runner = FakeRunner::new([
            successful_process(),
            FakeResponse::Exit {
                status: ProcessStatus::Exited(1),
                output: ProcessOutput::new(Vec::new(), b"show failed".to_vec()),
            },
        ]);
        let cancellation = CancellationToken::new();
        let (saved_plan, directory) = saved_plan_with_space();

        let error =
            run_fake(&runner, saved_plan, &cancellation).expect_err("non-zero show should fail");

        assert!(matches!(
            error.kind(),
            TerraformExecutionErrorKind::NonZero {
                command: TerraformCommand::Show,
                ..
            }
        ));
        assert_eq!(runner.invocations.borrow().len(), 2);
        fs::remove_dir(directory).expect("test directory should be empty");
    }

    #[test]
    fn interrupts_and_reaps_plan_before_cleanup_without_starting_show() {
        let cancellation = CancellationToken::new();
        let interrupt_count = Rc::new(Cell::new(0));
        let runner = FakeRunner::new([FakeResponse::Pending {
            cancellation: cancellation.clone(),
            output: ProcessOutput::empty(),
            interrupt_count: interrupt_count.clone(),
        }]);
        let (saved_plan, directory) = saved_plan_with_space();

        let error =
            run_fake(&runner, saved_plan, &cancellation).expect_err("cancelled plan should fail");

        assert!(matches!(
            error.kind(),
            TerraformExecutionErrorKind::Interrupted {
                command: TerraformCommand::Plan,
                ..
            }
        ));
        assert_eq!(
            interrupt_count.get(),
            1,
            "interrupt should be requested once"
        );
        assert_eq!(runner.invocations.borrow().len(), 1);
        fs::remove_dir(directory).expect("test directory should be empty");
    }

    #[test]
    fn interrupts_and_reaps_show_before_cleanup() {
        let cancellation = CancellationToken::new();
        let interrupt_count = Rc::new(Cell::new(0));
        let runner = FakeRunner::new([
            successful_process(),
            FakeResponse::Pending {
                cancellation: cancellation.clone(),
                output: ProcessOutput::empty(),
                interrupt_count: interrupt_count.clone(),
            },
        ]);
        let (saved_plan, directory) = saved_plan_with_space();

        let error =
            run_fake(&runner, saved_plan, &cancellation).expect_err("cancelled show should fail");

        assert!(matches!(
            error.kind(),
            TerraformExecutionErrorKind::Interrupted {
                command: TerraformCommand::Show,
                ..
            }
        ));
        assert_eq!(
            interrupt_count.get(),
            1,
            "interrupt should be requested once"
        );
        assert_eq!(runner.invocations.borrow().len(), 2);
        fs::remove_dir(directory).expect("test directory should be empty");
    }

    #[test]
    fn reports_cleanup_failure_separately() {
        let runner = FakeRunner::new([FakeResponse::MakePlanPathDirectory, show_process()]);
        let cancellation = CancellationToken::new();
        let (saved_plan, directory) = saved_plan_with_space();
        let plan_path = saved_plan.path().to_owned();

        let error = run_fake(&runner, saved_plan, &cancellation)
            .expect_err("cleanup of a directory should fail");

        assert!(matches!(error, test_support::PlanTestError::Cleanup { .. }));
        assert!(
            error
                .to_string()
                .starts_with("failed to remove the temporary Terraform plan: ")
        );
        assert!(plan_path.is_dir());
        fs::remove_dir(plan_path).expect("test plan directory should be removed");
        fs::remove_dir(directory).expect("test directory should be empty");
    }

    #[test]
    fn keeps_parser_failures_distinct_from_process_failures() {
        let runner = FakeRunner::new([
            successful_process(),
            FakeResponse::Exit {
                status: ProcessStatus::Exited(0),
                output: ProcessOutput::new(
                    json!({"format_version": "2.0"}).to_string().into_bytes(),
                    Vec::new(),
                ),
            },
        ]);
        let cancellation = CancellationToken::new();
        let (saved_plan, directory) = saved_plan_with_space();

        let error = run_fake(&runner, saved_plan, &cancellation)
            .expect_err("unsupported plan format should fail");

        assert!(matches!(
            error.kind(),
            TerraformExecutionErrorKind::InvalidOutput {
                command: TerraformCommand::Show,
                ..
            }
        ));
        fs::remove_dir(directory).expect("test directory should be empty");
    }

    #[test]
    #[ignore = "requires Terraform CLI"]
    fn gets_saved_plan_from_the_basic_terraform_scenario() {
        let setup = Command::new("python3")
            .args(["fixtures/basic/plan.py", "test", "setup"])
            .output()
            .expect("scenario setup should start");
        assert!(
            setup.status.success(),
            "scenario setup failed: {}",
            String::from_utf8_lossy(&setup.stderr)
        );
        let directory = PathBuf::from(
            String::from_utf8(setup.stdout)
                .expect("scenario path should be UTF-8")
                .trim(),
        );

        let cancellation = CancellationToken::new();
        let mut ignore_event = |_| {};
        let mut ignore_phase = |_| {};
        let result = run_plan(
            &directory,
            &cancellation,
            &super::super::command::SystemProcessRunner,
            &mut ignore_event,
            &mut ignore_phase,
        );
        let cleanup = Command::new("python3")
            .args(["fixtures/basic/plan.py", "test", "clean"])
            .arg(&directory)
            .output()
            .expect("scenario cleanup should start");
        assert!(
            cleanup.status.success(),
            "scenario cleanup failed: {}",
            String::from_utf8_lossy(&cleanup.stderr)
        );

        let plan = result.expect("Terraform plan should be obtained");
        assert_eq!(plan.summary().creates, 1);
        assert_eq!(plan.summary().updates, 2);
        assert_eq!(plan.summary().replaces, 1);
        assert_eq!(plan.summary().deletes, 1);
    }
}
