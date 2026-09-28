use std::{
    ffi::OsString,
    fmt, fs,
    io::{self, IsTerminal, Write},
    path::Path,
    process::ExitCode,
    sync::mpsc,
    thread::{self, JoinHandle},
    time::Instant,
};

mod environments;
mod event_loop;
pub(crate) mod invocation;
mod synthetic;
mod terminal;

use crate::{
    app::{
        execution::{
            ApplyStatus, ExecutionContext, ExecutionEvent, ExecutionEventKind, ExecutionPhase,
            ExecutionStage, ExecutionState, HistoryKey, Tool, VariableSources,
        },
        plan::PlanSummary,
        review::{PlanReview, PlanReviewMessage},
        session::{ReviewedChanges, SessionOutcome},
    },
    infra::{
        CancellationToken, ClipboardExecutor,
        history::HistoryStore,
        termination::{self, TerminationSignal},
        terraform,
    },
};

#[cfg(feature = "test-support")]
use crate::test_support;

const EXECUTION_FAILURE: u8 = 1;
const INTERRUPTED: u8 = 130;

pub(crate) fn run_plan(root: &Path, compare_ref: Option<&str>) -> ExitCode {
    if compare_ref.is_some() {
        report_error("--compare-ref is unavailable while Git comparison is paused");
        return ExitCode::from(EXECUTION_FAILURE);
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        report_error("terraleph plan requires an interactive terminal");
        return ExitCode::from(EXECUTION_FAILURE);
    }
    let Ok(executable) = terraform::resolve_executable(Tool::Terraform) else {
        report_error("terraform was not found in PATH");
        return ExitCode::from(EXECUTION_FAILURE);
    };
    run_managed_invocation(
        &executable,
        Tool::Terraform,
        root,
        root,
        &[],
        &[OsString::from("-detailed-exitcode")],
        &[],
        false,
        false,
        false,
        invocation::variable_sources(root, &[]).unwrap_or_default(),
    )
}

