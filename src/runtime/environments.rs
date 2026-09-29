use std::{
    ffi::OsString,
    io,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crossterm::event::{Event, KeyEventKind};
use ratatui::{Terminal, backend::Backend};

use crate::{
    app::{
        environments::{Environment, EnvironmentSession, PlanResult},
        execution::{Diagnostic, ExecutionContext, Tool},
        review::PlanReviewMessage,
        session::{Effect, SessionOutcome, SessionState},
    },
    infra::{
        CancellationToken, ClipboardExecutor,
        history::HistoryStore,
        termination,
        terraform::{
            self,
            configuration::{self, ExecutionLocation},
        },
    },
    ui::features::{
        environments::{EnvironmentInput, EnvironmentView},
        execution::ExecutionViewState,
    },
};

use super::{
    WorkerGuard,
    event_loop::{self, ClipboardWriter, RuntimeEffects, SessionViews},
    invocation::Invocation,
    terminal::{self, TerminalInput},
};

struct Completion {
    index: usize,
    result: PlanResult,
    diagnostics: Vec<Diagnostic>,
}

#[expect(
    clippy::too_many_lines,
    reason = "the environment loop owns plan acquisition, review input, and the apply hand-off"
)]
pub(super) fn run(invocation: &Invocation, environments: Vec<Environment>) -> io::Result<ExitCode> {
    super::prepare_saved_plan_lifecycle();
    let mut state = EnvironmentSession::new(environments, invocation.detailed_exitcode())
        .with_exploration_root(invocation.directory().to_owned());
    let cancellation = CancellationToken::new();
    let (sender, receiver) = mpsc::channel();
    let mut plans: Vec<Option<terraform::SavedPlan>> =
        (0..state.plans().len()).map(|_| None).collect();
    let mut worker = WorkerGuard {
        cancellation: cancellation.clone(),
        handle: None,
    };
    let mut view = EnvironmentView::default();
    let mut clipboard = ClipboardExecutor::new();
    let mut apply_runtime = ApplyRuntime::new(invocation);
    let mut apply: Option<EnvironmentApply> = None;
    let mut outcome = None;
    let mut dirty = true;
    let mut stopped_by = None;
    let result = terminal::run(|terminal| -> io::Result<()> {
        let input = TerminalInput::spawn()?;
        loop {
            stopped_by = termination::requested();
            if stopped_by.is_some() {
                break;
            }
            if let Some(active) = apply.as_mut() {
                match apply_runtime.step(
                    terminal,
                    &input,
                    &mut state,
                    &mut view,
                    active,
                    &plans,
                    &mut clipboard,
                    &mut dirty,
                )? {
                    ApplyStep::Continue => {}
                    ApplyStep::Closed => apply = None,
                    ApplyStep::Finished(finished) => {
                        outcome = Some(finished);
                        break;
                    }
                }
                continue;
            }
            if let Ok(completion) = receiver.try_recv() {
                worker
                    .join()
                    .map_err(|_| io::Error::other("environment worker panicked"))?;
                accept_completion(&mut state, completion);
                dirty = true;
            } else if let Some(result) = worker.poll_finished() {
                result.map_err(|_| io::Error::other("environment worker panicked"))?;
                let completion = receiver
                    .try_recv()
                    .map_err(|_| io::Error::other("environment worker returned no result"))?;
                accept_completion(&mut state, completion);
                dirty = true;
            }
            if cancellation.is_cancelled() {
                state.interrupt();
                break;
            }
            if let Some(index) = state.start_next() {
                dirty = true;
                if let Err(error) = start_worker(
                    invocation,
                    &state,
                    index,
                    &mut plans,
                    &mut worker,
                    &sender,
                    apply_runtime.history.as_ref(),
                ) {
                    state.complete(index, PlanResult::Error(error.to_string()), Vec::new());
                }
            }
            dirty |= state.clear_expired_copy_feedback(std::time::Instant::now());
            draw_if_needed(&state, &mut view, terminal, &mut dirty)?;
            let Some(input_event) = input.next(Duration::from_millis(50))? else {
                continue;
            };
            if !event_requires_draw(&input_event) {
                continue;
            }
            dirty = true;
            if let Event::Key(key) = input_event {
                let size = terminal.size()?;
                let Some(input) = view.handle_key(key, size, &state) else {
                    continue;
                };
                match input {
                    EnvironmentInput::Quit => break,
                    EnvironmentInput::Interrupt => {
                        state.interrupt();
                        break;
                    }
                    EnvironmentInput::Retry(index) => {
                        state.retry(index);
                    }
                    EnvironmentInput::Review(index, action) => {
                        if let Some(Effect::WriteClipboard(effect)) =
                            state.update_review(index, *action, std::time::Instant::now())
                        {
                            state.update_review(
                                index,
                                event_loop::complete_copy(&mut clipboard, &effect),
                                std::time::Instant::now(),
                            );
                        }
                        if state.plans()[index]
                            .session()
                            .and_then(SessionState::apply_confirmation)
                            .is_some()
                        {
                            apply = Some(EnvironmentApply::new(&state, index));
                        }
                    }
                }
            }
        }
        Ok(())
    });
    // A closed terminal fails the loop before the next signal check, so an error also defers to a
    // signal that has already arrived.
    let stopped_by =
        stopped_by.or_else(|| result.as_ref().err().and_then(|_| termination::received()));
    cancellation.cancel();
    if result.is_err() || stopped_by.is_some() {
        apply_runtime.cancellation.cancel();
    }
    let joined = worker.join();
    let apply_joined = apply_runtime.worker.join();
    let cleanup = cleanup_plans(plans);
    if let Some(signal) = stopped_by {
        cleanup?;
        super::report_terminated(signal);
        return Ok(ExitCode::from(signal.exit_code()));
    }
    result?;
    joined.map_err(|_| io::Error::other("environment worker panicked"))?;
    apply_joined.map_err(|_| super::worker_panic_error(super::WorkerKind::Apply))?;
    cleanup?;
    Ok(match outcome {
        Some(SessionOutcome::Applied {
            status,
            summary_line,
        }) => super::report_applied(status, summary_line.as_deref()),
        _ => ExitCode::from(state.exit_code()),
    })
}

