use std::{
    ffi::OsString,
    io,
    path::Path,
    process::ExitCode,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use ratatui::{Terminal, backend::Backend};

use crate::{
    app::{
        environments::{Environment, EnvironmentSession, PlanResult},
        execution::{Diagnostic, ExecutionContext, Tool},
        review::PlanReviewMessage,
        session::{Action, Effect, SessionOutcome, SessionState},
    },
    infra::{
        CancellationToken, ClipboardExecutor,
        history::HistoryStore,
        terraform::{
            self,
            configuration::{self, ExecutionLocation},
        },
    },
    ui::{
        QuitConfirmationInput,
        features::{
            environments::{EnvironmentInput, EnvironmentView},
            execution, plan_review,
        },
        quit_confirmation_key_to_input,
    },
};

use super::{WorkerGuard, event_loop, invocation::Invocation};

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
    let result = ratatui::run(|terminal| -> io::Result<()> {
        loop {
            if let Some(active) = apply.as_mut() {
                match apply_runtime.step(
                    terminal,
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
            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            let input_event = event::read()?;
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
                            let result = clipboard.execute(&effect);
                            state.update_review(
                                index,
                                Action::CopyCompleted {
                                    target: effect.target(),
                                    result,
                                },
                                std::time::Instant::now(),
                            );
                        }
                        if state.plans()[index]
                            .session()
                            .and_then(SessionState::apply_confirmation)
                            .is_some()
                        {
                            apply = Some(EnvironmentApply::new(index));
                        }
                    }
                }
            }
        }
        Ok(())
    });
    cancellation.cancel();
    if result.is_err() {
        apply_runtime.cancellation.cancel();
    }
    let joined = worker.join();
    let apply_joined = apply_runtime.worker.join();
    let cleanup = cleanup_plans(plans);
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
    execution_view: execution::ExecutionViewState,
    review_view: plan_review::PlanReviewViewState,
    confirmation_view: plan_review::ApplyConfirmationViewState,
    quit_confirmation: bool,
}

impl EnvironmentApply {
    fn new(index: usize) -> Self {
        Self {
            index,
            execution_view: execution::ExecutionViewState::default(),
            review_view: plan_review::PlanReviewViewState::default(),
            confirmation_view: plan_review::ApplyConfirmationViewState::default(),
            quit_confirmation: false,
        }
    }

    fn running(&self, state: &EnvironmentSession) -> bool {
        state.plans()[self.index]
            .session()
            .and_then(SessionState::apply)
            .is_some_and(|apply| apply.result().is_none())
    }

    fn handle_key<B: Backend<Error = io::Error>>(
        &mut self,
        terminal: &Terminal<B>,
        state: &EnvironmentSession,
        key: KeyEvent,
    ) -> io::Result<Option<Action>> {
        let Some(session) = state.plans()[self.index].session() else {
            return Ok(None);
        };
        let key = if self.quit_confirmation {
            self.quit_confirmation = false;
            match quit_confirmation_key_to_input(key) {
                QuitConfirmationInput::Confirm => return Ok(Some(Action::Quit)),
                QuitConfirmationInput::Cancel => return Ok(None),
                QuitConfirmationInput::Consume => {
                    self.quit_confirmation = true;
                    return Ok(None);
                }
                QuitConfirmationInput::Forward(key) => key,
            }
        } else {
            key
        };
        let action = event_loop::handle_key_event(
            terminal,
            session,
            &mut self.execution_view,
            &mut self.review_view,
            &mut self.confirmation_view,
            key,
        )?;
        if matches!(action, Some(Action::Quit)) {
            self.quit_confirmation = true;
            return Ok(None);
        }
        Ok(action)
    }
}