pub(crate) fn run_invocation(
    executable: &Path,
    invocation: &invocation::Invocation,
    variable_sources: VariableSources,
) -> ExitCode {
    run_managed_invocation(
        executable,
        invocation.tool(),
        invocation.launch_root(),
        invocation.directory(),
        invocation.global_arguments(),
        &invocation.plan_arguments(),
        &invocation.apply_arguments(),
        invocation.is_apply(),
        invocation.detailed_exitcode(),
        invocation.initial_overview(),
        variable_sources,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the managed invocation keeps launch, display, and Terraform argument boundaries"
)]
fn run_managed_invocation(
    executable: &Path,
    tool: Tool,
    launch_root: &Path,
    display_root: &Path,
    global_arguments: &[OsString],
    plan_arguments: &[OsString],
    apply_arguments: &[OsString],
    apply_entry: bool,
    detailed_exitcode: bool,
    initial_overview: bool,
    variable_sources: VariableSources,
) -> ExitCode {
    prepare_saved_plan_lifecycle();
    let (saved_plan, plan_arguments) =
        match terraform::saved_plan_for_plan(display_root, plan_arguments) {
            Ok(result) => result,
            Err(error) => {
                report_error(&format!(
                    "failed to prepare the {} plan: {error}",
                    tool.display_name()
                ));
                return ExitCode::from(EXECUTION_FAILURE);
            }
        };
    let (plan_run, status) = match terraform::run_passthrough_plan(
        executable,
        launch_root,
        global_arguments,
        &plan_arguments,
        saved_plan,
    ) {
        Ok(result) => result,
        Err(error) => {
            report_error(&format!(
                "failed to run {} plan: {error}",
                tool.display_name()
            ));
            return ExitCode::from(EXECUTION_FAILURE);
        }
    };
    // Terraform shares the terminal's process group, so terminal signals stop it on its own. A
    // recorded signal decides the exit before the plan result, whether the plan failed from that
    // same signal or finished despite a signal sent only to Terraleph.
    if let Some(signal) = termination::received() {
        let _ = plan_run.saved_plan.cleanup();
        report_terminated(signal);
        return ExitCode::from(signal.exit_code());
    }
    if !status.is_plan_success() {
        let exit = if status == terraform::ProcessStatus::Signaled
            || status.code() == Some(i32::from(INTERRUPTED))
        {
            INTERRUPTED
        } else {
            EXECUTION_FAILURE
        };
        let _ = plan_run.saved_plan.cleanup();
        return ExitCode::from(exit);
    }
    run_saved_plan_review(
        tool,
        launch_root,
        display_root,
        global_arguments,
        apply_arguments,
        apply_entry,
        plan_run,
        detailed_exitcode,
        initial_overview,
        variable_sources,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the runtime passes each execution boundary to the review worker"
)]
fn run_saved_plan_review(
    tool: Tool,
    launch_root: &Path,
    display_root: &Path,
    global_arguments: &[OsString],
    apply_arguments: &[OsString],
    apply_entry: bool,
    plan_run: terraform::PlanRun,
    detailed_exitcode: bool,
    initial_overview: bool,
    variable_sources: VariableSources,
) -> ExitCode {
    let changed = plan_run.changed;
    let saved_plan = plan_run.saved_plan;
    let review_root = match fs::canonicalize(display_root) {
        Ok(root) => root,
        Err(error) => {
            report_error(&format!(
                "failed to resolve the {} execution directory before review: {error}",
                tool.display_name()
            ));
            let _ = saved_plan.cleanup();
            return ExitCode::from(EXECUTION_FAILURE);
        }
    };
    let cancellation = CancellationToken::new();
    let (sender, receiver) = mpsc::channel();
    let history = HistoryStore::platform();
    let context = ExecutionContext::loading(review_root)
        .with_tool(tool)
        .with_launch_root(launch_root)
        .with_variable_sources(variable_sources);
    let worker = match spawn_review_worker(
        tool,
        display_root,
        launch_root,
        global_arguments,
        saved_plan.path(),
        changed,
        apply_entry,
        context.clone(),
        &cancellation,
        history.as_ref(),
        sender.clone(),
    ) {
        Ok(worker) => worker,
        Err(error) => {
            report_error(&format!("failed to start the plan worker: {error}"));
            let _ = saved_plan.cleanup();
            return ExitCode::from(EXECUTION_FAILURE);
        }
    };
    let mut worker = WorkerGuard {
        cancellation: cancellation.clone(),
        handle: Some(worker),
    };
    let mut apply_worker = WorkerGuard {
        cancellation: cancellation.clone(),
        handle: None,
    };
    let mut clipboard = ClipboardExecutor::new();
    let effects = event_loop::RuntimeEffects {
        tool,
        root: launch_root,
        display_root,
        global_arguments,
        apply_arguments,
        sender: &sender,
        plan_path: saved_plan.path(),
        cancellation: &cancellation,
        clipboard: &mut clipboard,
        apply_worker: &mut apply_worker,
        history: history.as_ref(),
    };
    let ui_result = run_interactive(context, &receiver, &mut worker, effects, initial_overview);
    let (ui_result, cleanup_result) = finish_review(
        ui_result,
        &cancellation,
        &mut apply_worker,
        &mut worker,
        saved_plan,
    );
    let primary_exit = review_exit(ui_result, apply_entry, detailed_exitcode, changed);
    if let Err(error) = cleanup_result {
        report_error(&format!(
            "failed to remove the temporary {} plan: {error}",
            tool.display_name()
        ));
        ExitCode::from(EXECUTION_FAILURE)
    } else {
        primary_exit
    }
}

// Every entry that creates a Terraleph-owned plan runs this first. Delegated commands never
// reach it, so they keep the default signal dispositions.
fn prepare_saved_plan_lifecycle() {
    terraform::remove_orphaned_plans();
    if let Err(error) = termination::install() {
        report_error(&format!("failed to handle termination signals: {error}"));
    }
}