struct EnvironmentApply {
    index: usize,
    // Copied from the plan, because the apply effects borrow them while its session is updated.
    tool: Tool,
    root: PathBuf,
    views: SessionViews,
}

impl EnvironmentApply {
    fn new(state: &EnvironmentSession, index: usize) -> Self {
        let plan = &state.plans()[index];
        Self {
            index,
            tool: plan.tool,
            root: plan.directory().to_owned(),
            views: SessionViews::default(),
        }
    }
}

struct ApplyRuntime {
    apply_arguments: Vec<OsString>,
    cancellation: CancellationToken,
    sender: mpsc::Sender<PlanReviewMessage>,
    receiver: mpsc::Receiver<PlanReviewMessage>,
    worker: WorkerGuard,
    history: Option<HistoryStore>,
}

enum ApplyStep {
    Continue,
    Closed,
    Finished(SessionOutcome),
}

impl ApplyRuntime {
    #[expect(
        clippy::too_many_arguments,
        reason = "the apply step reads input, draws the environment view, and routes its effects"
    )]
    fn step<B: Backend<Error = io::Error>>(
        &mut self,
        terminal: &mut Terminal<B>,
        input: &TerminalInput,
        state: &mut EnvironmentSession,
        view: &mut EnvironmentView,
        apply: &mut EnvironmentApply,
        plans: &[Option<terraform::SavedPlan>],
        clipboard: &mut ClipboardExecutor,
        dirty: &mut bool,
    ) -> io::Result<ApplyStep> {
        // Apply runs only the saved plan acquired and reviewed for this environment, in its own
        // directory; it never re-plans and never touches another environment's plan.
        let mut effects = RuntimeEffects {
            tool: apply.tool,
            root: &apply.root,
            display_root: &apply.root,
            global_arguments: &[],
            apply_arguments: &self.apply_arguments,
            sender: &self.sender,
            plan_path: plans
                .get(apply.index)
                .and_then(Option::as_ref)
                .map(terraform::SavedPlan::path),
            cancellation: &self.cancellation,
            clipboard,
            apply_worker: &mut self.worker,
            history: self.history.as_ref(),
        };
        let (outcome, redraw) = receive_apply(
            &self.receiver,
            state.session_mut(apply.index),
            &mut apply.views.execution,
            &mut effects,
        )?;
        *dirty |= redraw;
        if let Some(outcome) = outcome {
            return Ok(ApplyStep::Finished(outcome));
        }
        draw_apply_if_needed(
            terminal,
            state,
            view,
            apply.index,
            &mut apply.views,
            dirty,
            Instant::now(),
        )?;
        let Some(input_event) = input.next(Duration::from_millis(50))? else {
            return Ok(ApplyStep::Continue);
        };
        if !event_requires_draw(&input_event) {
            return Ok(ApplyStep::Continue);
        }
        *dirty = true;
        if let Event::Key(key) = input_event
            && let Some(session) = state.session_mut(apply.index)
            && let Some(action) = apply.views.handle_key(terminal, session, key)?
            && let Some(outcome) =
                event_loop::dispatch(session, action, &mut apply.views.execution, &mut effects)
        {
            return Ok(ApplyStep::Finished(outcome));
        }
        Ok(if state.plans()[apply.index].review().is_some() {
            ApplyStep::Closed
        } else {
            ApplyStep::Continue
        })
    }

    fn new(invocation: &Invocation) -> Self {
        let cancellation = CancellationToken::new();
        let (sender, receiver) = mpsc::channel();
        Self {
            apply_arguments: invocation.apply_arguments(),
            worker: WorkerGuard {
                cancellation: cancellation.clone(),
                handle: None,
            },
            cancellation,
            sender,
            receiver,
            history: HistoryStore::platform(),
        }
    }
}

