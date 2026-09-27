use std::{
    fs, io,
    path::Path,
    sync::mpsc::Receiver,
    time::{Duration, Instant},
};

use crossterm::event::{self, Event, KeyCode, KeyEvent};
use ratatui::{DefaultTerminal, Terminal, backend::Backend, layout::Rect};

use crate::{
    app::{
        copy::{CopyEffect, CopyFeedback, CopyResult, CopyTarget},
        execution::{
            ExecutionContextValue, ExecutionStage, ExecutionState, ExecutionTargetState, Tool,
        },
        review::PlanReviewMessage,
        session::{
            self, Action, Effect, ReviewScreen, ReviewSessionState, SessionOutcome, SessionState,
        },
    },
    infra::{CancellationToken, ClipboardExecutor, terraform},
    ui::{
        QuitConfirmationInput,
        features::{execution, overview, plan_review},
        quit_confirmation_key_to_input,
    },
};

#[expect(
    clippy::too_many_lines,
    reason = "the event loop keeps the connected session lifecycle together"
)]
pub(crate) fn run_connected(
    terminal: &mut DefaultTerminal,
    execution: ExecutionState,
    messages: &Receiver<PlanReviewMessage>,
    plan_worker: &mut super::WorkerGuard,
    mut effects: RuntimeEffects<'_, ClipboardExecutor>,
    initial_overview: bool,
) -> io::Result<SessionOutcome> {
    let mut execution_view = execution::ExecutionViewState::default();
    let mut review_view = plan_review::PlanReviewViewState::default();
    let mut confirmation_view = plan_review::ApplyConfirmationViewState::default();
    let mut state = SessionState::new(execution);
    let mut start_in_overview = initial_overview;
    let mut quit_confirmation = false;
    let mut dirty = true;

    loop {
        let awaiting_initial_overview = start_in_overview;
        let (outcome, received) = receive_messages_with_initial_overview(
            messages,
            &mut state,
            &mut execution_view,
            &mut effects,
            &mut start_in_overview,
        );
        dirty |= received;
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }

        let finished_workers = reap_workers(plan_worker, &mut effects)?;
        let (outcome, received) = receive_messages_with_initial_overview(
            messages,
            &mut state,
            &mut execution_view,
            &mut effects,
            &mut start_in_overview,
        );
        dirty |= received;
        if let Some(outcome) = outcome {
            return Ok(outcome);
        }
        if let Some(outcome) = dispatch_finished_workers(
            &mut state,
            &mut execution_view,
            finished_workers,
            &mut effects,
        ) {
            return Ok(outcome);
        }
        // The initial Overview opens from a worker message, not a key, so no key path has
        // aligned its selection and scroll with the terminal before the first draw.
        if awaiting_initial_overview
            && !start_in_overview
            && let Some(overview_state) = state.overview()
        {
            let size = terminal.size()?;
            overview::reconcile_view(
                Rect::new(0, 0, size.width, size.height),
                overview_state,
                review_view.overview_mut(),
            );
        }

        let now = Instant::now();
        draw_if_needed_with_quit_confirmation(
            &mut state,
            terminal,
            execution_view,
            &review_view,
            &confirmation_view,
            &mut dirty,
            now,
            quit_confirmation,
        )?;

        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Resize(width, height) => {
                    dirty = true;
                    reconcile_resize(
                        &state,
                        &mut review_view,
                        Rect::new(0, 0, width, height),
                        quit_confirmation,
                    );
                }
                Event::Key(key) if key.is_press() => {
                    dirty = true;
                    let mut confirmed_quit = false;
                    let action = if quit_confirmation {
                        match quit_confirmation_key_to_input(key) {
                            QuitConfirmationInput::Confirm => {
                                quit_confirmation = false;
                                confirmed_quit = true;
                                Some(Action::Quit)
                            }
                            QuitConfirmationInput::Cancel => {
                                quit_confirmation = false;
                                None
                            }
                            QuitConfirmationInput::Consume => None,
                            QuitConfirmationInput::Forward(key) => {
                                quit_confirmation = false;
                                handle_key_event(
                                    terminal,
                                    &state,
                                    &mut execution_view,
                                    &mut review_view,
                                    &mut confirmation_view,
                                    key,
                                )?
                            }
                        }
                    } else {
                        handle_key_event(
                            terminal,
                            &state,
                            &mut execution_view,
                            &mut review_view,
                            &mut confirmation_view,
                            key,
                        )?
                    };
                    if let Some(action) = action {
                        if let Some(action) = action_after_quit_confirmation(
                            action,
                            confirmed_quit,
                            &mut quit_confirmation,
                        ) && let Some(outcome) =
                            dispatch(&mut state, action, &mut execution_view, &mut effects)
                        {
                            return Ok(outcome);
                        }
                        if state
                            .apply()
                            .is_some_and(|apply| apply.stage() == ExecutionStage::Applying)
                        {
                            draw_if_needed_with_quit_confirmation(
                                &mut state,
                                terminal,
                                execution_view,
                                &review_view,
                                &confirmation_view,
                                &mut dirty,
                                Instant::now(),
                                quit_confirmation,
                            )?;
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn action_after_quit_confirmation(
    action: Action,
    confirmed_quit: bool,
    quit_confirmation: &mut bool,
) -> Option<Action> {
    if matches!(&action, Action::Quit) && !confirmed_quit {
        *quit_confirmation = true;
        None
    } else {
        Some(action)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FinishedWorkers {
    plan: bool,
    apply: bool,
}

fn reap_workers(
    plan_worker: &mut super::WorkerGuard,
    effects: &mut RuntimeEffects<'_, impl ClipboardWriter>,
) -> io::Result<FinishedWorkers> {
    let plan_join = plan_worker.poll_finished();
    let apply_join = effects.apply_worker.poll_finished();
    if apply_join.as_ref().is_some_and(Result::is_err) {
        return Err(super::worker_panic_error(super::WorkerKind::Apply));
    }
    if plan_join.as_ref().is_some_and(Result::is_err) {
        return Err(super::worker_panic_error(super::WorkerKind::Plan));
    }
    Ok(FinishedWorkers {
        plan: plan_join.is_some(),
        apply: apply_join.is_some(),
    })
}

fn dispatch_finished_workers<C: ClipboardWriter>(
    state: &mut SessionState,
    execution_view: &mut execution::ExecutionViewState,
    finished_workers: FinishedWorkers,
    effects: &mut RuntimeEffects<'_, C>,
) -> Option<SessionOutcome> {
    if finished_workers.plan
        && state
            .execution()
            .is_some_and(|execution| execution.result().is_none())
    {
        return dispatch(state, Action::WorkerDisconnected, execution_view, effects);
    }
    if finished_workers.apply && state.apply().is_some_and(|apply| apply.result().is_none()) {
        return dispatch(state, Action::WorkerDisconnected, execution_view, effects);
    }
    None
}

fn should_draw(state: &SessionState, dirty: bool) -> bool {
    dirty
        || state
            .execution()
            .is_some_and(|execution| execution.result().is_none())
        || state.apply().is_some_and(|apply| apply.result().is_none())
        || state.copy_feedback().is_some_and(CopyFeedback::pending)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the draw step receives the runtime-owned views and rendering state"
)]
pub(super) fn draw_if_needed_with_quit_confirmation<B: Backend>(
    state: &mut SessionState,
    terminal: &mut Terminal<B>,
    execution_view: execution::ExecutionViewState,
    review_view: &plan_review::PlanReviewViewState,
    confirmation_view: &plan_review::ApplyConfirmationViewState,
    dirty: &mut bool,
    now: Instant,
    quit_confirmation: bool,
) -> Result<bool, B::Error> {
    // Layouts reserve footer width for any stored notice, so an expired notice is cleared
    // before the frame that would otherwise draw it as blank space.
    *dirty |= clear_expired_copy_feedback(state, now);
    if !should_draw(state, *dirty) {
        return Ok(false);
    }

    draw_with_quit_confirmation(
        state,
        terminal,
        execution_view,
        review_view,
        confirmation_view,
        now,
        quit_confirmation,
    )?;
    *dirty = false;
    Ok(true)
}

fn clear_expired_copy_feedback(state: &mut SessionState, now: Instant) -> bool {
    state
        .copy_feedback_mut()
        .is_some_and(|feedback| feedback.clear_expired(now))
}

// The apply confirmation body and the dialogs clamp against the current layout on their next
// input or render, so only the review and Overview offsets need a resize correction.
pub(super) fn reconcile_resize(
    state: &SessionState,
    review_view: &mut plan_review::PlanReviewViewState,
    area: Rect,
    quit_confirmation: bool,
) {
    if let Some(review) = state.review() {
        let layout = plan_review::layout_with_quit_confirmation(
            area,
            review_view.searching(),
            review,
            quit_confirmation,
        );
        review_view.reconcile(
            layout.body(),
            layout.max_vertical(),
            layout.max_horizontal(),
            layout.matches(),
        );
    }
    if let Some(overview_state) = state.overview() {
        overview::reconcile_view(area, overview_state, review_view.overview_mut());
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the key dispatcher keeps the existing review and apply paths together"
)]
pub(super) fn handle_key_event<B: Backend>(
    terminal: &Terminal<B>,
    state: &SessionState,
    execution_view: &mut execution::ExecutionViewState,
    review_view: &mut plan_review::PlanReviewViewState,
    confirmation_view: &mut plan_review::ApplyConfirmationViewState,
    key: KeyEvent,
) -> Result<Option<Action>, B::Error> {
    if let Some(execution) = state.execution() {
        return handle_execution_key_event(terminal, execution, execution_view, key);
    }

    if state.apply_confirmation().is_some() {
        if confirmation_view.overlay().is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Char('?') => confirmation_view.close_overlay(),
                KeyCode::Up | KeyCode::Char('k') => confirmation_view.scroll_overlay(-1),
                KeyCode::Down | KeyCode::Char('j') => confirmation_view.scroll_overlay(1),
                KeyCode::PageUp => confirmation_view.scroll_overlay(-8),
                KeyCode::PageDown => confirmation_view.scroll_overlay(8),
                KeyCode::Home => confirmation_view.overlay_top(),
                KeyCode::End => confirmation_view.overlay_bottom(),
                _ => {}
            }
            return Ok(None);
        }
        let confirmation = state
            .apply_confirmation()
            .expect("confirmation state should still be available");
        let size = terminal.size()?;
        let layout = plan_review::apply_confirmation_layout(
            Rect::new(0, 0, size.width, size.height),
            confirmation,
        );
        let input = plan_review::apply_confirmation_key_to_input(key);
        let input = match input {
            Some(plan_review::ApplyConfirmationInput::Cancel) => input,
            Some(_) if layout.renderable() => input,
            _ => None,
        };
        let expected = confirmation.review().confirmation_input();
        return Ok(input
            .and_then(|input| confirmation_view.apply(input, &expected, layout.max_vertical())));
    }

    if let Some(apply) = state.apply() {
        return handle_execution_key_event(terminal, apply, execution_view, key);
    }

    if let Some(overview_state) = state.overview() {
        return handle_overview_key_event(terminal, overview_state, review_view, key);
    }

    let Some(review) = state.review() else {
        return Ok(None);
    };
    if review_view.overlay().is_some() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => review_view.close_overlay(),
            KeyCode::Up | KeyCode::Char('k') => review_view.scroll_overlay(-1),
            KeyCode::Down | KeyCode::Char('j') => review_view.scroll_overlay(1),
            KeyCode::PageUp => review_view.scroll_overlay(-8),
            KeyCode::PageDown => review_view.scroll_overlay(8),
            KeyCode::Home => review_view.overlay_top(),
            KeyCode::End => review_view.overlay_bottom(),
            _ => {}
        }
        return Ok(None);
    }
    Ok(
        match plan_review::key_to_input(
            key,
            review_view.searching(),
            !review.review().search_query().is_empty(),
        ) {
            Some(plan_review::PlanReviewInput::Quit) => Some(Action::Quit),
            Some(plan_review::PlanReviewInput::Apply) => Some(Action::OpenApplyConfirmation),
            Some(plan_review::PlanReviewInput::Copy) => Some(Action::Copy(CopyTarget::Plan)),
            Some(plan_review::PlanReviewInput::OpenOverview) => {
                let content = overview::OverviewContent::project(
                    review,
                    review_view.overview().filter(),
                    review_view.overview().expanded(),
                );
                review_view
                    .overview_mut()
                    .reconcile(u16::MAX, content.rows.len());
                Some(Action::OpenOverview)
            }
            Some(plan_review::PlanReviewInput::SearchCancel)
                if !review_view.searching() && review.is_from_overview() =>
            {
                Some(Action::ReturnToOverview)
            }
            Some(input) => {
                let size = terminal.size()?;
                let body = plan_review::layout(
                    Rect::new(0, 0, size.width, size.height),
                    review_view.searching(),
                    review,
                );
                review_view
                    .apply_with_matches(
                        input,
                        body.body(),
                        body.max_vertical(),
                        body.max_horizontal(),
                        review.review().search_query(),
                        body.matches(),
                    )
                    .map(Action::ReviewSearchChanged)
            }
            None => None,
        },
    )
}

fn handle_overview_key_event<B: Backend>(
    terminal: &Terminal<B>,
    overview_state: &ReviewSessionState,
    review_view: &mut plan_review::PlanReviewViewState,
    key: KeyEvent,
) -> Result<Option<Action>, B::Error> {
    if review_view.overview().overlay().is_some() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') => review_view.overview_mut().close_overlay(),
            KeyCode::Up | KeyCode::Char('k') => review_view.overview_mut().scroll_overlay(-1),
            KeyCode::Down | KeyCode::Char('j') => review_view.overview_mut().scroll_overlay(1),
            KeyCode::PageUp => review_view.overview_mut().scroll_overlay(-8),
            KeyCode::PageDown => review_view.overview_mut().scroll_overlay(8),
            KeyCode::Home => review_view.overview_mut().overlay_top(),
            KeyCode::End => review_view.overview_mut().overlay_bottom(),
            _ => {}
        }
        return Ok(None);
    }
    let size = terminal.size()?;
    let content = overview::OverviewContent::project(
        overview_state,
        review_view.overview().filter(),
        review_view.overview().expanded(),
    );
    let layout = overview::layout(
        Rect::new(0, 0, size.width, size.height),
        overview_state,
        review_view.overview(),
        &content,
    );
    let input = overview::key_to_input(
        key,
        review_view.overview().searching(),
        !review_view.overview().filter().is_empty(),
    );
    let Some(input) = input else {
        return Ok(None);
    };
    let command = review_view.overview_mut().apply(
        input,
        layout.changes_body(),
        layout.relations(),
        layout.max_vertical(),
        &content,
    );
    if let Some(overview::OverviewCommand::Open(address)) = command.as_ref() {
        jump_to_overview_address(terminal, review_view, overview_state, address.as_deref())?;
    }
    Ok(command.map(|command| match command {
        overview::OverviewCommand::Open(address) => Action::OpenReviewFromOverview { address },
        overview::OverviewCommand::ViewPlan | overview::OverviewCommand::Back => {
            review_view.jump_to_line(0, u16::MAX);
            Action::OpenReviewFromOverview { address: None }
        }
        overview::OverviewCommand::Copy => Action::Copy(CopyTarget::Plan),
        overview::OverviewCommand::Quit => Action::Quit,
    }))
}