fn review_exit(
    ui_result: io::Result<SessionOutcome>,
    apply_entry: bool,
    detailed_exitcode: bool,
    changed: bool,
) -> ExitCode {
    // A signal that arrives after the session has an outcome does not replace that outcome.
    if ui_result.is_err()
        && let Some(signal) = termination::received()
    {
        report_terminated(signal);
        return ExitCode::from(signal.exit_code());
    }
    match ui_result {
        Ok(SessionOutcome::Reviewed { changes }) => {
            report_reviewed(changes);
            if !apply_entry && detailed_exitcode && changed {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            }
        }
        Ok(SessionOutcome::NoChanges) => {
            report_no_changes();
            ExitCode::SUCCESS
        }
        Ok(SessionOutcome::ApplyCanceled) => {
            report_apply_canceled();
            ExitCode::from(EXECUTION_FAILURE)
        }
        Ok(SessionOutcome::Applied {
            status,
            summary_line,
        }) => report_applied(status, summary_line.as_deref()),
        Ok(SessionOutcome::Interrupted(phase)) => {
            report_interrupted(phase);
            ExitCode::from(INTERRUPTED)
        }
        Ok(SessionOutcome::Failed(phase)) => {
            report_error(&format!("{} failed.", phase.title()));
            ExitCode::from(EXECUTION_FAILURE)
        }
        Err(error) => {
            report_error(&format!("TUI failed: {error}"));
            ExitCode::from(EXECUTION_FAILURE)
        }
    }
}

pub(crate) fn run_synthetic() -> io::Result<()> {
    synthetic::run_synthetic()
}

pub(crate) fn run_synthetic_execution() -> io::Result<()> {
    synthetic::run_synthetic_execution()
}

fn run_interactive(
    context: ExecutionContext,
    receiver: &mpsc::Receiver<PlanReviewMessage>,
    plan_worker: &mut WorkerGuard,
    effects: event_loop::RuntimeEffects<'_, ClipboardExecutor>,
    initial_overview: bool,
) -> io::Result<SessionOutcome> {
    terminal::run(|terminal| {
        #[cfg(feature = "test-support")]
        if test_support::panic_after_draw_requested() {
            terminal.draw(|_| {})?;
            panic!("synthetic terminal panic");
        }

        event_loop::run_connected(
            terminal,
            ExecutionState::with_context(Instant::now(), context),
            receiver,
            plan_worker,
            effects,
            initial_overview,
        )
    })
}

fn report_reviewed(changes: Option<ReviewedChanges>) {
    let _ = writeln!(io::stdout(), "{}", reviewed_report(changes));
}

fn reviewed_report(changes: Option<ReviewedChanges>) -> String {
    let Some(ReviewedChanges { resources, outputs }) = changes else {
        return "No changes.".to_owned();
    };
    let mut lines = vec![format!(
        "Plan: {} to add, {} to change, {} to replace, {} to destroy.",
        resources.creates, resources.updates, resources.replaces, resources.deletes
    )];
    // Without resource changes, the zero counts alone would not say what the plan changes.
    if resources == PlanSummary::default() && outputs > 0 {
        lines.push(format!("Outputs: {outputs} changed."));
    }
    lines.push("Apply was not run.".to_owned());
    lines.join("\n")
}

fn report_no_changes() {
    let _ = writeln!(io::stdout(), "No changes.");
}

fn report_apply_canceled() {
    let _ = writeln!(io::stdout(), "Apply canceled.");
}

fn report_applied(status: ApplyStatus, summary_line: Option<&str>) -> ExitCode {
    match status {
        ApplyStatus::Succeeded => {
            report_apply_success(summary_line);
            ExitCode::SUCCESS
        }
        ApplyStatus::Failed => {
            report_apply_failure(false);
            ExitCode::from(EXECUTION_FAILURE)
        }
        ApplyStatus::Interrupted => {
            report_apply_failure(true);
            ExitCode::from(INTERRUPTED)
        }
    }
}

fn report_apply_success(summary_line: Option<&str>) {
    let _ = writeln!(
        io::stdout(),
        "{}",
        summary_line.unwrap_or("Apply complete.")
    );
}