// A worker panic is reported before any message is read. A disconnect is judged only after the
// messages the worker sent have been processed, so a final message is never lost to it.
fn receive_apply<C: ClipboardWriter>(
    messages: &mpsc::Receiver<PlanReviewMessage>,
    session: Option<&mut SessionState>,
    execution_view: &mut ExecutionViewState,
    effects: &mut RuntimeEffects<'_, C>,
) -> io::Result<(Option<SessionOutcome>, bool)> {
    let finished = effects.apply_worker.poll_finished();
    if finished.as_ref().is_some_and(Result::is_err) {
        return Err(super::worker_panic_error(super::WorkerKind::Apply));
    }
    let Some(session) = session else {
        return Ok((None, false));
    };
    let (outcome, received) =
        event_loop::receive_messages(messages, session, execution_view, effects);
    if outcome.is_some() {
        return Ok((outcome, received));
    }
    let (outcome, disconnected) =
        event_loop::dispatch_apply_disconnect(session, execution_view, finished.is_some(), effects);
    Ok((outcome, received || disconnected))
}

fn draw_apply_if_needed<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &mut EnvironmentSession,
    view: &mut EnvironmentView,
    index: usize,
    views: &mut SessionViews,
    dirty: &mut bool,
    now: Instant,
) -> Result<bool, B::Error> {
    *dirty |= state.clear_expired_copy_feedback(now);
    *dirty |= views.scheduled_draw.is_some_and(|at| now >= at);
    let running = state.plans()[index]
        .session()
        .and_then(SessionState::apply)
        .is_some_and(|apply| apply.result().is_none());
    if !*dirty && !running {
        return Ok(false);
    }
    draw_apply(terminal, state, view, index, views, now)?;
    *dirty = false;
    Ok(true)
}