fn draw_apply<B: Backend<Error = io::Error>>(
    terminal: &mut Terminal<B>,
    state: &EnvironmentSession,
    view: &mut EnvironmentView,
    apply: &EnvironmentApply,
) -> io::Result<()> {
    let Some(session) = state.plans()[apply.index].session() else {
        return Ok(());
    };
    if let Some(confirmation) = session.apply_confirmation() {
        terminal.draw(|frame| {
            view.render_apply_confirmation(
                frame,
                state,
                apply.index,
                confirmation,
                &apply.confirmation_view,
            );
        })?;
    } else if let Some(execution) = session.apply() {
        terminal.draw(|frame| {
            execution::render_execution_with_quit_confirmation(
                frame,
                execution,
                apply.execution_view,
                Instant::now(),
                apply.quit_confirmation,
            );
        })?;
    }
    Ok(())
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
        reason = "the apply step draws the environment view and routes its effects"
    )]
    fn step<B: Backend<Error = io::Error>>(
        &mut self,
        terminal: &mut Terminal<B>,
        state: &mut EnvironmentSession,
        view: &mut EnvironmentView,
        apply: &mut EnvironmentApply,
        plans: &[Option<terraform::SavedPlan>],
        clipboard: &mut ClipboardExecutor,
        dirty: &mut bool,
    ) -> io::Result<ApplyStep> {
        let (finished, messages) = self.receive(state, apply, plans, clipboard)?;
        *dirty |= messages;
        if let Some(outcome) = finished {
            return Ok(ApplyStep::Finished(outcome));
        }
        *dirty |= state.clear_expired_copy_feedback(Instant::now());
        if *dirty || apply.running(state) {
            draw_apply(terminal, state, view, apply)?;
            *dirty = false;
        }
        if !event::poll(Duration::from_millis(50))? {
            return Ok(ApplyStep::Continue);
        }
        let input_event = event::read()?;
        if !event_requires_draw(&input_event) {
            return Ok(ApplyStep::Continue);
        }
        *dirty = true;
        if let Event::Key(key) = input_event
            && let Some(action) = apply.handle_key(terminal, state, key)?
            && let Some(outcome) = self.dispatch(state, apply, action, plans, clipboard)
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

    fn receive(
        &mut self,
        state: &mut EnvironmentSession,
        apply: &mut EnvironmentApply,
        plans: &[Option<terraform::SavedPlan>],
        clipboard: &mut ClipboardExecutor,
    ) -> io::Result<(Option<SessionOutcome>, bool)> {
        let finished = self.worker.poll_finished();
        if finished.as_ref().is_some_and(Result::is_err) {
            return Err(super::worker_panic_error(super::WorkerKind::Apply));
        }
        let mut received = false;
        while let Ok(message) = self.receiver.try_recv() {
            received = true;
            if let Some(outcome) = self.dispatch(
                state,
                apply,
                SessionState::from_message(message),
                plans,
                clipboard,
            ) {
                return Ok((Some(outcome), received));
            }
        }
        if finished.is_some() && apply.running(state) {
            received = true;
            let outcome = self.dispatch(state, apply, Action::WorkerDisconnected, plans, clipboard);
            return Ok((outcome, received));
        }
        Ok((None, received))
    }

    fn dispatch(
        &mut self,
        state: &mut EnvironmentSession,
        apply: &mut EnvironmentApply,
        action: Action,
        plans: &[Option<terraform::SavedPlan>],
        clipboard: &mut ClipboardExecutor,
    ) -> Option<SessionOutcome> {
        let mut next = Some(action);
        while let Some(action) = next.take() {
            let session = state.session_mut(apply.index)?;
            match event_loop::update_session(
                session,
                action,
                &mut apply.execution_view,
                Instant::now(),
            ) {
                None => {}
                Some(Effect::CancelExecution) => self.cancellation.cancel(),
                Some(Effect::StartApply) => {
                    next = self
                        .start(state, apply.index, plans)
                        .err()
                        .map(|message| Action::ApplyFailed { message });
                }
                Some(Effect::PersistHistory(successes)) => {
                    if let Some(history) = &self.history
                        && let Err(error) = history.record(&successes)
                    {
                        super::report_error(&format!("failed to save apply history: {error}"));
                    }
                }
                Some(Effect::WriteClipboard(effect)) => {
                    let result = clipboard.execute(&effect);
                    next = Some(Action::CopyCompleted {
                        target: effect.target(),
                        result,
                    });
                }
                Some(Effect::Finish(outcome)) => return Some(outcome),
            }
        }
        None
    }

    // Apply runs only the saved plan acquired and reviewed for this environment, in its own
    // directory; it never re-plans and never touches another environment's plan.
    fn start(
        &mut self,
        state: &EnvironmentSession,
        index: usize,
        plans: &[Option<terraform::SavedPlan>],
    ) -> Result<(), String> {
        let plan = &state.plans()[index];
        let execution = plan
            .session()
            .and_then(SessionState::apply)
            .ok_or_else(|| "The environment is not ready to apply.".to_owned())?;
        let root = plan.directory();
        event_loop::verify_apply_target(execution, plan.tool, root, root, &[], &self.cancellation)?;
        let plan_path = plans
            .get(index)
            .and_then(Option::as_ref)
            .map(|saved| saved.path().to_owned())
            .ok_or_else(|| "The reviewed plan is no longer available.".to_owned())?;
        let handle = super::spawn_apply_worker(
            plan.tool,
            root,
            &[],
            &self.apply_arguments,
            &plan_path,
            &self.cancellation,
            &self.sender,
        )
        .map_err(|error| format!("failed to start the apply worker: {error}"))?;
        self.worker.set_handle(handle);
        Ok(())
    }
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
        &mut |_| {},
    )
    .map_err(|error| environment_failure(&error, cancellation))?;
    Ok(PlanResult::Ready {
        review: Box::new(review),
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
    use std::path::PathBuf;

    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::app::{
        copy::{CopyResult, CopyTarget},
        environments::{EnvironmentAvailability, EnvironmentIdentity},
        plan::Plan,
        review::{PlanMetadata, PlanReview, test_support::plan_document},
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

    #[test]
    fn completed_environments_skip_idle_draws_and_redraw_for_events_and_copy_expiration() {
        let mut state = ready_session();
        let mut view = EnvironmentView::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        let mut dirty = true;
        let mut draws = 0;

        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("initial draw should succeed")
        );
        draws += 1;
        assert!(
            !draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("empty poll should succeed")
        );
        assert_eq!(draws, 1);

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
            draws += 1;
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
        draws += 1;
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
        draws += 1;
        assert!(
            !draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("idle poll after expiration should succeed")
        );
        assert_eq!(draws, 5);
    }

    #[test]
    fn acquisition_draws_during_poll_and_retry_and_completion_request_a_draw() {
        let mut state = EnvironmentSession::new(vec![available("a")], false);
        let mut view = EnvironmentView::default();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
        let mut dirty = true;
        let mut draws = 0;

        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("initial draw should succeed")
        );
        draws += 1;
        let index = state.start_next().expect("environment should start");
        dirty = true;
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("worker start should draw")
        );
        draws += 1;
        assert!(should_draw(&state, false));
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("acquisition poll should draw")
        );
        draws += 1;

        assert!(state.complete(index, PlanResult::Error("failed".to_owned()), Vec::new()));
        dirty = true;
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("worker result should draw")
        );
        draws += 1;
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
        draws += 1;
        assert!(should_draw(&state, false));

        let retry_index = state.start_next().expect("retry should start");
        assert_eq!(retry_index, index);
        dirty = true;
        assert!(state.complete(retry_index, ready(), Vec::new()));
        assert!(
            draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("retry result should draw")
        );
        draws += 1;
        assert!(
            !draw_if_needed(&state, &mut view, &mut terminal, &mut dirty)
                .expect("idle poll after retry should not draw")
        );
        assert_eq!(draws, 6);
    }
}