fn report_apply_failure(interrupted: bool) {
    let result = if interrupted {
        "Apply interrupted. Changes may already be applied."
    } else {
        "Apply failed. Changes may already be applied."
    };
    let _ = writeln!(io::stdout(), "{result}");
}

fn report_terminated(signal: TerminationSignal) {
    let _ = writeln!(io::stderr(), "Stopped by {}.", signal.name());
}

fn report_interrupted(phase: ExecutionStage) {
    let message = match phase {
        ExecutionStage::Initializing => "Initialization cancelled.",
        _ => "Plan cancelled.",
    };
    let _ = writeln!(io::stdout(), "{message}");
}

// The saved plan outlives both workers: it is removed once, only after every worker that may
// read it has been joined.
fn finish_review(
    ui_result: io::Result<SessionOutcome>,
    cancellation: &CancellationToken,
    apply_worker: &mut WorkerGuard,
    plan_worker: &mut WorkerGuard,
    saved_plan: terraform::SavedPlan,
) -> (io::Result<SessionOutcome>, io::Result<()>) {
    if ui_result.is_err() {
        cancellation.cancel();
    }
    let apply_join = apply_worker.join();
    let plan_join = plan_worker.join();
    let ui_result = finalize_ui_result(ui_result, &apply_join, &plan_join);
    (ui_result, saved_plan.cleanup())
}

fn finalize_ui_result(
    ui_result: io::Result<SessionOutcome>,
    apply_join: &thread::Result<()>,
    plan_join: &thread::Result<()>,
) -> io::Result<SessionOutcome> {
    let ui_worker_panic = ui_result.as_ref().err().and_then(worker_panic_kind);
    if ui_result.is_err() && ui_worker_panic.is_none() {
        return ui_result;
    }
    if apply_join.is_err() {
        return Err(worker_panic_error(WorkerKind::Apply));
    }
    if ui_worker_panic.is_some() {
        return ui_result;
    }
    if plan_join.is_err() {
        return Err(worker_panic_error(WorkerKind::Plan));
    }
    ui_result
}

fn worker_panic_kind(error: &io::Error) -> Option<WorkerKind> {
    error
        .get_ref()
        .and_then(|source| source.downcast_ref::<WorkerPanic>())
        .map(|panic| panic.0)
}

fn worker_panic_error(worker: WorkerKind) -> io::Error {
    io::Error::other(WorkerPanic(worker))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerKind {
    Plan,
    Apply,
}

#[derive(Debug)]
struct WorkerPanic(WorkerKind);

impl fmt::Display for WorkerPanic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let worker = match self.0 {
            WorkerKind::Plan => "plan",
            WorkerKind::Apply => "apply",
        };
        write!(formatter, "{worker} worker panicked")
    }
}

impl std::error::Error for WorkerPanic {}

#[expect(
    clippy::too_many_arguments,
    reason = "the worker receives the explicit plan execution boundaries"
)]
fn spawn_review_worker(
    tool: Tool,
    display_root: &Path,
    launch_root: &Path,
    global_arguments: &[OsString],
    plan_path: &Path,
    plan_changed: bool,
    apply_entry: bool,
    initial_context: ExecutionContext,
    cancellation: &CancellationToken,
    history: Option<&HistoryStore>,
    sender: mpsc::Sender<PlanReviewMessage>,
) -> io::Result<JoinHandle<()>> {
    let worker_cancellation = cancellation.clone();
    let worker_display_root = display_root.to_owned();
    let worker_launch_root = launch_root.to_owned();
    let worker_global_arguments = global_arguments.to_vec();
    let worker_plan_path = plan_path.to_owned();
    let worker_initial_context = initial_context;
    let worker_history = history.cloned();
    thread::Builder::new()
        .name("terraleph-plan".to_owned())
        .spawn(move || {
            let mut event_sink = |event| {
                let _ = sender.send(PlanReviewMessage::Event(event));
            };
            let mut phase_sink = |phase: ExecutionPhase| {
                let _ = sender.send(PlanReviewMessage::Event(ExecutionEvent {
                    received_at: Instant::now(),
                    kind: ExecutionEventKind::Phase(phase),
                }));
            };
            match terraform::read_saved_plan_review(
                tool,
                &worker_display_root,
                &worker_launch_root,
                &worker_global_arguments,
                &worker_plan_path,
                plan_changed,
                apply_entry,
                worker_initial_context,
                &worker_cancellation,
                &terraform::SystemProcessRunner,
                &mut event_sink,
                &mut phase_sink,
            ) {
                Ok(review) => {
                    let review = with_previous_durations(review, worker_history.as_ref());
                    if !worker_cancellation.is_cancelled() {
                        let _ = sender.send(PlanReviewMessage::Completed(review));
                    }
                }
                Err(error) => {
                    let _ = sender.send(PlanReviewMessage::Failed {
                        message: error.to_string(),
                        interrupted: worker_cancellation.is_cancelled(),
                    });
                }
            }
        })
}