fn jump_to_overview_address<B: Backend>(
    terminal: &Terminal<B>,
    review_view: &mut plan_review::PlanReviewViewState,
    overview: &ReviewSessionState,
    address: Option<&str>,
) -> Result<(), B::Error> {
    let line = address
        .and_then(|address| overview.review().document().block_for_address(address))
        .map_or(0, |block| block.lines().start);
    let size = terminal.size()?;
    let layout = plan_review::overview_detail_layout(
        Rect::new(0, 0, size.width, size.height),
        overview.review(),
    );
    review_view.jump_to_line(line, layout.max_vertical());
    Ok(())
}

fn handle_execution_key_event<B: Backend>(
    terminal: &Terminal<B>,
    state: &ExecutionState,
    execution_view: &mut execution::ExecutionViewState,
    key: KeyEvent,
) -> Result<Option<Action>, B::Error> {
    Ok(
        match execution::execution_key_to_input(key, state.stage(), execution_view.logs_open()) {
            Some(execution::ExecutionInput::Quit) => Some(Action::Quit),
            Some(execution::ExecutionInput::Action(action)) => Some(Action::Execution(action)),
            Some(execution::ExecutionInput::SelectTarget(direction)) => {
                let targets = state
                    .progress()
                    .display_target_indices(state.result().is_some());
                execution_view.select_target(direction, &targets);
                let size = terminal.size()?;
                let layout = execution::execution_layout_with_view(
                    Rect::new(0, 0, size.width, size.height),
                    state,
                    *execution_view,
                );
                if let Some(position) = execution_view
                    .selected_target()
                    .and_then(|selected| targets.iter().position(|index| *index == selected))
                {
                    execution_view.ensure_target_visible(
                        position,
                        layout.target_body().height,
                        layout.target_max_vertical(),
                    );
                }
                None
            }
            Some(execution::ExecutionInput::ToggleFocus) => {
                execution_view.toggle_focus();
                None
            }
            Some(execution::ExecutionInput::OpenLogs) => {
                execution_view.open_logs();
                None
            }
            Some(execution::ExecutionInput::CloseLogs) => {
                execution_view.close_logs();
                None
            }
            Some(execution::ExecutionInput::End) => {
                execution_view.end();
                None
            }
            Some(execution::ExecutionInput::Scroll(scroll)) => {
                let size = terminal.size()?;
                let layout = execution::execution_layout_with_view(
                    Rect::new(0, 0, size.width, size.height),
                    state,
                    *execution_view,
                );
                if state.is_apply() && !execution_view.logs_open() {
                    let (current, max) = execution::execution_target_scroll_position_with_view(
                        *execution_view,
                        &layout,
                    );
                    execution_view.apply_target_scroll(
                        scroll,
                        current,
                        max,
                        layout.target_body().height,
                    );
                    return Ok(None);
                }
                let (current_vertical, _) =
                    execution::execution_scroll_position_with_view(state, *execution_view, &layout);
                match scroll {
                    execution::ExecutionScroll::Left
                    | execution::ExecutionScroll::Right
                    | execution::ExecutionScroll::LeftEdge
                    | execution::ExecutionScroll::RightEdge => {
                        let (current, max) =
                            execution::execution_horizontal_scroll_position_with_view(
                                *execution_view,
                                &layout,
                            );
                        execution_view.apply_horizontal_scroll(
                            scroll,
                            current,
                            max,
                            current_vertical,
                        );
                    }
                    _ => {
                        let (current, max) = execution::execution_scroll_position_with_view(
                            state,
                            *execution_view,
                            &layout,
                        );
                        execution_view.apply_scroll(scroll, current, max, layout.body().height);
                    }
                }
                None
            }
            Some(execution::ExecutionInput::Copy(target)) => Some(Action::Copy(target)),
            None => None,
        },
    )
}

fn draw_with_quit_confirmation<B: Backend>(
    state: &SessionState,
    terminal: &mut Terminal<B>,
    execution_view: execution::ExecutionViewState,
    review_view: &plan_review::PlanReviewViewState,
    confirmation_view: &plan_review::ApplyConfirmationViewState,
    now: Instant,
    quit_confirmation: bool,
) -> Result<(), B::Error> {
    match state {
        SessionState::Execution(execution) => {
            terminal.draw(|frame| {
                execution::render_execution_with_quit_confirmation(
                    frame,
                    execution,
                    execution_view,
                    now,
                    quit_confirmation,
                );
            })?;
        }
        SessionState::Review(review) => match review.screen() {
            ReviewScreen::Raw(_) => {
                terminal.draw(|frame| {
                    plan_review::render_with_quit_confirmation(
                        frame,
                        review,
                        review_view,
                        now,
                        quit_confirmation,
                    );
                })?;
            }
            ReviewScreen::Overview => {
                terminal.draw(|frame| {
                    overview::render_with_quit_confirmation(
                        frame,
                        review,
                        review_view.overview(),
                        now,
                        quit_confirmation,
                    );
                })?;
            }
            ReviewScreen::ApplyConfirmation { .. } => {
                terminal.draw(|frame| {
                    plan_review::render_apply_confirmation(frame, review, confirmation_view);
                })?;
            }
        },
        SessionState::Apply(execution) => {
            terminal.draw(|frame| {
                execution::render_execution_with_quit_confirmation(
                    frame,
                    execution,
                    execution_view,
                    now,
                    quit_confirmation,
                );
            })?;
        }
    }
    Ok(())
}

fn receive_messages_with_initial_overview<C: ClipboardWriter>(
    messages: &Receiver<PlanReviewMessage>,
    state: &mut SessionState,
    execution_view: &mut execution::ExecutionViewState,
    effects: &mut RuntimeEffects<'_, C>,
    start_in_overview: &mut bool,
) -> (Option<SessionOutcome>, bool) {
    // The runtime keeps a sender alive for the apply worker, so the channel never disconnects
    // while this loop runs; a worker that exits without a final message is found by joining it.
    let mut received = false;
    while let Ok(message) = messages.try_recv() {
        received = true;
        let open_overview =
            *start_in_overview && matches!(message, PlanReviewMessage::Completed(_));
        if open_overview {
            *start_in_overview = false;
        }
        if let Some(outcome) = dispatch(
            state,
            SessionState::from_message(message),
            execution_view,
            effects,
        ) {
            return (Some(outcome), received);
        }
        if open_overview
            && state.review().is_some()
            && let Some(outcome) = dispatch(state, Action::OpenOverview, execution_view, effects)
        {
            return (Some(outcome), received);
        }
    }
    (None, received)
}