fn draw_apply<B: Backend>(
    terminal: &mut Terminal<B>,
    state: &EnvironmentSession,
    view: &mut EnvironmentView,
    index: usize,
    views: &mut SessionViews,
    now: Instant,
) -> Result<(), B::Error> {
    let Some(session) = state.plans()[index].session() else {
        views.scheduled_draw = None;
        return Ok(());
    };
    if let Some(confirmation) = session.apply_confirmation() {
        terminal.draw(|frame| {
            view.render_apply_confirmation(
                frame,
                state,
                index,
                confirmation,
                &views.confirmation,
                now,
            );
        })?;
    } else if session.apply().is_some() {
        event_loop::draw_with_quit_confirmation(session, terminal, views, now)?;
    }
    views.scheduled_draw = event_loop::scheduled_draw_after(session, now);
    Ok(())
}

fn should_draw(state: &EnvironmentSession, dirty: bool) -> bool {
    dirty || state.acquiring()
}

fn draw_if_needed<B: Backend>(
    state: &EnvironmentSession,
    view: &mut EnvironmentView,
    terminal: &mut Terminal<B>,
    dirty: &mut bool,
) -> Result<bool, B::Error> {
    if !should_draw(state, *dirty) {
        return Ok(false);
    }
    terminal.draw(|frame| view.render(frame, state))?;
    *dirty = false;
    Ok(true)
}

fn event_requires_draw(event: &Event) -> bool {
    match event {
        Event::Resize(_, _) => true,
        Event::Key(key) => key.kind != KeyEventKind::Release,
        _ => false,
    }
}

fn accept_completion(state: &mut EnvironmentSession, completion: Completion) {
    state.complete(completion.index, completion.result, completion.diagnostics);
}

fn start_worker(
    invocation: &Invocation,
    state: &EnvironmentSession,
    index: usize,
    plans: &mut [Option<terraform::SavedPlan>],
    worker: &mut WorkerGuard,
    sender: &mpsc::Sender<Completion>,
    history: Option<&HistoryStore>,
) -> io::Result<()> {
    if let Some(old_plan) = plans[index].take() {
        old_plan.cleanup()?;
    }
    let environment = &state.plans()[index];
    let root = environment.directory().to_owned();
    let tool = environment.tool;
    let (saved_plan, arguments) =
        terraform::saved_plan_for_plan(&root, &invocation.plan_arguments())?;
    let plan_path = saved_plan.path().to_owned();
    plans[index] = Some(saved_plan);
    let cancellation = worker.cancellation.clone();
    let sender = sender.clone();
    let launch_root = invocation.directory().to_owned();
    let history = history.cloned();
    worker.set_handle(
        thread::Builder::new()
            .name("terraleph-environment".to_owned())
            .spawn(move || {
                let mut diagnostics = Vec::new();
                let result = acquire(
                    tool,
                    &root,
                    &launch_root,
                    &arguments,
                    &plan_path,
                    &cancellation,
                    &mut diagnostics,
                )
                .map_or_else(PlanResult::Error, |result| match result {
                    PlanResult::Ready { review, changed } => PlanResult::Ready {
                        review: Box::new(super::with_previous_durations(*review, history.as_ref())),
                        changed,
                    },
                    result => result,
                });
                let _ = sender.send(Completion {
                    index,
                    result,
                    diagnostics,
                });
            })?,
    );
    Ok(())
}