fn with_previous_durations(review: PlanReview, history: Option<&HistoryStore>) -> PlanReview {
    let Some(history) = history else {
        return review;
    };
    let keys: Vec<_> = review
        .apply_targets()
        .iter()
        .map(|target| HistoryKey::for_target(review.context(), target))
        .collect();
    let previous_durations = history.load_many(&keys);
    review.with_previous_durations(previous_durations)
}

pub(super) fn spawn_apply_worker(
    tool: Tool,
    root: &Path,
    global_arguments: &[OsString],
    apply_arguments: &[OsString],
    plan_path: &Path,
    cancellation: &CancellationToken,
    sender: &mpsc::Sender<PlanReviewMessage>,
) -> io::Result<JoinHandle<()>> {
    let worker_root = root.to_owned();
    let worker_global_arguments = global_arguments.to_vec();
    let worker_apply_arguments = apply_arguments.to_vec();
    let worker_plan_path = plan_path.to_owned();
    let worker_cancellation = cancellation.clone();
    let worker_sender = sender.clone();
    thread::Builder::new()
        .name("terraleph-apply".to_owned())
        .spawn(move || {
            let mut event_sink = |event| {
                let _ = worker_sender.send(PlanReviewMessage::ApplyEvent(event));
            };
            match terraform::run_apply_with_arguments(
                tool,
                &worker_root,
                &worker_global_arguments,
                &worker_apply_arguments,
                &worker_plan_path,
                &worker_cancellation,
                &terraform::SystemProcessRunner,
                &mut event_sink,
            ) {
                Ok(result) => {
                    let _ = worker_sender.send(PlanReviewMessage::ApplyCompleted {
                        status: result.status(),
                        summary_line: result.summary_line().map(str::to_owned),
                    });
                }
                Err(error) => {
                    let _ = worker_sender.send(PlanReviewMessage::ApplyFailed {
                        message: error.to_string(),
                    });
                }
            }
        })
}

fn report_error(message: &str) {
    let _ = writeln!(io::stderr(), "{message}");
}

pub(super) struct WorkerGuard {
    cancellation: CancellationToken,
    handle: Option<JoinHandle<()>>,
}

impl WorkerGuard {
    fn set_handle(&mut self, handle: JoinHandle<()>) {
        debug_assert!(self.handle.is_none());
        self.handle = Some(handle);
    }

    fn poll_finished(&mut self) -> Option<thread::Result<()>> {
        self.handle
            .as_ref()
            .is_some_and(JoinHandle::is_finished)
            .then(|| self.join())
    }

    fn join(&mut self) -> thread::Result<()> {
        self.handle.take().map_or(Ok(()), JoinHandle::join)
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, time::Duration};

    use super::*;

    fn panic_join() -> thread::Result<()> {
        Err(Box::new("worker panic"))
    }

    fn temporary_saved_plan() -> terraform::SavedPlan {
        let (saved_plan, _) = terraform::saved_plan_for_plan(Path::new("."), &[])
            .expect("a temporary saved plan should be created");
        saved_plan
    }