pub(super) fn update_session(
    state: &mut SessionState,
    action: Action,
    execution_view: &mut execution::ExecutionViewState,
    now: Instant,
) -> Option<Effect> {
    let was_apply = state.apply().is_some();
    let had_apply_result = state.apply().is_some_and(|apply| apply.result().is_some());
    let effect = session::update(state, action, now);
    let entered_apply = !was_apply && state.apply().is_some();
    let apply_result_ready =
        !had_apply_result && state.apply().is_some_and(|apply| apply.result().is_some());
    if entered_apply {
        *execution_view = execution::ExecutionViewState::default();
        if let Some(apply) = state.apply() {
            let targets = apply.progress().display_target_indices(false);
            execution_view.initialize_target_selection(&targets);
        }
    } else if apply_result_ready {
        *execution_view = execution::ExecutionViewState::default();
        if let Some(apply) = state.apply() {
            execution_view.select_result_target(
                &apply.progress().display_target_indices(true),
                apply.progress().first_bound_failed_index(),
                apply
                    .progress()
                    .first_bound_failed_index()
                    .and_then(|index| apply.progress().targets().get(index))
                    .and_then(ExecutionTargetState::first_error_line),
                apply.stage() == ExecutionStage::ApplySucceeded,
            );
        }
    }
    effect
}

fn dispatch<C: ClipboardWriter>(
    state: &mut SessionState,
    action: Action,
    execution_view: &mut execution::ExecutionViewState,
    effects: &mut RuntimeEffects<'_, C>,
) -> Option<SessionOutcome> {
    let effect = update_session(state, action, execution_view, Instant::now());
    apply_effect(state, effect, execution_view, effects)
}

fn apply_effect<C: ClipboardWriter>(
    state: &mut SessionState,
    effect: Option<Effect>,
    execution_view: &mut execution::ExecutionViewState,
    effects: &mut RuntimeEffects<'_, C>,
) -> Option<SessionOutcome> {
    match effect {
        None => None,
        Some(Effect::CancelExecution) => {
            effects.cancellation.cancel();
            None
        }
        Some(Effect::PersistHistory(successes)) => {
            if let Some(history) = effects.history
                && let Err(error) = history.record(&successes)
            {
                super::report_error(&format!("failed to save apply history: {error}"));
            }
            None
        }
        Some(Effect::StartApply) => {
            let apply = state.apply()?;
            if let Err(message) = verify_apply_context(apply, effects) {
                return dispatch(
                    state,
                    Action::ApplyFailed { message },
                    execution_view,
                    effects,
                );
            }
            match super::spawn_apply_worker(
                effects.tool,
                effects.root,
                effects.global_arguments,
                effects.apply_arguments,
                effects.plan_path,
                effects.cancellation,
                effects.sender,
            ) {
                Ok(handle) => effects.apply_worker.set_handle(handle),
                Err(error) => {
                    return dispatch(
                        state,
                        Action::ApplyFailed {
                            message: format!("failed to start the apply worker: {error}"),
                        },
                        execution_view,
                        effects,
                    );
                }
            }
            None
        }
        Some(Effect::WriteClipboard(effect)) => {
            let target = effect.target();
            let result = effects.clipboard.execute(&effect);
            dispatch(
                state,
                Action::CopyCompleted { target, result },
                execution_view,
                effects,
            )
        }
        Some(Effect::Finish(outcome)) => Some(outcome),
    }
}

fn verify_apply_context(
    apply: &ExecutionState,
    effects: &RuntimeEffects<'_, impl ClipboardWriter>,
) -> Result<(), String> {
    verify_apply_target(
        apply,
        effects.tool,
        effects.root,
        effects.display_root,
        effects.global_arguments,
        effects.cancellation,
    )
}

pub(super) fn verify_apply_target(
    apply: &ExecutionState,
    tool: Tool,
    root: &Path,
    display_root: &Path,
    global_arguments: &[std::ffi::OsString],
    cancellation: &CancellationToken,
) -> Result<(), String> {
    verify_apply_directory(apply.context().cwd_path(), display_root, tool)?;
    let workspace = terraform::read_workspace_with_arguments(
        tool,
        root,
        global_arguments,
        cancellation,
        &terraform::SystemProcessRunner,
    )
    .map_err(|error| {
        format!(
            "Could not re-confirm the {} workspace: {error}",
            tool.display_name()
        )
    })?;
    let expected = match apply.context().workspace() {
        ExecutionContextValue::Known(workspace) => workspace,
        ExecutionContextValue::Loading => {
            return Err(format!(
                "The {} workspace is not available for apply.",
                tool.display_name()
            ));
        }
    };
    if workspace != *expected {
        return Err(format!(
            "The {} workspace changed from {expected} to {workspace}. Re-run plan and review it again before applying.",
            tool.display_name()
        ));
    }
    Ok(())
}

fn verify_apply_directory(expected: &Path, current: &Path, tool: Tool) -> Result<(), String> {
    let expected_root = fs::canonicalize(expected).map_err(|error| {
        format!(
            "Could not re-confirm the reviewed {} directory: {error}",
            tool.display_name()
        )
    })?;
    let current_root = fs::canonicalize(current).map_err(|error| {
        format!(
            "Could not resolve the current {} execution directory: {error}",
            tool.display_name()
        )
    })?;
    if current_root != expected_root {
        return Err(
            "The execution directory changed. Re-run plan and review it again before applying."
                .to_owned(),
        );
    }
    Ok(())
}

pub(super) trait ClipboardWriter {
    fn execute(&mut self, effect: &CopyEffect) -> CopyResult;
}

impl ClipboardWriter for ClipboardExecutor {
    fn execute(&mut self, effect: &CopyEffect) -> CopyResult {
        Self::execute(self, effect)
    }
}