fn acquire(
    tool: Tool,
    root: &Path,
    launch_root: &Path,
    arguments: &[OsString],
    plan_path: &Path,
    cancellation: &CancellationToken,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<PlanResult, String> {
    let config =
        configuration::read_configuration(root, tool, None).map_err(|error| error.to_string())?;
    if config.execution_location == ExecutionLocation::HcpCandidate {
        return Ok(PlanResult::ExcludedHcp);
    }
    if !config.has_backend {
        return Err("The environment no longer has backend configuration.".to_owned());
    }
    let changed = terraform::run_environment_plan(
        tool,
        root,
        arguments,
        cancellation,
        &terraform::SystemProcessRunner,
        diagnostics,
    )
    .map_err(|error| environment_failure(&error, cancellation))?;
    let planned_at = Instant::now();
    let variables =
        super::invocation::variable_sources(root, arguments).map_err(|error| error.to_string())?;
    let context = ExecutionContext::loading(root)
        .with_tool(tool)
        .with_launch_root(launch_root)
        .with_variable_sources(variables);
    let review = terraform::read_saved_plan_review(
        tool,
        root,
        root,
        &[],
        plan_path,
        changed,
        false,
        context,
        cancellation,
        &terraform::SystemProcessRunner,
        &mut |_| {},
    )
    .map_err(|error| environment_failure(&error, cancellation))?;
    Ok(PlanResult::Ready {
        review: Box::new(review.with_planned_at(planned_at)),
        changed,
    })
}

fn environment_failure(
    error: &terraform::TerraformExecutionError,
    cancellation: &CancellationToken,
) -> String {
    if error.is_interrupted() {
        cancellation.cancel();
    }
    error.to_string()
}

fn cleanup_plans(plans: Vec<Option<terraform::SavedPlan>>) -> io::Result<()> {
    let mut first_error = None;
    for plan in plans.into_iter().flatten() {
        if let Err(error) = plan.cleanup() {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::{
        app::{
            copy::{CopyResult, CopyTarget},
            environments::{EnvironmentAvailability, EnvironmentIdentity},
            plan::Plan,
            review::{PlanMetadata, PlanReview, test_support::plan_document},
            session::Action,
        },
        runtime::event_loop::test_support::terminal_text,
    };

    fn available(name: &str) -> Environment {
        Environment {
            tool: Tool::Terraform,
            availability: EnvironmentAvailability::Available(EnvironmentIdentity {
                directory: PathBuf::from(name),
                workspace: "default".to_owned(),
            }),
        }
    }

    fn ready() -> PlanResult {
        PlanResult::Ready {
            review: Box::new(PlanReview::new(
                PathBuf::from("/test"),
                "default".to_owned(),
                plan_document("No changes.\n".to_owned()),
                Plan::empty(),
                PlanMetadata::new(false),
                Vec::new(),
            )),
            changed: false,
        }
    }

    fn ready_session() -> EnvironmentSession {
        let mut state = EnvironmentSession::new(vec![available("a")], false);
        let index = state.start_next().expect("environment should start");
        assert!(state.complete(index, ready(), Vec::new()));
        state
    }

    fn record_copy(state: &mut EnvironmentSession, now: Instant) {
        let Some(Effect::WriteClipboard(effect)) =
            state.update_review(0, Action::Copy(CopyTarget::Plan), now)
        else {
            panic!("plan copy should produce a clipboard effect");
        };
        assert!(
            state
                .update_review(
                    0,
                    Action::CopyCompleted {
                        target: effect.target(),
                        result: CopyResult::Written,
                    },
                    now,
                )
                .is_none()
        );
    }

    mod receive_apply {
        use super::*;
        use crate::app::{
            copy::CopyEffect,
            execution::{ApplyStatus, ExecutionState},
        };

        const DISCONNECT_MESSAGE: &str = "Apply worker disconnected.";

        struct Fixture {
            sender: mpsc::Sender<PlanReviewMessage>,
            receiver: mpsc::Receiver<PlanReviewMessage>,
            cancellation: CancellationToken,
            worker: WorkerGuard,
            session: SessionState,
            view: ExecutionViewState,
        }

        impl Fixture {
            fn new(worker: thread::JoinHandle<()>) -> Self {
                while !worker.is_finished() {
                    thread::yield_now();
                }
                let cancellation = CancellationToken::new();
                let (sender, receiver) = mpsc::channel();
                Self {
                    sender,
                    receiver,
                    worker: WorkerGuard {
                        cancellation: cancellation.clone(),
                        handle: Some(worker),
                    },
                    cancellation,
                    session: SessionState::Apply(Box::new(ExecutionState::applying(
                        Instant::now(),
                        ExecutionContext::loading("/project"),
                    ))),
                    view: ExecutionViewState::default(),
                }
            }

            fn queue_apply_completed(&self) {
                self.sender
                    .send(PlanReviewMessage::ApplyCompleted {
                        status: ApplyStatus::Succeeded,
                        summary_line: None,
                    })
                    .expect("the receiver should be alive");
            }

            fn receive(&mut self) -> io::Result<(Option<SessionOutcome>, bool)> {
                let mut clipboard = NoClipboard;
                let mut effects = RuntimeEffects {
                    tool: Tool::Terraform,
                    root: Path::new("/project"),
                    display_root: Path::new("/project"),
                    global_arguments: &[],
                    apply_arguments: &[],
                    sender: &self.sender,
                    plan_path: None,
                    cancellation: &self.cancellation,
                    clipboard: &mut clipboard,
                    apply_worker: &mut self.worker,
                    history: None,
                };
                receive_apply(
                    &self.receiver,
                    Some(&mut self.session),
                    &mut self.view,
                    &mut effects,
                )
            }

            fn reports_disconnect(&self) -> bool {
                self.session.apply().is_some_and(|apply| {
                    apply
                        .progress()
                        .diagnostics()
                        .iter()
                        .any(|diagnostic| diagnostic.summary == DISCONNECT_MESSAGE)
                })
            }
        }

        struct NoClipboard;

        impl ClipboardWriter for NoClipboard {
            fn execute(&mut self, _effect: &CopyEffect) -> CopyResult {
                CopyResult::Written
            }
        }

        #[test]
        fn worker_panic_fails_before_a_queued_message_is_read() {
            let mut fixture = Fixture::new(thread::spawn(|| panic!("apply panic")));
            fixture.queue_apply_completed();

            let error = fixture
                .receive()
                .expect_err("a panicked apply worker should fail the runtime");

            assert_eq!(error.to_string(), "apply worker panicked");
            assert!(fixture.receiver.try_recv().is_ok());
            assert!(
                fixture
                    .session
                    .apply()
                    .is_some_and(|apply| apply.result().is_none())
            );
        }

        #[test]
        fn queued_final_message_is_processed_before_a_finished_worker_is_reported() {
            let mut fixture = Fixture::new(thread::spawn(|| {}));
            fixture.queue_apply_completed();

            let (outcome, redraw) = fixture.receive().expect("the worker exited normally");

            assert!(outcome.is_none());
            assert!(redraw);
            assert!(!fixture.reports_disconnect());
            assert!(
                fixture
                    .session
                    .apply()
                    .is_some_and(|apply| apply.result().is_some())
            );
        }

        #[test]
        fn finished_worker_without_a_final_message_is_reported_as_disconnected() {
            let mut fixture = Fixture::new(thread::spawn(|| {}));

            let (outcome, redraw) = fixture.receive().expect("the worker exited normally");

            assert!(outcome.is_none());
            assert!(redraw);
            assert!(fixture.reports_disconnect());
        }
    }

    #[test]
    fn open_confirmation_redraws_once_when_the_plan_age_changes() {
        use crate::app::plan::{ResourceChangeKind, test_support::resource_change};

        let planned_at = Instant::now();
        let mut state = EnvironmentSession::new(vec![available("a")], false);
        let index = state.start_next().expect("environment should start");
        let review = PlanReview::new(
            PathBuf::from("/test"),
            "default".to_owned(),
            plan_document("Plan: 1 to add, 0 to change, 0 to destroy.\n".to_owned()),
            Plan {
                resource_changes: vec![resource_change(
                    "terraform_data.api",
                    ResourceChangeKind::Create,
                )],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        )
        .with_planned_at(planned_at);
        assert!(state.complete(
            index,
            PlanResult::Ready {
                review: Box::new(review),
                changed: true,
            },
            Vec::new(),
        ));
        state.update_review(index, Action::OpenApplyConfirmation, planned_at);
        let mut views = SessionViews::default();
        let mut view = EnvironmentView::default();
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("test terminal");
        let mut dirty = true;
        let mut draw = |state: &mut EnvironmentSession, dirty: &mut bool, elapsed| {
            draw_apply_if_needed(
                &mut terminal,
                state,
                &mut view,
                index,
                &mut views,
                dirty,
                planned_at + Duration::from_secs(elapsed),
            )
            .expect("confirmation should render")
        };

        assert!(draw(&mut state, &mut dirty, 30));
        assert!(!draw(&mut state, &mut dirty, 59));
        assert!(draw(&mut state, &mut dirty, 60));
        assert!(!draw(&mut state, &mut dirty, 61));
        assert_eq!(
            views.scheduled_draw,
            Some(planned_at + Duration::from_secs(120))
        );
        assert!(terminal_text(&terminal).contains("Planned: 1m ago"));
    }

    #[test]
    fn cleanup_removes_owned_plans_and_keeps_user_output() {
        let directory = tempfile::tempdir().expect("user output directory should be created");
        let user_output = directory.path().join("review.tfplan");
        fs::write(&user_output, "").expect("user output should be written");
        let (owned, _) = terraform::saved_plan_for_plan(directory.path(), &[])
            .expect("owned plan should be created");
        let owned_path = owned.path().to_owned();
        let (user, _) = terraform::saved_plan_for_plan(
            directory.path(),
            &[OsString::from("-out=review.tfplan")],
        )
        .expect("user output should be accepted");

        cleanup_plans(vec![Some(owned), None, Some(user)]).expect("cleanup should succeed");

        assert!(!owned_path.exists());
        assert!(user_output.exists());
    }

    #[test]
    fn completed_environments_skip_idle_draws_and_redraw_for_events_and_copy_expiration() {
        let mut state = ready_session();
        let mut view = EnvironmentView::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        let mut dirty = true;

        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("initial draw should succeed")
        );
        assert!(
            !draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("empty poll should succeed")
        );

        for event in [
            Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            Event::Resize(120, 40),
        ] {
            assert!(event_requires_draw(&event));
            dirty = true;
            assert!(
                draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                    .expect("input and resize should draw")
            );
        }
        assert!(!event_requires_draw(&Event::Key(KeyEvent::new_with_kind(
            KeyCode::Down,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        ))));
        assert!(!event_requires_draw(&Event::FocusGained));

        let copied_at = Instant::now();
        let copy_event = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(event_requires_draw(&copy_event));
        dirty = true;
        record_copy(&mut state, copied_at);
        assert!(should_draw(&state, dirty));
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("copy notice should draw")
        );
        assert!(
            !draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("pending copy notice should not draw on an empty poll")
        );

        dirty |= state.clear_expired_copy_feedback(copied_at + Duration::from_secs(3));
        assert!(dirty);
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("expired copy notice should draw once")
        );
        assert!(
            !draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("idle poll after expiration should succeed")
        );
    }

    #[test]
    fn acquisition_draws_during_poll_and_retry_and_completion_request_a_draw() {
        let mut state = EnvironmentSession::new(vec![available("a")], false);
        let mut view = EnvironmentView::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        let mut dirty = true;

        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("initial draw should succeed")
        );
        let index = state.start_next().expect("environment should start");
        dirty = true;
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("worker start should draw")
        );
        assert!(should_draw(&state, false));
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("acquisition poll should draw")
        );

        assert!(state.complete(index, PlanResult::Error("failed".to_owned()), Vec::new()));
        dirty = true;
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("worker result should draw")
        );
        assert!(
            !draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("completed poll should be idle")
        );

        assert!(state.retry(index));
        dirty = true;
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("retry should draw")
        );
        assert!(should_draw(&state, false));

        let retry_index = state.start_next().expect("retry should start");
        assert_eq!(retry_index, index);
        dirty = true;
        assert!(state.complete(retry_index, ready(), Vec::new()));
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("retry result should draw")
        );
        assert!(
            !draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("idle poll after retry should not draw")
        );
    }
}