    // Reports whether cancellation was seen and the saved plan still existed when the worker was
    // about to exit, which fails if cleanup ran before the worker was joined.
    fn plan_reading_worker(
        plan_path: PathBuf,
        cancellation: &CancellationToken,
        wait_for_cancellation: bool,
    ) -> (WorkerGuard, mpsc::Receiver<(bool, bool)>) {
        let (observed, observations) = mpsc::channel();
        let worker_cancellation = cancellation.clone();
        let handle = thread::spawn(move || {
            let started = Instant::now();
            while wait_for_cancellation
                && !worker_cancellation.is_cancelled()
                && started.elapsed() < Duration::from_secs(5)
            {
                thread::sleep(Duration::from_millis(1));
            }
            thread::sleep(Duration::from_millis(20));
            let _ = observed.send((worker_cancellation.is_cancelled(), plan_path.exists()));
        });
        let guard = WorkerGuard {
            cancellation: cancellation.clone(),
            handle: Some(handle),
        };
        (guard, observations)
    }

    fn idle_worker(cancellation: &CancellationToken) -> WorkerGuard {
        WorkerGuard {
            cancellation: cancellation.clone(),
            handle: None,
        }
    }

    #[test]
    fn normal_exit_joins_both_workers_before_removing_the_saved_plan() {
        let saved_plan = temporary_saved_plan();
        let plan_path = saved_plan.path().to_owned();
        let cancellation = CancellationToken::new();
        let (mut plan_worker, plan_observed) =
            plan_reading_worker(plan_path.clone(), &cancellation, false);
        let (mut apply_worker, apply_observed) =
            plan_reading_worker(plan_path.clone(), &cancellation, false);
        let outcome = SessionOutcome::NoChanges;

        let (ui_result, cleanup) = finish_review(
            Ok(outcome.clone()),
            &cancellation,
            &mut apply_worker,
            &mut plan_worker,
            saved_plan,
        );

        assert_eq!(ui_result.expect("the UI outcome should be kept"), outcome);
        cleanup.expect("the saved plan should be removed");
        assert!(!cancellation.is_cancelled());
        assert_eq!(plan_observed.try_recv(), Ok((false, true)));
        assert_eq!(apply_observed.try_recv(), Ok((false, true)));
        assert!(!plan_path.exists());
    }

    #[test]
    fn ui_error_cancels_and_joins_the_worker_before_removing_the_saved_plan() {
        let saved_plan = temporary_saved_plan();
        let plan_path = saved_plan.path().to_owned();
        let cancellation = CancellationToken::new();
        let (mut plan_worker, plan_observed) =
            plan_reading_worker(plan_path.clone(), &cancellation, true);
        let mut apply_worker = idle_worker(&cancellation);

        let (ui_result, cleanup) = finish_review(
            Err(io::Error::other("terminal failed")),
            &cancellation,
            &mut apply_worker,
            &mut plan_worker,
            saved_plan,
        );

        let error = ui_result.expect_err("the UI error should be kept");
        assert_eq!(error.to_string(), "terminal failed");
        cleanup.expect("the saved plan should be removed");
        assert_eq!(plan_observed.try_recv(), Ok((true, true)));
        assert!(!plan_path.exists());
    }

    #[test]
    fn worker_panic_fails_the_review_and_still_removes_the_saved_plan() {
        let saved_plan = temporary_saved_plan();
        let plan_path = saved_plan.path().to_owned();
        let cancellation = CancellationToken::new();
        let mut plan_worker = WorkerGuard {
            cancellation: cancellation.clone(),
            handle: Some(thread::spawn(|| panic!("secret panic payload"))),
        };
        let mut apply_worker = idle_worker(&cancellation);

        let (ui_result, cleanup) = finish_review(
            Ok(SessionOutcome::Interrupted(ExecutionStage::Initializing)),
            &cancellation,
            &mut apply_worker,
            &mut plan_worker,
            saved_plan,
        );

        let error = ui_result.expect_err("the worker panic should fail the review");
        assert_eq!(error.to_string(), "plan worker panicked");
        cleanup.expect("the saved plan should be removed");
        assert!(!plan_path.exists());
    }