pub(super) struct RuntimeEffects<'a, C: ClipboardWriter = ClipboardExecutor> {
    pub(super) tool: Tool,
    pub(super) root: &'a Path,
    pub(super) display_root: &'a Path,
    pub(super) global_arguments: &'a [std::ffi::OsString],
    pub(super) apply_arguments: &'a [std::ffi::OsString],
    pub(super) sender: &'a std::sync::mpsc::Sender<PlanReviewMessage>,
    pub(super) plan_path: &'a Path,
    pub(super) cancellation: &'a CancellationToken,
    pub(super) clipboard: &'a mut C,
    pub(super) apply_worker: &'a mut super::WorkerGuard,
    pub(super) history: Option<&'a super::HistoryStore>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{
        execution::{
            ApplyStatus, EventStream, ExecutionAction, ExecutionContext, ExecutionEvent,
            ExecutionEventKind, ExecutionLogLine, ExecutionTargetSpec, HistoryKey,
            SuccessfulTarget,
        },
        plan::{
            Plan, PlanAction, ResourceChangeKind,
            test_support::{output_change, resource_change},
        },
        review::{PlanMetadata, PlanReview, test_support::plan_document},
    };
    use crate::infra::history::HistoryStore;
    use crate::runtime::{
        WorkerGuard, event_loop::test_support::terminal_text, finalize_ui_result,
    };
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::{backend::TestBackend, buffer::Cell};
    use rstest::rstest;
    use std::{
        path::PathBuf,
        sync::mpsc::{self, Sender},
        thread::{self, JoinHandle},
    };
    use tempfile::TempDir;

    mod effects {
        use super::*;

        #[cfg(unix)]
        #[test]
        fn apply_directory_recheck_rejects_a_retargeted_symlink() {
            use std::os::unix::fs::symlink;

            let root = TempDir::new().expect("test directory should be created");
            let first = root.path().join("first");
            let second = root.path().join("second");
            let link = root.path().join("current");
            fs::create_dir(&first).expect("first directory should be created");
            fs::create_dir(&second).expect("second directory should be created");
            symlink(&first, &link).expect("initial directory link should be created");
            let expected = fs::canonicalize(&link).expect("initial link should resolve");
            fs::remove_file(&link).expect("initial directory link should be removed");
            symlink(&second, &link).expect("retargeted directory link should be created");

            let error = verify_apply_directory(&expected, &link, Tool::Terraform)
                .expect_err("apply should reject a changed symlink target");
            assert!(error.contains("execution directory changed"));
        }

        #[test]
        fn history_write_failure_does_not_produce_a_session_outcome() {
            let fixture = TempDir::new().expect("test directory should be created");
            let root = fixture.path().join("history");
            fs::write(&root, b"not a directory").expect("blocking file should be written");
            let history = HistoryStore::new(root);
            let context = ExecutionContext::loading("/project").with_workspace("default");
            let target = ExecutionTargetSpec {
                address: "terraform_data.api".to_owned(),
                actions: vec![PlanAction::Update],
            };
            let key = HistoryKey::for_target(&context, &target).expect("workspace is known");
            let (sender, _messages) = mpsc::channel();
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut apply_worker = worker_guard(None);
            let mut effects =
                test_effects(&sender, &cancellation, &mut clipboard, &mut apply_worker);
            effects.history = Some(&history);
            let mut state = SessionState::new(ExecutionState::new(Instant::now()));
            let mut execution_view = execution::ExecutionViewState::default();

            let outcome = apply_effect(
                &mut state,
                Some(Effect::PersistHistory(vec![SuccessfulTarget {
                    key,
                    duration: Duration::from_secs(1),
                }])),
                &mut execution_view,
                &mut effects,
            );

            assert!(outcome.is_none());
        }
    }

    mod workers {
        use super::*;

        fn receive_messages<C: ClipboardWriter>(
            messages: &Receiver<PlanReviewMessage>,
            state: &mut SessionState,
            execution_view: &mut execution::ExecutionViewState,
            effects: &mut RuntimeEffects<'_, C>,
        ) -> (Option<SessionOutcome>, bool) {
            let mut start_in_overview = false;
            super::receive_messages_with_initial_overview(
                messages,
                state,
                execution_view,
                effects,
                &mut start_in_overview,
            )
        }

        #[test]
        fn finished_plan_without_final_message_becomes_a_failed_outcome() {
            let (sender, receiver) = mpsc::channel();
            let handle = thread::spawn(|| {});
            wait_for_finished(&handle);
            let mut plan_worker = worker_guard(Some(handle));
            let mut apply_worker = worker_guard(None);
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut effects =
                test_effects(&sender, &cancellation, &mut clipboard, &mut apply_worker);
            let mut state = SessionState::new(ExecutionState::with_context(
                Instant::now(),
                ExecutionContext::loading("/project"),
            ));
            let mut execution_view = execution::ExecutionViewState::default();

            let (_, drained) =
                receive_messages(&receiver, &mut state, &mut execution_view, &mut effects);
            assert!(!drained);
            let finished = reap_workers(&mut plan_worker, &mut effects)
                .expect("a normal worker exit should be reaped");
            assert_eq!(
                finished,
                FinishedWorkers {
                    plan: true,
                    apply: false
                }
            );

            let (_, drained) =
                receive_messages(&receiver, &mut state, &mut execution_view, &mut effects);
            assert!(!drained);
            assert!(matches!(
                dispatch_finished_workers(&mut state, &mut execution_view, finished, &mut effects,),
                Some(SessionOutcome::Failed(ExecutionStage::Initializing))
            ));
        }

        #[test]
        fn finished_apply_without_final_message_keeps_the_failed_result() {
            let (sender, _receiver) = mpsc::channel();
            let handle = thread::spawn(|| {});
            wait_for_finished(&handle);
            let mut plan_worker = worker_guard(None);
            let mut apply_worker = worker_guard(Some(handle));
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut effects =
                test_effects(&sender, &cancellation, &mut clipboard, &mut apply_worker);
            let mut state = SessionState::Apply(Box::new(ExecutionState::applying(
                Instant::now(),
                ExecutionContext::loading("/project"),
            )));
            let mut execution_view = execution::ExecutionViewState::default();

            let finished = reap_workers(&mut plan_worker, &mut effects)
                .expect("a normal worker exit should be reaped");
            assert_eq!(
                finished,
                FinishedWorkers {
                    plan: false,
                    apply: true
                }
            );
            assert!(
                dispatch_finished_workers(&mut state, &mut execution_view, finished, &mut effects,)
                    .is_none()
            );
            let apply = state.apply().expect("apply state should remain visible");
            assert_eq!(apply.stage(), ExecutionStage::ApplyFailed);
            assert!(apply.result().is_some());
        }

        #[test]
        fn finished_plan_after_apply_started_does_not_fail_apply() {
            let (sender, _receiver) = mpsc::channel();
            let handle = thread::spawn(|| {});
            wait_for_finished(&handle);
            let mut plan_worker = worker_guard(Some(handle));
            let mut apply_worker = worker_guard(None);
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut effects =
                test_effects(&sender, &cancellation, &mut clipboard, &mut apply_worker);
            let mut state = apply_state(Instant::now(), None);
            let mut execution_view = execution::ExecutionViewState::default();

            let finished = reap_workers(&mut plan_worker, &mut effects)
                .expect("a normal worker exit should be reaped");
            assert_eq!(
                finished,
                FinishedWorkers {
                    plan: true,
                    apply: false
                }
            );
            assert!(
                dispatch_finished_workers(&mut state, &mut execution_view, finished, &mut effects,)
                    .is_none()
            );
            assert!(state.apply().is_some_and(|apply| apply.result().is_none()));
        }

        #[test]
        fn plan_worker_panic_is_reported_without_exposing_its_payload() {
            let (sender, _receiver) = mpsc::channel();
            let handle = thread::spawn(|| panic!("secret panic payload"));
            wait_for_finished(&handle);
            let mut plan_worker = worker_guard(Some(handle));
            let mut apply_worker = worker_guard(None);
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut effects =
                test_effects(&sender, &cancellation, &mut clipboard, &mut apply_worker);

            let error = reap_workers(&mut plan_worker, &mut effects)
                .expect_err("a panicked plan worker should fail the runtime");
            assert_eq!(error.to_string(), "plan worker panicked");
        }

        #[test]
        fn apply_worker_panic_is_reported_without_exposing_its_payload() {
            let (sender, _receiver) = mpsc::channel();
            let handle = thread::spawn(|| panic!("secret panic payload"));
            wait_for_finished(&handle);
            let mut plan_worker = worker_guard(None);
            let mut apply_worker = worker_guard(Some(handle));
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut effects =
                test_effects(&sender, &cancellation, &mut clipboard, &mut apply_worker);

            let error = reap_workers(&mut plan_worker, &mut effects)
                .expect_err("a panicked apply worker should fail the runtime");
            assert_eq!(error.to_string(), "apply worker panicked");
        }

        #[test]
        fn final_message_after_the_first_drain_is_processed_before_disconnect() {
            let (sender, receiver) = mpsc::channel();
            let (release_sender, release_receiver) = mpsc::sync_channel(0);
            let worker_sender = sender.clone();
            let handle = thread::spawn(move || {
                release_receiver
                    .recv()
                    .expect("test should release the worker");
                worker_sender
                    .send(PlanReviewMessage::Failed {
                        message: "late final message".to_owned(),
                        interrupted: false,
                    })
                    .expect("the receiver should still be alive");
            });
            let mut plan_worker = worker_guard(Some(handle));
            let mut apply_worker = worker_guard(None);
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut effects =
                test_effects(&sender, &cancellation, &mut clipboard, &mut apply_worker);
            let mut state = SessionState::new(ExecutionState::with_context(
                Instant::now(),
                ExecutionContext::loading("/project"),
            ));
            let mut execution_view = execution::ExecutionViewState::default();

            let (_, drained) =
                receive_messages(&receiver, &mut state, &mut execution_view, &mut effects);
            assert!(!drained);
            release_sender
                .send(())
                .expect("the worker should still be waiting");
            wait_for_finished(plan_worker.handle.as_ref().expect("plan handle"));

            let finished = reap_workers(&mut plan_worker, &mut effects)
                .expect("the worker should exit normally");
            let (_, drained) =
                receive_messages(&receiver, &mut state, &mut execution_view, &mut effects);
            assert!(drained);
            assert!(
                dispatch_finished_workers(&mut state, &mut execution_view, finished, &mut effects,)
                    .is_none()
            );
            assert!(
                state
                    .execution()
                    .is_some_and(|execution| execution.result().is_some())
            );
        }

        #[test]
        fn final_message_does_not_require_a_finished_worker() {
            let (sender, receiver) = mpsc::channel();
            let (sent_sender, sent_receiver) = mpsc::sync_channel(0);
            let (release_sender, release_receiver) = mpsc::sync_channel(0);
            let worker_sender = sender.clone();
            let handle = thread::spawn(move || {
                worker_sender
                    .send(PlanReviewMessage::Failed {
                        message: "worker still alive".to_owned(),
                        interrupted: false,
                    })
                    .expect("the receiver should still be alive");
                sent_sender
                    .send(())
                    .expect("the test should observe the send");
                release_receiver
                    .recv()
                    .expect("the test should release the worker");
            });
            let mut plan_worker = worker_guard(Some(handle));
            let mut apply_worker = worker_guard(None);
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut effects =
                test_effects(&sender, &cancellation, &mut clipboard, &mut apply_worker);
            let mut state = SessionState::new(ExecutionState::with_context(
                Instant::now(),
                ExecutionContext::loading("/project"),
            ));
            let mut execution_view = execution::ExecutionViewState::default();
            sent_receiver
                .recv()
                .expect("the worker should have sent its final message");

            let (_, drained) =
                receive_messages(&receiver, &mut state, &mut execution_view, &mut effects);
            assert!(drained);
            let finished = reap_workers(&mut plan_worker, &mut effects)
                .expect("a live worker should not be joined");
            assert_eq!(finished, FinishedWorkers::default());
            assert!(
                dispatch_finished_workers(&mut state, &mut execution_view, finished, &mut effects,)
                    .is_none()
            );
            assert!(
                state
                    .execution()
                    .is_some_and(|execution| execution.result().is_some())
            );

            release_sender
                .send(())
                .expect("the worker should still be waiting");
            wait_for_finished(plan_worker.handle.as_ref().expect("plan handle"));
            assert_eq!(
                reap_workers(&mut plan_worker, &mut effects)
                    .expect("the worker should exit normally"),
                FinishedWorkers {
                    plan: true,
                    apply: false
                }
            );
        }

        #[test]
        fn delayed_final_message_after_cancellation_is_processed_before_disconnect() {
            let (sender, receiver) = mpsc::channel();
            let (release_sender, release_receiver) = mpsc::sync_channel(0);
            let (sent_sender, sent_receiver) = mpsc::sync_channel(0);
            let worker_sender = sender.clone();
            let handle = thread::spawn(move || {
                release_receiver
                    .recv()
                    .expect("test should release the worker");
                worker_sender
                    .send(PlanReviewMessage::Completed(PlanReview::new(
                        PathBuf::from("/project"),
                        "default".to_owned(),
                        plan_document("No changes.\n".to_owned()),
                        Plan::empty(),
                        PlanMetadata::new(false),
                        Vec::new(),
                    )))
                    .expect("the receiver should still be alive");
                sent_sender
                    .send(())
                    .expect("the test should observe the send");
            });
            let mut plan_worker = worker_guard(Some(handle));
            let mut apply_worker = worker_guard(None);
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut effects =
                test_effects(&sender, &cancellation, &mut clipboard, &mut apply_worker);
            let mut state = SessionState::new(ExecutionState::with_context(
                Instant::now(),
                ExecutionContext::loading("/project"),
            ));
            let mut execution_view = execution::ExecutionViewState::default();

            assert!(
                dispatch(
                    &mut state,
                    Action::Execution(ExecutionAction::RequestCancellation),
                    &mut execution_view,
                    &mut effects,
                )
                .is_none()
            );
            assert!(
                state
                    .execution()
                    .is_some_and(ExecutionState::cancellation_requested)
            );
            release_sender
                .send(())
                .expect("the worker should still be waiting");
            sent_receiver
                .recv()
                .expect("the test should observe the final message");
            wait_for_finished(plan_worker.handle.as_ref().expect("plan handle"));

            let finished = reap_workers(&mut plan_worker, &mut effects)
                .expect("the worker should exit normally");
            let (outcome, drained) =
                receive_messages(&receiver, &mut state, &mut execution_view, &mut effects);
            assert!(drained);
            assert_eq!(
                outcome,
                Some(SessionOutcome::Interrupted(ExecutionStage::Initializing))
            );
            assert!(finished.plan);
            assert_eq!(
                reap_workers(&mut plan_worker, &mut effects)
                    .expect("a reaped worker has no handle"),
                FinishedWorkers::default()
            );
            assert!(
                dispatch_finished_workers(
                    &mut state,
                    &mut execution_view,
                    FinishedWorkers::default(),
                    &mut effects,
                )
                .is_none()
            );
        }

        #[test]
        fn final_message_then_worker_panic_is_reported_by_outer_join() {
            let (sender, receiver) = mpsc::channel();
            let worker_sender = sender.clone();
            let handle = thread::spawn(move || {
                worker_sender
                    .send(PlanReviewMessage::Completed(PlanReview::new(
                        PathBuf::from("/project"),
                        "default".to_owned(),
                        plan_document("No changes.\n".to_owned()),
                        Plan::empty(),
                        PlanMetadata::new(false),
                        Vec::new(),
                    )))
                    .expect("the receiver should still be alive");
                panic!("secret panic payload");
            });
            wait_for_finished(&handle);
            let mut plan_worker = worker_guard(Some(handle));
            let mut apply_worker = worker_guard(None);
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut effects =
                test_effects(&sender, &cancellation, &mut clipboard, &mut apply_worker);
            let mut state = SessionState::new(ExecutionState::with_context(
                Instant::now(),
                ExecutionContext::loading("/project"),
            ));
            let mut execution_view = execution::ExecutionViewState::default();

            let (outcome, drained) =
                receive_messages(&receiver, &mut state, &mut execution_view, &mut effects);
            assert!(drained);
            assert!(outcome.is_none());
            assert!(
                state.review().is_some(),
                "the final review message should complete the UI state"
            );
            let plan_join = plan_worker.join();
            let ui_outcome = Ok(SessionOutcome::Reviewed { changes: None });
            let error = finalize_ui_result(ui_outcome, &Ok(()), &plan_join)
                .expect_err("the outer join should report the worker panic");
            assert_eq!(error.to_string(), "plan worker panicked");
        }

        fn wait_for_finished(handle: &JoinHandle<()>) {
            while !handle.is_finished() {
                thread::yield_now();
            }
        }
    }

    mod copy_feedback {
        use super::*;

        struct DrawCase {
            name: &'static str,
            state: SessionState,
            dirty: bool,
            expected: bool,
        }

        #[derive(Debug, Clone, Copy)]
        enum CopyFlashTarget {
            Review,
            Overview,
            Apply,
        }

        #[test]
        fn draw_decision_covers_dirty_runtime_and_feedback_states() {
            let started_at = Instant::now();
            let mut failed_execution =
                ExecutionState::with_context(started_at, ExecutionContext::loading("failed"));
            failed_execution.fail("plan failed".to_owned(), started_at);

            let mut review_feedback = review_state();
            record_copy(
                &mut review_feedback,
                CopyTarget::Plan,
                CopyResult::Written,
                started_at,
            );
            let mut apply_feedback = apply_state(started_at, Some(ApplyStatus::Succeeded));
            record_copy(
                &mut apply_feedback,
                CopyTarget::Execution,
                CopyResult::Written,
                started_at,
            );

            assert_draw_cases([
                DrawCase {
                    name: "no_feedback",
                    state: review_state(),
                    dirty: false,
                    expected: false,
                },
                DrawCase {
                    name: "dirty_confirmation",
                    state: confirmation_state(),
                    dirty: true,
                    expected: true,
                },
                DrawCase {
                    name: "clean_confirmation",
                    state: confirmation_state(),
                    dirty: false,
                    expected: false,
                },
                DrawCase {
                    name: "running_execution",
                    state: SessionState::new(ExecutionState::with_context(
                        started_at,
                        ExecutionContext::loading("loading..."),
                    )),
                    dirty: false,
                    expected: true,
                },
                DrawCase {
                    name: "failed_execution_static",
                    state: SessionState::new(failed_execution.clone()),
                    dirty: false,
                    expected: false,
                },
                DrawCase {
                    name: "failed_execution_after_resize",
                    state: SessionState::new(failed_execution),
                    dirty: true,
                    expected: true,
                },
                DrawCase {
                    name: "apply_in_progress",
                    state: apply_state(started_at, None),
                    dirty: false,
                    expected: true,
                },
                DrawCase {
                    name: "apply_succeeded",
                    state: apply_state(started_at, Some(ApplyStatus::Succeeded)),
                    dirty: false,
                    expected: false,
                },
                DrawCase {
                    name: "review_feedback_pending",
                    state: review_feedback,
                    dirty: false,
                    expected: true,
                },
                DrawCase {
                    name: "finished_apply_feedback_pending",
                    state: apply_feedback,
                    dirty: false,
                    expected: true,
                },
            ]);
        }

        #[test]
        fn failed_execution_copy_notice_draws_once_when_it_expires() {
            let started_at = Instant::now();
            let mut execution =
                ExecutionState::with_context(started_at, ExecutionContext::loading("failed"));
            execution.fail("plan failed".to_owned(), started_at);
            let mut state = SessionState::new(execution);
            let copied_at = started_at + Duration::from_millis(100);
            let notice_expired_at = copied_at + Duration::from_secs(5);
            record_copy(
                &mut state,
                CopyTarget::Diagnostic,
                CopyResult::Written,
                copied_at,
            );

            assert!(should_draw(&state, false));

            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let execution_view = execution::ExecutionViewState::default();
            let review_view = plan_review::PlanReviewViewState::default();
            let confirmation_view = plan_review::ApplyConfirmationViewState::default();
            let mut dirty = false;
            assert!(
                draw_if_needed(
                    &mut state,
                    &mut terminal,
                    execution_view,
                    &review_view,
                    &confirmation_view,
                    &mut dirty,
                    notice_expired_at,
                )
                .expect("expired diagnostic copy notice should render once")
            );
            assert!(!should_draw(&state, false));
            assert!(
                !draw_if_needed(
                    &mut state,
                    &mut terminal,
                    execution_view,
                    &review_view,
                    &confirmation_view,
                    &mut dirty,
                    notice_expired_at,
                )
                .expect("cleared diagnostic copy notice should stop rendering")
            );
        }

        fn assert_draw_cases(cases: impl IntoIterator<Item = DrawCase>) {
            for case in cases {
                assert_eq!(
                    should_draw(&case.state, case.dirty),
                    case.expected,
                    "case: {}",
                    case.name
                );
            }
        }

        #[rstest]
        #[case::review(CopyFlashTarget::Review)]
        #[case::overview(CopyFlashTarget::Overview)]
        #[case::apply(CopyFlashTarget::Apply)]
        fn successful_copy_flash_lifecycle_draws_through_the_runtime_step(
            #[case] target: CopyFlashTarget,
        ) {
            let started_at = Instant::now();
            let flash_active_at = started_at + Duration::from_millis(100);
            let expired_at = started_at + Duration::from_millis(200);
            let (mut state, mut terminal, execution_view, review_view, confirmation_view) =
                copy_runtime_fixture(target, started_at);
            let never_copied = terminal.backend().buffer().clone();
            let before_copy = copy_target_cells(target, &terminal);
            record_copy(
                &mut state,
                target.copy_target(),
                CopyResult::Written,
                started_at,
            );
            let mut dirty = false;
            assert!(
                draw_if_needed(
                    &mut state,
                    &mut terminal,
                    execution_view,
                    &review_view,
                    &confirmation_view,
                    &mut dirty,
                    started_at,
                )
                .expect("copy result should render")
            );

            let flash = copy_target_cells(target, &terminal);
            assert_ne!(flash, before_copy, "{}", terminal_text(&terminal));
            assert!(
                draw_if_needed(
                    &mut state,
                    &mut terminal,
                    execution_view,
                    &review_view,
                    &confirmation_view,
                    &mut dirty,
                    flash_active_at,
                )
                .expect("active flash should render")
            );

            assert_eq!(copy_target_cells(target, &terminal), flash);

            assert!(
                draw_if_needed(
                    &mut state,
                    &mut terminal,
                    execution_view,
                    &review_view,
                    &confirmation_view,
                    &mut dirty,
                    expired_at,
                )
                .expect("expired flash should render")
            );

            assert!(should_draw(&state, false));
            assert_eq!(copy_target_cells(target, &terminal), before_copy);
            match target {
                CopyFlashTarget::Review | CopyFlashTarget::Overview => {
                    assert!(terminal_text(&terminal).contains("Copied."));
                }
                CopyFlashTarget::Apply => {
                    assert!(terminal_text(&terminal).contains("Apply complete"));
                }
            }

            assert!(
                draw_if_needed(
                    &mut state,
                    &mut terminal,
                    execution_view,
                    &review_view,
                    &confirmation_view,
                    &mut dirty,
                    expired_at,
                )
                .expect("copy notice should keep the static result rendered")
            );

            let notice_expired_at = started_at + Duration::from_secs(3);
            assert!(
                draw_if_needed(
                    &mut state,
                    &mut terminal,
                    execution_view,
                    &review_view,
                    &confirmation_view,
                    &mut dirty,
                    notice_expired_at,
                )
                .expect("expired copy notice should render once")
            );
            assert!(!should_draw(&state, false));
            assert_eq!(
                terminal.backend().buffer(),
                &never_copied,
                "{}",
                terminal_text(&terminal)
            );
        }

        #[rstest]
        #[case::review(CopyFlashTarget::Review)]
        #[case::overview(CopyFlashTarget::Overview)]
        #[case::apply(CopyFlashTarget::Apply)]
        fn failed_copy_shows_only_the_notification_through_the_runtime_step(
            #[case] target: CopyFlashTarget,
        ) {
            let started_at = Instant::now();
            let (mut state, mut terminal, execution_view, review_view, confirmation_view) =
                copy_runtime_fixture(target, started_at);
            let before_copy = copy_target_cells(target, &terminal);
            record_copy(
                &mut state,
                target.copy_target(),
                CopyResult::Failed,
                started_at,
            );
            let mut dirty = true;

            assert!(
                draw_if_needed(
                    &mut state,
                    &mut terminal,
                    execution_view,
                    &review_view,
                    &confirmation_view,
                    &mut dirty,
                    started_at,
                )
                .expect("failed copy result should render")
            );
            assert_eq!(copy_target_cells(target, &terminal), before_copy);
            assert!(terminal_text(&terminal).contains("Copy failed."));
            assert!(should_draw(&state, false));
            let notice_expired_at = started_at + Duration::from_secs(5);
            assert!(
                draw_if_needed(
                    &mut state,
                    &mut terminal,
                    execution_view,
                    &review_view,
                    &confirmation_view,
                    &mut dirty,
                    notice_expired_at,
                )
                .expect("expired copy failure notice should render once")
            );
            assert!(!terminal_text(&terminal).contains("Copy failed."));
            assert!(!should_draw(&state, false));
        }

        fn copy_target_cells(
            target: CopyFlashTarget,
            terminal: &Terminal<TestBackend>,
        ) -> Vec<Cell> {
            let texts: &[&str] = match target {
                CopyFlashTarget::Review => &["copy body marker", "terraform_data.api"],
                CopyFlashTarget::Overview => &["terraform_data.api"],
                CopyFlashTarget::Apply => &["flash"],
            };
            texts
                .iter()
                .flat_map(|text| text_cells(terminal, text))
                .collect()
        }

        fn text_cells(terminal: &Terminal<TestBackend>, text: &str) -> Vec<Cell> {
            let buffer = terminal.backend().buffer();
            let area = buffer.area();
            for y in area.y..area.bottom() {
                let symbols = (area.x..area.right())
                    .map(|x| buffer.cell((x, y)).expect("text cell").symbol())
                    .collect::<Vec<_>>();
                for start in 0..symbols.len() {
                    if !symbols[start..].concat().starts_with(text) {
                        continue;
                    }
                    return (0..text.chars().count())
                        .map(|offset| {
                            let x = area.x + u16::try_from(start + offset).expect("text offset");
                            buffer.cell((x, y)).expect("text cell").clone()
                        })
                        .collect();
                }
            }
            panic!("{text} should be visible\n{}", terminal_text(terminal));
        }

        fn review_state() -> SessionState {
            SessionState::Review(Box::new(ReviewSessionState::new(review_plan())))
        }

        fn copy_flash_state(target: CopyFlashTarget, started_at: Instant) -> SessionState {
            match target {
                CopyFlashTarget::Review => {
                    let mut review = PlanReview::new(
                        PathBuf::from("/project"),
                        "default".to_owned(),
                        plan_document(copy_plan_text()),
                        Plan::empty(),
                        PlanMetadata::new(false),
                        Vec::new(),
                    );
                    review.set_search_query("terraform_data".to_owned());
                    SessionState::Review(Box::new(ReviewSessionState::new(review)))
                }
                CopyFlashTarget::Overview => SessionState::Review(Box::new(
                    session::test_support::overview_session(PlanReview::new(
                        PathBuf::from("/project"),
                        "default".to_owned(),
                        plan_document(copy_plan_text()),
                        Plan {
                            resource_changes: vec![resource_change(
                                "terraform_data.api",
                                ResourceChangeKind::Update,
                            )],
                            ..Plan::empty()
                        },
                        PlanMetadata::new(false),
                        Vec::new(),
                    )),
                )),
                CopyFlashTarget::Apply => {
                    let mut state = apply_state(started_at, None);
                    if let SessionState::Apply(execution) = &mut state {
                        execution.record(ExecutionEvent {
                            received_at: started_at,
                            kind: ExecutionEventKind::Informational {
                                event_type: "log".to_owned(),
                                message: Some("flash".to_owned()),
                            },
                        });
                        execution.finish_apply(ApplyStatus::Succeeded, None, None, started_at);
                    }
                    state
                }
            }
        }

        fn copy_runtime_fixture(
            target: CopyFlashTarget,
            started_at: Instant,
        ) -> (
            SessionState,
            Terminal<TestBackend>,
            execution::ExecutionViewState,
            plan_review::PlanReviewViewState,
            plan_review::ApplyConfirmationViewState,
        ) {
            let mut state = copy_flash_state(target, started_at);
            // The narrow Overview has no footer width to spare, so a leftover notice reservation
            // would hide hints that the never-copied frame shows.
            let (width, height) = match target {
                CopyFlashTarget::Overview => (40, 16),
                CopyFlashTarget::Review | CopyFlashTarget::Apply => (80, 24),
            };
            let mut terminal =
                Terminal::new(TestBackend::new(width, height)).expect("test terminal");
            let execution_view = execution::ExecutionViewState::default();
            let review_view = match target {
                CopyFlashTarget::Review => copy_review_view(&state),
                CopyFlashTarget::Overview | CopyFlashTarget::Apply => {
                    plan_review::PlanReviewViewState::default()
                }
            };
            let confirmation_view = plan_review::ApplyConfirmationViewState::default();
            let mut dirty = true;
            draw_if_needed(
                &mut state,
                &mut terminal,
                execution_view,
                &review_view,
                &confirmation_view,
                &mut dirty,
                started_at,
            )
            .expect("state before copy should render");
            (
                state,
                terminal,
                execution_view,
                review_view,
                confirmation_view,
            )
        }

        fn copy_review_view(state: &SessionState) -> plan_review::PlanReviewViewState {
            let area = Rect::new(0, 0, 80, 24);
            let review = state.review().expect("review state");
            let query = review.review().search_query();
            let layout = plan_review::layout(area, false, review);
            let mut view = plan_review::PlanReviewViewState::default();
            view.apply_with_matches(
                plan_review::PlanReviewInput::Down,
                area,
                layout.max_vertical(),
                layout.max_horizontal(),
                query,
                &[],
            );
            view.apply_with_matches(
                plan_review::PlanReviewInput::Right,
                area,
                layout.max_vertical(),
                layout.max_horizontal(),
                query,
                &[],
            );
            view.apply_with_matches(
                plan_review::PlanReviewInput::SearchStart,
                area,
                layout.max_vertical(),
                layout.max_horizontal(),
                query,
                &[],
            );
            view
        }

        fn record_copy(
            state: &mut SessionState,
            target: CopyTarget,
            result: CopyResult,
            now: Instant,
        ) {
            session::update(state, Action::CopyCompleted { target, result }, now);
        }

        impl CopyFlashTarget {
            fn copy_target(self) -> CopyTarget {
                match self {
                    Self::Review | Self::Overview => CopyTarget::Plan,
                    Self::Apply => CopyTarget::Execution,
                }
            }
        }
    }

    mod confirmation {
        use super::*;

        #[test]
        fn confirmation_input_is_drawn_through_the_runtime_step() {
            let now = Instant::now();
            let mut state = confirmation_state();
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let mut views = ScreenViews::default();
            let mut dirty = true;

            let action = views
                .handle_key(&terminal, &state, KeyCode::Char('y'), KeyModifiers::NONE)
                .expect("confirmation input should be handled");

            assert_eq!(action, None);
            assert_eq!(views.confirmation.input(), "y");

            assert!(
                draw_if_needed(
                    &mut state,
                    &mut terminal,
                    views.execution,
                    &views.review,
                    &views.confirmation,
                    &mut dirty,
                    now,
                )
                .expect("confirmation should render")
            );

            assert!(!dirty);
            let text = terminal_text(&terminal);
            assert!(text.contains("Type yes to apply (exact match)."));
            assert!(text.contains("│ > y|"), "{text}");
        }

        #[test]
        fn confirmation_body_scrolls_up_right_after_paging_past_the_end() {
            let now = Instant::now();
            let mut state = SessionState::Review(Box::new(
                session::test_support::apply_confirmation_session(PlanReview::new(
                    PathBuf::from("/project"),
                    "default".to_owned(),
                    plan_document("Plan: 0 to add, 0 to change, 20 to destroy.\n".to_owned()),
                    Plan {
                        resource_changes: (0..20)
                            .map(|index| {
                                resource_change(
                                    &format!("terraform_data.old_{index:02}"),
                                    ResourceChangeKind::Delete,
                                )
                            })
                            .collect(),
                        ..Plan::empty()
                    },
                    PlanMetadata::new(true),
                    Vec::new(),
                )),
            ));
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let mut views = ScreenViews::default();
            for _ in 0..10 {
                views
                    .handle_key(&terminal, &state, KeyCode::PageDown, KeyModifiers::NONE)
                    .expect("confirmation page down should be handled");
            }
            let end = render_apply_to_text(&mut state, &mut terminal, &views, now);

            views
                .handle_key(&terminal, &state, KeyCode::Up, KeyModifiers::NONE)
                .expect("confirmation scroll up should be handled");
            let scrolled = render_apply_to_text(&mut state, &mut terminal, &views, now);

            assert!(end.contains("terraform_data.old_19"), "{end}");
            assert!(!scrolled.contains("terraform_data.old_19"), "{scrolled}");
            assert!(scrolled.contains("terraform_data.old_18"), "{scrolled}");
        }
    }

    mod review {
        use super::*;

        #[test]
        fn cancelling_confirmation_preserves_review_position() {
            let now = Instant::now();
            let plan = PlanReview::new(
                PathBuf::from("/project"),
                "staging".to_owned(),
                plan_document(
                    (0..60)
                        .map(|index| format!("review line {index}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                ),
                Plan {
                    resource_changes: vec![resource_change(
                        "terraform_data.api",
                        ResourceChangeKind::Update,
                    )],
                    ..Plan::empty()
                },
                PlanMetadata::new(true),
                Vec::new(),
            );
            let mut state = SessionState::Review(Box::new(ReviewSessionState::new(plan)));
            let terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let mut views = ScreenViews::default();
            let review = state.review().expect("review state");
            let layout = plan_review::layout(Rect::new(0, 0, 80, 24), false, review);
            views.review.apply_with_matches(
                plan_review::PlanReviewInput::Down,
                layout.body(),
                layout.max_vertical(),
                layout.max_horizontal(),
                review.review().search_query(),
                &[],
            );
            let position = views.review.scroll();

            let open = views
                .handle_key(&terminal, &state, KeyCode::Char('a'), KeyModifiers::NONE)
                .expect("apply key should be handled")
                .expect("apply key should open confirmation");
            update_session(&mut state, open, &mut views.execution, now);
            let cancel = views
                .handle_key(&terminal, &state, KeyCode::Esc, KeyModifiers::NONE)
                .expect("escape confirmation should be handled")
                .expect("escape should cancel");
            update_session(&mut state, cancel, &mut views.execution, now);

            let review = state.review().expect("cancel should restore review");
            assert!(review.review().search_query().is_empty());
            assert_eq!(views.review.scroll(), position);
        }

        #[test]
        fn confirmed_filter_keeps_full_plan_actions_available() {
            let now = Instant::now();
            let mut plan = PlanReview::new(
                PathBuf::from("/project"),
                "default".to_owned(),
                plan_document("Plan: 1 to add.\n".to_owned()),
                Plan {
                    resource_changes: vec![resource_change(
                        "terraform_data.worker",
                        ResourceChangeKind::Create,
                    )],
                    ..Plan::empty()
                },
                PlanMetadata::new(true),
                Vec::new(),
            );
            plan.set_search_query("worker".to_owned());
            let mut state = SessionState::Review(Box::new(ReviewSessionState::new(plan)));
            let terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let mut views = ScreenViews::default();

            assert!(matches!(
                views
                    .handle_key(&terminal, &state, KeyCode::Char('a'), KeyModifiers::NONE)
                    .expect("apply key should be handled"),
                Some(Action::OpenApplyConfirmation)
            ));
            assert!(matches!(
                views
                    .handle_key(&terminal, &state, KeyCode::Char('y'), KeyModifiers::NONE)
                    .expect("copy key should be handled"),
                Some(Action::Copy(CopyTarget::Plan))
            ));
            assert!(matches!(
                views
                    .handle_key(&terminal, &state, KeyCode::Char('q'), KeyModifiers::NONE)
                    .expect("quit key should be handled"),
                Some(Action::Quit)
            ));
            assert_eq!(
                views
                    .handle_key(&terminal, &state, KeyCode::Char('c'), KeyModifiers::CONTROL)
                    .expect("control-c should be handled"),
                Some(Action::Quit)
            );

            let clear = views
                .handle_key(&terminal, &state, KeyCode::Esc, KeyModifiers::NONE)
                .expect("clear filter key should be handled")
                .expect("clear filter should update the review");
            update_session(&mut state, clear, &mut views.execution, now);
            assert_eq!(
                state
                    .review()
                    .expect("review state")
                    .review()
                    .search_query(),
                ""
            );

            assert!(matches!(
                views
                    .handle_key(&terminal, &state, KeyCode::Char('y'), KeyModifiers::NONE)
                    .expect("copy key should be handled"),
                Some(Action::Copy(CopyTarget::Plan))
            ));
        }
    }

    mod quit_confirmation {
        use super::*;

        #[test]
        fn overview_quit_is_visible_before_runtime_dispatch() {
            let now = Instant::now();
            let mut state = overview_state();
            session::update(
                &mut state,
                Action::CopyCompleted {
                    target: CopyTarget::Plan,
                    result: CopyResult::Written,
                },
                now,
            );
            let terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let mut views = ScreenViews::default();

            let action = views
                .handle_key(&terminal, &state, KeyCode::Char('q'), KeyModifiers::NONE)
                .expect("overview quit input should be handled");
            assert_eq!(action, Some(Action::Quit));

            let mut quit_confirmation = false;
            assert_eq!(
                action_after_quit_confirmation(
                    action.expect("quit should produce an action"),
                    false,
                    &mut quit_confirmation,
                ),
                None
            );
            assert!(quit_confirmation);

            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            draw_with_quit_confirmation(
                &state,
                &mut terminal,
                views.execution,
                &views.review,
                &views.confirmation,
                now,
                quit_confirmation,
            )
            .expect("overview confirmation should render");
            let text = terminal_text(&terminal);
            assert!(text.contains("Quit Terraleph?"), "{text}");
            assert!(text.contains("Copied."), "{text}");
            assert!(!text.contains("q quit"), "{text}");

            quit_confirmation = false;
            assert!(state.overview().is_some());

            assert!(matches!(
                action_after_quit_confirmation(Action::Quit, true, &mut quit_confirmation),
                Some(Action::Quit)
            ));
            assert!(matches!(
                session::update(&mut state, Action::Quit, now),
                Some(Effect::Finish(SessionOutcome::Reviewed { .. }))
            ));
        }

        #[test]
        fn forwarded_quit_confirmation_key_reaches_the_current_screen_once() {
            let state = confirmation_state();
            let terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let mut execution_view = execution::ExecutionViewState::default();
            let mut review_view = plan_review::PlanReviewViewState::default();
            let mut confirmation_view = plan_review::ApplyConfirmationViewState::default();
            let raw_key = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::SHIFT);

            let QuitConfirmationInput::Forward(key) = quit_confirmation_key_to_input(raw_key)
            else {
                panic!("the screen input should be forwarded");
            };
            assert_eq!(key, raw_key);

            assert_eq!(
                handle_key_event(
                    &terminal,
                    &state,
                    &mut execution_view,
                    &mut review_view,
                    &mut confirmation_view,
                    key,
                )
                .expect("forwarded screen input should be handled"),
                None
            );
            assert_eq!(confirmation_view.input(), "y");
        }

        fn overview_state() -> SessionState {
            SessionState::Review(Box::new(session::test_support::overview_session(
                review_plan(),
            )))
        }
    }

    mod viewport {
        use super::*;

        #[test]
        fn resize_clamps_the_review_offset_after_shrinking_and_growing() {
            let state = SessionState::Review(Box::new(ReviewSessionState::new(PlanReview::new(
                PathBuf::from("/project"),
                "default".to_owned(),
                plan_document(copy_plan_text()),
                Plan::empty(),
                PlanMetadata::new(false),
                Vec::new(),
            ))));
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let mut views = ScreenViews::default();
            let shrunk = resize(&mut terminal, &state, &mut views.review, 60, 16);
            views
                .handle_key(&terminal, &state, KeyCode::End, KeyModifiers::NONE)
                .expect("review end should be handled");
            let shrunk_max =
                plan_review::layout(shrunk, false, state.review().expect("review")).max_vertical();
            assert_eq!(views.review.scroll().0, shrunk_max);

            let grown = resize(&mut terminal, &state, &mut views.review, 80, 24);

            let grown_max =
                plan_review::layout(grown, false, state.review().expect("review")).max_vertical();
            assert!(grown_max < shrunk_max);
            assert_eq!(views.review.scroll().0, grown_max);
        }

        #[test]
        fn resize_keeps_the_overview_selection_visible_after_shrinking_and_growing() {
            let mut state = SessionState::Review(Box::new(
                session::test_support::overview_session(PlanReview::new(
                    PathBuf::from("/project"),
                    "default".to_owned(),
                    plan_document("Plan: 0 to add, 30 to change, 0 to destroy.\n".to_owned()),
                    Plan {
                        resource_changes: (0..30)
                            .map(|index| {
                                resource_change(
                                    &format!("terraform_data.server_{index:02}"),
                                    ResourceChangeKind::Update,
                                )
                            })
                            .collect(),
                        ..Plan::empty()
                    },
                    PlanMetadata::new(true),
                    Vec::new(),
                )),
            ));
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("test terminal");
            let mut views = ScreenViews::default();
            resize(&mut terminal, &state, &mut views.review, 60, 16);
            views
                .handle_key(&terminal, &state, KeyCode::End, KeyModifiers::NONE)
                .expect("overview end should be handled");
            assert_eq!(views.review.overview().selected(), Some(29));
            let shrunk_scroll = views.review.overview().scroll();

            resize(&mut terminal, &state, &mut views.review, 100, 30);

            assert_eq!(views.review.overview().selected(), Some(29));
            assert!(views.review.overview().scroll() < shrunk_scroll);
            let mut dirty = true;
            draw_if_needed(
                &mut state,
                &mut terminal,
                views.execution,
                &views.review,
                &views.confirmation,
                &mut dirty,
                Instant::now(),
            )
            .expect("grown overview should render");
            let text = terminal_text(&terminal);
            assert!(text.contains("terraform_data.server_29"), "{text}");
        }

        fn resize(
            terminal: &mut Terminal<TestBackend>,
            state: &SessionState,
            review_view: &mut plan_review::PlanReviewViewState,
            width: u16,
            height: u16,
        ) -> Rect {
            terminal.backend_mut().resize(width, height);
            let area = Rect::new(0, 0, width, height);
            reconcile_resize(state, review_view, area, false);
            area
        }
    }

    mod apply_view {
        use super::*;

        #[rstest]
        #[case::failed_end(ApplyStatus::Failed, KeyCode::End, KeyModifiers::NONE)]
        #[case::failed_alt_right(ApplyStatus::Failed, KeyCode::Char('>'), KeyModifiers::ALT)]
        #[case::interrupted_end(ApplyStatus::Interrupted, KeyCode::End, KeyModifiers::NONE)]
        #[case::interrupted_alt_right(
            ApplyStatus::Interrupted,
            KeyCode::Char('>'),
            KeyModifiers::ALT
        )]
        fn end_keys_reach_the_rendered_log_tail(
            #[case] status: ApplyStatus,
            #[case] code: KeyCode,
            #[case] modifiers: KeyModifiers,
        ) {
            let started_at = Instant::now();
            let mut state = long_apply_state(started_at, Some(status));
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let mut views = ScreenViews::default();
            let action = views
                .handle_key(&terminal, &state, code, modifiers)
                .expect("end key should be handled");

            assert_eq!(action, None);
            let mut dirty = true;
            assert!(
                draw_if_needed(
                    &mut state,
                    &mut terminal,
                    views.execution,
                    &views.review,
                    &views.confirmation,
                    &mut dirty,
                    started_at,
                )
                .expect("result should render")
            );
            assert!(terminal_text(&terminal).contains("tail marker"));
        }

        #[test]
        fn manual_scroll_survives_new_log_until_end_restores_following() {
            let started_at = Instant::now();
            let mut state = long_apply_state(started_at, None);
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let mut views = ScreenViews::default();
            assert_eq!(
                views
                    .handle_key(&terminal, &state, KeyCode::Char('v'), KeyModifiers::NONE)
                    .expect("log viewer key should be handled"),
                None
            );
            assert!(views.execution.logs_open());

            assert_eq!(
                views
                    .handle_key(&terminal, &state, KeyCode::Down, KeyModifiers::NONE)
                    .expect("scroll key should be handled"),
                None
            );
            assert!(!views.execution.follows_latest());

            let new_log = ExecutionEvent {
                received_at: started_at + Duration::from_secs(1),
                kind: ExecutionEventKind::Log(ExecutionLogLine {
                    stream: EventStream::Stdout,
                    text: "new tail marker".to_owned(),
                }),
            };
            let _ = update_session(
                &mut state,
                Action::ApplyWorkerEvent(new_log),
                &mut views.execution,
                started_at + Duration::from_secs(1),
            );
            assert!(!views.execution.follows_latest());

            assert_eq!(
                views
                    .handle_key(&terminal, &state, KeyCode::End, KeyModifiers::NONE)
                    .expect("end key should be handled"),
                None
            );
            assert!(views.execution.follows_latest());

            let mut dirty = true;
            draw_if_needed(
                &mut state,
                &mut terminal,
                views.execution,
                &views.review,
                &views.confirmation,
                &mut dirty,
                started_at + Duration::from_secs(1),
            )
            .expect("updated log should render");
            assert!(terminal_text(&terminal).contains("new tail marker"));

            assert_apply_log_view_can_close_and_reopen(
                &mut state,
                &mut terminal,
                &mut views,
                started_at + Duration::from_secs(1),
            );
        }

        fn assert_apply_log_view_can_close_and_reopen(
            state: &mut SessionState,
            terminal: &mut Terminal<TestBackend>,
            views: &mut ScreenViews,
            now: Instant,
        ) {
            assert_eq!(
                views
                    .handle_key(terminal, state, KeyCode::Esc, KeyModifiers::NONE)
                    .expect("escape should close the log viewer"),
                None
            );
            assert!(!views.execution.logs_open());
            let compact_text = render_apply_to_text(state, terminal, views, now);
            assert!(compact_text.contains("new tail marker"));

            assert_eq!(
                views
                    .handle_key(terminal, state, KeyCode::Char('v'), KeyModifiers::NONE)
                    .expect("v should reopen the log viewer"),
                None
            );
            assert!(views.execution.logs_open());
            assert!(views.execution.follows_latest());
            let reopened_text = render_apply_to_text(state, terminal, views, now);
            assert!(reopened_text.contains("new tail marker"));
        }

        #[test]
        fn update_session_resets_only_when_apply_starts_or_finishes() {
            let now = Instant::now();
            let mut state = applyable_review_state();
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");
            let mut views = ScreenViews::default();
            views
                .execution
                .apply_scroll(execution::ExecutionScroll::Down, 10, 20, 10);
            views
                .execution
                .apply_horizontal_scroll(execution::ExecutionScroll::Right, 2, 5, 10);

            let apply_action = views
                .handle_key(&terminal, &state, KeyCode::Char('a'), KeyModifiers::NONE)
                .expect("apply key should be handled");
            assert!(
                update_session(
                    &mut state,
                    apply_action.expect("apply key should produce an action"),
                    &mut views.execution,
                    now,
                )
                .is_none()
            );
            assert_eq!(views.execution.horizontal(), 3);
            assert!(!views.execution.follows_latest());

            for character in "yes".chars() {
                assert!(
                    views
                        .handle_key(
                            &terminal,
                            &state,
                            KeyCode::Char(character),
                            KeyModifiers::NONE
                        )
                        .expect("confirmation key should be handled")
                        .is_none()
                );
            }
            let confirm_action = views
                .handle_key(&terminal, &state, KeyCode::Enter, KeyModifiers::NONE)
                .expect("confirmation should be handled");
            assert!(matches!(
                update_session(
                    &mut state,
                    confirm_action.expect("yes should produce an action"),
                    &mut views.execution,
                    now,
                ),
                Some(Effect::StartApply)
            ));
            assert_eq!(views.execution.horizontal(), 0);
            assert_eq!(views.execution.vertical_offset(2, 90), 2);

            assert_apply_start_path(&mut state, &mut terminal, &mut views, now);
            assert_apply_completion_and_copy_path(&mut state, &mut terminal, &mut views, now);
        }

        fn assert_apply_start_path(
            state: &mut SessionState,
            terminal: &mut Terminal<TestBackend>,
            views: &mut ScreenViews,
            now: Instant,
        ) {
            views.execution.open_logs();
            for index in 0..40 {
                let text = if index == 0 {
                    "apply log line 0 with enough width to exercise the production horizontal scrollbar after the result is complete".to_owned()
                } else if index == 39 {
                    "tail apply marker".to_owned()
                } else {
                    format!("apply log line {index}")
                };
                let _ = update_session(
                    state,
                    Action::ApplyWorkerEvent(ExecutionEvent {
                        received_at: now,
                        kind: ExecutionEventKind::Log(ExecutionLogLine {
                            stream: EventStream::Stdout,
                            text,
                        }),
                    }),
                    &mut views.execution,
                    now,
                );
            }
            let text = render_apply_to_text(state, terminal, views, now);
            assert_eq!(views.execution.horizontal(), 0);
            assert!(text.contains("tail apply marker"));

            for code in [KeyCode::Up, KeyCode::Right] {
                assert!(
                    views
                        .handle_key(terminal, state, code, KeyModifiers::NONE)
                        .expect("manual execution key should be handled")
                        .is_none()
                );
            }
            assert!(execution_scroll_position(state, views.execution) > 0);
            assert!(views.execution.horizontal() > 0);
        }

        fn assert_apply_completion_and_copy_path(
            state: &mut SessionState,
            terminal: &mut Terminal<TestBackend>,
            views: &mut ScreenViews,
            now: Instant,
        ) {
            let _ = update_session(
                state,
                Action::ApplyCompleted {
                    status: ApplyStatus::Succeeded,
                    summary_line: None,
                },
                &mut views.execution,
                now,
            );
            assert!(!views.execution.logs_open());
            assert_eq!(views.execution.horizontal(), 0);
            assert_eq!(views.execution.vertical_offset(2, 90), 2);
            let text = render_apply_to_text(state, terminal, views, now);
            assert!(text.contains("tail apply marker"));

            assert_eq!(
                views
                    .handle_key(terminal, state, KeyCode::Tab, KeyModifiers::NONE)
                    .expect("tab should focus logs after result"),
                None
            );
            for code in [KeyCode::Up, KeyCode::Right] {
                assert!(
                    views
                        .handle_key(terminal, state, code, KeyModifiers::NONE)
                        .expect("post-result execution key should be handled")
                        .is_none()
                );
            }
            let copied_vertical = execution_scroll_position(state, views.execution);
            let copied_horizontal = views.execution.horizontal();
            assert!(copied_vertical > 0);
            assert!(copied_horizontal > 0);

            let (sender, _messages) = std::sync::mpsc::channel();
            let cancellation = CancellationToken::new();
            let mut clipboard = TestClipboard;
            let mut apply_worker = WorkerGuard {
                cancellation: cancellation.clone(),
                handle: None,
            };
            let mut effects = RuntimeEffects {
                tool: Tool::Terraform,
                root: Path::new("/project"),
                display_root: Path::new("/project"),
                global_arguments: &[],
                apply_arguments: &[],
                sender: &sender,
                plan_path: Path::new("/project/review.tfplan"),
                cancellation: &cancellation,
                clipboard: &mut clipboard,
                apply_worker: &mut apply_worker,
                history: None,
            };
            assert!(
                dispatch(
                    state,
                    Action::Copy(CopyTarget::Execution),
                    &mut views.execution,
                    &mut effects,
                )
                .is_none()
            );
            assert!(
                state
                    .copy_feedback()
                    .expect("apply feedback")
                    .notice()
                    .is_some()
            );
            let _ = render_apply_to_text(state, terminal, views, now);
            assert_eq!(
                execution_scroll_position(state, views.execution),
                copied_vertical
            );
            assert_eq!(views.execution.horizontal(), copied_horizontal);
        }

        fn execution_scroll_position(
            state: &SessionState,
            view: execution::ExecutionViewState,
        ) -> u16 {
            let apply = state.apply().expect("apply state");
            execution::execution_scroll_position_with_view(
                apply,
                view,
                &execution::execution_layout_with_view(
                    ratatui::layout::Rect::new(0, 0, 80, 24),
                    apply,
                    view,
                ),
            )
            .0
        }

        fn long_apply_state(started_at: Instant, status: Option<ApplyStatus>) -> SessionState {
            let mut execution = ExecutionState::applying(
                started_at,
                ExecutionContext::loading("/project").with_workspace("default"),
            );
            for index in 0..40 {
                execution.record(ExecutionEvent {
                    received_at: started_at,
                    kind: ExecutionEventKind::Log(ExecutionLogLine {
                        stream: EventStream::Stdout,
                        text: if index == 39 {
                            "tail marker".to_owned()
                        } else {
                            format!("log line {index}")
                        },
                    }),
                });
            }
            if let Some(status) = status {
                execution.finish_apply(status, None, None, started_at);
            }
            SessionState::Apply(Box::new(execution))
        }

        // Output-only, so an apply completes without per-resource progress events.
        fn applyable_review_state() -> SessionState {
            SessionState::Review(Box::new(ReviewSessionState::new(PlanReview::new(
                PathBuf::from("/project"),
                "default".to_owned(),
                plan_document("Changes to Outputs:\n  + endpoint = \"example\"\n".to_owned()),
                Plan {
                    output_changes: vec![output_change("endpoint", PlanAction::Create)],
                    ..Plan::empty()
                },
                PlanMetadata::new(true),
                Vec::new(),
            ))))
        }
    }

    fn draw_if_needed<B: Backend>(
        state: &mut SessionState,
        terminal: &mut Terminal<B>,
        execution_view: execution::ExecutionViewState,
        review_view: &plan_review::PlanReviewViewState,
        confirmation_view: &plan_review::ApplyConfirmationViewState,
        dirty: &mut bool,
        now: Instant,
    ) -> Result<bool, B::Error> {
        draw_if_needed_with_quit_confirmation(
            state,
            terminal,
            execution_view,
            review_view,
            confirmation_view,
            dirty,
            now,
            false,
        )
    }

    struct TestClipboard;

    impl ClipboardWriter for TestClipboard {
        fn execute(&mut self, _effect: &CopyEffect) -> CopyResult {
            CopyResult::Written
        }
    }

    fn worker_guard(handle: Option<JoinHandle<()>>) -> WorkerGuard {
        WorkerGuard {
            cancellation: CancellationToken::new(),
            handle,
        }
    }

    fn test_effects<'a>(
        sender: &'a Sender<PlanReviewMessage>,
        cancellation: &'a CancellationToken,
        clipboard: &'a mut TestClipboard,
        apply_worker: &'a mut WorkerGuard,
    ) -> RuntimeEffects<'a, TestClipboard> {
        RuntimeEffects {
            tool: Tool::Terraform,
            root: Path::new("/project"),
            display_root: Path::new("/project"),
            global_arguments: &[],
            apply_arguments: &[],
            sender,
            plan_path: Path::new("/project/review.tfplan"),
            cancellation,
            clipboard,
            apply_worker,
            history: None,
        }
    }

    #[derive(Default)]
    struct ScreenViews {
        execution: execution::ExecutionViewState,
        review: plan_review::PlanReviewViewState,
        confirmation: plan_review::ApplyConfirmationViewState,
    }

    impl ScreenViews {
        fn handle_key(
            &mut self,
            terminal: &Terminal<TestBackend>,
            state: &SessionState,
            code: KeyCode,
            modifiers: KeyModifiers,
        ) -> Result<Option<Action>, <TestBackend as Backend>::Error> {
            handle_key_event(
                terminal,
                state,
                &mut self.execution,
                &mut self.review,
                &mut self.confirmation,
                KeyEvent::new(code, modifiers),
            )
        }
    }

    fn render_apply_to_text(
        state: &mut SessionState,
        terminal: &mut Terminal<TestBackend>,
        views: &ScreenViews,
        now: Instant,
    ) -> String {
        let mut dirty = true;
        assert!(
            draw_if_needed(
                state,
                terminal,
                views.execution,
                &views.review,
                &views.confirmation,
                &mut dirty,
                now,
            )
            .expect("apply should render")
        );
        terminal_text(terminal)
    }

    fn review_plan() -> PlanReview {
        PlanReview::new(
            PathBuf::from("/project"),
            "default".to_owned(),
            plan_document("No changes.\n".to_owned()),
            Plan::empty(),
            PlanMetadata::new(false),
            Vec::new(),
        )
    }

    fn confirmation_state() -> SessionState {
        SessionState::Review(Box::new(session::test_support::apply_confirmation_session(
            review_plan(),
        )))
    }

    fn apply_state(started_at: Instant, status: Option<ApplyStatus>) -> SessionState {
        let mut execution =
            ExecutionState::applying(started_at, ExecutionContext::loading("loading..."));
        if let Some(status) = status {
            execution.finish_apply(status, None, None, started_at);
        }
        SessionState::Apply(Box::new(execution))
    }

    fn copy_plan_text() -> String {
        let mut lines = vec![
            "Terraform will perform the following actions:".to_owned(),
            String::new(),
            "xcopy body marker remains visible after the notification and has a long suffix for horizontal scrolling".to_owned(),
            "xterraform_data.api contains the search highlight that must return after the copy flash".to_owned(),
        ];
        lines.extend((0..40).map(|index| format!("synthetic plan page line {index}")));
        format!("{}\n", lines.join("\n"))
    }
}

#[cfg(test)]
pub(super) mod test_support {
    use ratatui::{Terminal, backend::TestBackend};

    pub(crate) fn terminal_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        let area = buffer.area();
        let mut text = String::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                text.push_str(buffer.cell((x, y)).expect("test cell").symbol());
            }
            text.push('\n');
        }
        text
    }
}