    #[test]
    fn ui_error_takes_precedence_over_worker_panics() {
        let error = finalize_ui_result(
            Err(io::Error::other("terminal failed")),
            &panic_join(),
            &panic_join(),
        )
        .expect_err("the UI error should be returned");

        assert_eq!(error.to_string(), "terminal failed");
    }

    #[test]
    fn ui_error_with_worker_panic_text_is_not_reclassified() {
        let ui_error = io::Error::other("apply worker panicked");
        let error = finalize_ui_result(Err(ui_error), &panic_join(), &panic_join())
            .expect_err("the original UI error should be returned");

        assert_eq!(error.to_string(), "apply worker panicked");
        assert!(worker_panic_kind(&error).is_none());
    }

    #[test]
    fn apply_panic_takes_precedence_over_plan_panic_after_successful_ui() {
        let error = finalize_ui_result(
            Ok(SessionOutcome::Interrupted(ExecutionStage::Initializing)),
            &panic_join(),
            &panic_join(),
        )
        .expect_err("a worker panic should fail a successful UI result");

        assert_eq!(error.to_string(), "apply worker panicked");
    }

    #[test]
    fn apply_join_panic_takes_precedence_over_an_earlier_plan_panic() {
        let apply_join = panic_join();
        let plan_join = Ok(());
        let error = finalize_ui_result(
            Err(worker_panic_error(WorkerKind::Plan)),
            &apply_join,
            &plan_join,
        )
        .expect_err("a worker panic should fail the runtime");

        assert_eq!(error.to_string(), "apply worker panicked");
    }

    #[test]
    fn apply_ui_panic_takes_precedence_over_a_later_plan_join_panic() {
        let apply_join = Ok(());
        let plan_join = panic_join();
        let error = finalize_ui_result(
            Err(worker_panic_error(WorkerKind::Apply)),
            &apply_join,
            &plan_join,
        )
        .expect_err("the earlier apply panic should remain primary");

        assert_eq!(error.to_string(), "apply worker panicked");
        assert_eq!(worker_panic_kind(&error), Some(WorkerKind::Apply));
    }

    #[test]
    fn successful_worker_joins_preserve_the_ui_outcome() {
        let outcome = SessionOutcome::Interrupted(ExecutionStage::Initializing);
        let apply_join = Ok(());
        let plan_join = Ok(());
        let actual = finalize_ui_result(Ok(outcome.clone()), &apply_join, &plan_join)
            .expect("successful worker joins should preserve the UI result");
        assert_eq!(actual, outcome);
    }

    #[test]
    fn reviewed_report_adds_changed_outputs_only_without_resource_changes() {
        struct ReportCase {
            name: &'static str,
            changes: Option<ReviewedChanges>,
            expected: &'static str,
        }

        for case in [
            ReportCase {
                name: "no_changes",
                changes: None,
                expected: "No changes.",
            },
            ReportCase {
                name: "outputs_only",
                changes: Some(ReviewedChanges {
                    resources: PlanSummary::default(),
                    outputs: 3,
                }),
                expected: "Plan: 0 to add, 0 to change, 0 to replace, 0 to destroy.\nOutputs: 3 changed.\nApply was not run.",
            },
            ReportCase {
                name: "resources_and_outputs",
                changes: Some(ReviewedChanges {
                    resources: PlanSummary {
                        creates: 1,
                        updates: 2,
                        replaces: 3,
                        deletes: 4,
                    },
                    outputs: 1,
                }),
                expected: "Plan: 1 to add, 2 to change, 3 to replace, 4 to destroy.\nApply was not run.",
            },
            ReportCase {
                name: "nonstandard_only",
                changes: Some(ReviewedChanges {
                    resources: PlanSummary::default(),
                    outputs: 0,
                }),
                expected: "Plan: 0 to add, 0 to change, 0 to replace, 0 to destroy.\nApply was not run.",
            },
        ] {
            assert_eq!(
                reviewed_report(case.changes),
                case.expected,
                "case: {}",
                case.name
            );
        }
    }
}
