use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crossterm::event::{self, Event, KeyEvent};
use ratatui::{Terminal, backend::Backend, layout::Rect};

use crate::{
    app::{
        copy::CopyResult,
        environments::{
            Environment, EnvironmentAvailability, EnvironmentIdentity, EnvironmentSession,
            PlanResult,
        },
        execution::{
            ApplyStatus, EventStream, ExecutionContext, ExecutionEvent, ExecutionEventKind,
            ExecutionLogLine, ExecutionPhase, ExecutionState, ExecutionTargetSpec, ResourceAction,
            ResourceEvent, ResourceEventKind, Tool,
        },
        plan::{Plan, PlanAction, PlanValue, ResourceChange, ResourceChangeKind, ResourceMode},
        review::{PlanBlock, PlanBlockKind, PlanDocument, PlanLineKind, PlanMetadata, PlanReview},
        session::{Action, Effect, ReviewSessionState, SessionState},
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

const SYNTHETIC_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(super) fn run_synthetic() -> io::Result<()> {
    if std::env::args().any(|argument| argument == "--environments") {
        return run_synthetic_environments();
    }
    run_synthetic_session(SyntheticSession::new(
        SessionState::Review(Box::new(synthetic_review())),
        execution::ExecutionViewState::default(),
        None,
    ))
}

pub(super) fn run_synthetic_execution() -> io::Result<()> {
    run_synthetic_session(synthetic_execution_session(Instant::now()))
}

fn run_synthetic_session(mut session: SyntheticSession) -> io::Result<()> {
    ratatui::run(|terminal| {
        let mut event = None;
        loop {
            if session.step(terminal, event.take().as_ref(), Instant::now())? {
                return Ok(());
            }
            if event::poll(session.poll_timeout(Instant::now()))? {
                event = Some(event::read()?);
            }
        }
    })
}

// Each step mirrors one pass of the connected loop; a timer stands in for the apply worker.
struct SyntheticSession {
    state: SessionState,
    review_view: plan_review::PlanReviewViewState,
    confirmation_view: plan_review::ApplyConfirmationViewState,
    execution_view: execution::ExecutionViewState,
    quit_confirmation: bool,
    complete_apply_at: Option<Instant>,
    dirty: bool,
}

impl SyntheticSession {
    fn new(
        state: SessionState,
        execution_view: execution::ExecutionViewState,
        complete_apply_at: Option<Instant>,
    ) -> Self {
        Self {
            state,
            review_view: plan_review::PlanReviewViewState::default(),
            confirmation_view: plan_review::ApplyConfirmationViewState::default(),
            execution_view,
            quit_confirmation: false,
            complete_apply_at,
            dirty: true,
        }
    }

    fn step<B: Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        event: Option<&Event>,
        now: Instant,
    ) -> Result<bool, B::Error> {
        if let Some(event) = event
            && let Some(action) = self.handle_event(event, terminal)?
            && apply_synthetic_action(
                &mut self.state,
                action,
                &mut self.execution_view,
                &mut self.complete_apply_at,
                now,
            )
        {
            return Ok(true);
        }
        if self.complete_apply_at.is_some_and(|at| now >= at) {
            finish_synthetic_apply(
                &mut self.state,
                &mut self.execution_view,
                ApplyStatus::Succeeded,
                now,
            );
            self.complete_apply_at = None;
            self.dirty = true;
        }
        super::event_loop::draw_if_needed_with_quit_confirmation(
            &mut self.state,
            terminal,
            self.execution_view,
            &self.review_view,
            &self.confirmation_view,
            &mut self.dirty,
            now,
            self.quit_confirmation,
        )?;
        Ok(false)
    }

    fn poll_timeout(&self, now: Instant) -> Duration {
        self.complete_apply_at
            .map_or(SYNTHETIC_POLL_INTERVAL, |at| {
                at.saturating_duration_since(now)
                    .min(SYNTHETIC_POLL_INTERVAL)
            })
    }

    fn handle_event<B: Backend>(
        &mut self,
        event: &Event,
        terminal: &Terminal<B>,
    ) -> Result<Option<Action>, B::Error> {
        let key = match *event {
            Event::Resize(width, height) => {
                self.dirty = true;
                super::event_loop::reconcile_resize(
                    &self.state,
                    &mut self.review_view,
                    Rect::new(0, 0, width, height),
                    self.quit_confirmation,
                );
                return Ok(None);
            }
            Event::Key(key) if key.is_press() => key,
            _ => return Ok(None),
        };
        self.dirty = true;

        let mut confirmed_quit = false;
        let action = if self.quit_confirmation {
            match quit_confirmation_key_to_input(key) {
                QuitConfirmationInput::Confirm => {
                    self.quit_confirmation = false;
                    confirmed_quit = true;
                    Some(Action::Quit)
                }
                QuitConfirmationInput::Cancel => {
                    self.quit_confirmation = false;
                    None
                }
                QuitConfirmationInput::Consume => None,
                QuitConfirmationInput::Forward(key) => {
                    self.quit_confirmation = false;
                    self.handle_key(terminal, key)?
                }
            }
        } else {
            self.handle_key(terminal, key)?
        };
        let Some(action) = action else {
            return Ok(None);
        };
        if matches!(action, Action::Quit) && !confirmed_quit {
            self.quit_confirmation = true;
            return Ok(None);
        }
        Ok(Some(action))
    }

    fn handle_key<B: Backend>(
        &mut self,
        terminal: &Terminal<B>,
        key: KeyEvent,
    ) -> Result<Option<Action>, B::Error> {
        super::event_loop::handle_key_event(
            terminal,
            &self.state,
            &mut self.execution_view,
            &mut self.review_view,
            &mut self.confirmation_view,
            key,
        )
    }
}

// Stands in for the runtime effects: the synthetic session never starts Terraform, writes the
// clipboard, or saves apply history.
fn apply_synthetic_action(
    state: &mut SessionState,
    action: Action,
    execution_view: &mut execution::ExecutionViewState,
    complete_apply_at: &mut Option<Instant>,
    now: Instant,
) -> bool {
    match super::event_loop::update_session(state, action, execution_view, now) {
        Some(Effect::StartApply) => {
            record_synthetic_apply_events(state, execution_view, now);
            *complete_apply_at = Some(now + Duration::from_millis(250));
            false
        }
        Some(Effect::CancelExecution) => {
            finish_synthetic_apply(state, execution_view, ApplyStatus::Interrupted, now);
            *complete_apply_at = None;
            false
        }
        Some(Effect::WriteClipboard(effect)) => apply_synthetic_action(
            state,
            Action::CopyCompleted {
                target: effect.target(),
                result: CopyResult::Written,
            },
            execution_view,
            complete_apply_at,
            now,
        ),
        Some(Effect::Finish(_)) => true,
        Some(Effect::PersistHistory(_)) | None => false,
    }
}

fn synthetic_review() -> ReviewSessionState {
    let plan = PlanReview::new(
        PathBuf::from("/workspace/infra/prod"),
        "default".to_owned(),
        PlanDocument::with_blocks_and_line_kinds(
            "Terraform will perform the following actions:\n\n  # terraform_data.example will be updated in-place\n  ~ resource \"terraform_data.example\" {\n      ~ input = \"before\" -> \"after\"\n      note = \"searchable synthetic value\"\n    }\n\n  # terraform_data.cache will be created\n  + resource \"terraform_data\" \"cache\" {\n      input = \"cache\"\n    }\n\n  # terraform_data.old will be destroyed\n  - resource \"terraform_data\" \"old\" {}\n\nPlan: 1 to add, 1 to change, 1 to destroy.\n"
                .to_owned(),
            vec![
                PlanBlock::new(0..2, PlanBlockKind::Common),
                PlanBlock::with_addresses(
                    2..7,
                    PlanBlockKind::Resource,
                    vec!["terraform_data.example".to_owned()],
                ),
                PlanBlock::new(7..8, PlanBlockKind::Common),
                PlanBlock::with_addresses(
                    8..12,
                    PlanBlockKind::Resource,
                    vec!["terraform_data.cache".to_owned()],
                ),
                PlanBlock::new(12..13, PlanBlockKind::Common),
                PlanBlock::with_addresses(
                    13..15,
                    PlanBlockKind::Resource,
                    vec!["terraform_data.old".to_owned()],
                ),
                PlanBlock::new(15..17, PlanBlockKind::Common),
            ],
            vec![
                PlanLineKind::Intro,
                PlanLineKind::Intro,
                PlanLineKind::Note,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Intro,
                PlanLineKind::Note,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Body,
                PlanLineKind::Intro,
                PlanLineKind::Note,
                PlanLineKind::Body,
                PlanLineKind::Intro,
                PlanLineKind::Summary,
            ],
        ),
        Plan {
            value_addresses: BTreeSet::new(),
            resource_changes: vec![
                synthetic_change(
                    "terraform_data.example",
                    ResourceChangeKind::Update,
                    vec![PlanAction::Update],
                    "before",
                    "after",
                ),
                synthetic_change(
                    "terraform_data.cache",
                    ResourceChangeKind::Create,
                    vec![PlanAction::Create],
                    "",
                    "cache",
                ),
                synthetic_change(
                    "terraform_data.old",
                    ResourceChangeKind::Delete,
                    vec![PlanAction::Delete],
                    "old",
                    "",
                ),
            ],
            unsupported_changes: Vec::new(),
            output_changes: Vec::new(),
            drifted_resources: Vec::new(),
        },
        PlanMetadata::new(true),
        Vec::new(),
    )
    .with_context(
        ExecutionContext::loading("/workspace/infra/prod")
            .with_launch_root("/workspace")
            .with_workspace("default")
            .with_tool_version(Tool::Terraform, "1.9.0"),
    );
    ReviewSessionState::new(plan)
}

fn synthetic_change(
    address: &str,
    kind: ResourceChangeKind,
    actions: Vec<PlanAction>,
    before: &str,
    after: &str,
) -> ResourceChange {
    ResourceChange {
        address: address.to_owned(),
        provider: None,
        resource_type: Some("terraform_data".to_owned()),
        resource_name: Some(address.rsplit('.').next().unwrap_or(address).to_owned()),
        mode: ResourceMode::Managed,
        actions,
        kind,
        before: Some(PlanValue::Object(BTreeMap::from([(
            "input".to_owned(),
            PlanValue::String(before.to_owned()),
        )]))),
        after: Some(PlanValue::Object(BTreeMap::from([(
            "input".to_owned(),
            PlanValue::String(after.to_owned()),
        )]))),
        before_sensitive: None,
        after_sensitive: None,
        after_unknown: None,
        replace_paths: None,
        action_reason: None,
        previous_address: None,
        importing: None,
    }
}

fn finish_synthetic_apply(
    state: &mut SessionState,
    execution_view: &mut execution::ExecutionViewState,
    status: ApplyStatus,
    now: Instant,
) {
    let summary_line = matches!(status, ApplyStatus::Succeeded)
        .then(|| "Apply complete! Resources: 1 added, 1 changed, 1 destroyed.".to_owned());
    let _ = super::event_loop::update_session(
        state,
        Action::ApplyCompleted {
            status,
            summary_line,
        },
        execution_view,
        now,
    );
}

fn record_synthetic_apply_events(
    state: &mut SessionState,
    execution_view: &mut execution::ExecutionViewState,
    now: Instant,
) {
    for kind in synthetic_apply_events() {
        let _ = super::event_loop::update_session(
            state,
            Action::ApplyWorkerEvent(ExecutionEvent {
                received_at: now,
                kind,
            }),
            execution_view,
            now,
        );
    }
}

fn resource_event(
    address: &str,
    kind: ResourceEventKind,
    action: ResourceAction,
    message: &str,
) -> ExecutionEventKind {
    ExecutionEventKind::Resource(ResourceEvent {
        address: address.to_owned(),
        kind,
        action: Some(action),
        message: Some(message.to_owned()),
    })
}

fn run_synthetic_environments() -> io::Result<()> {
    let names = if std::env::args().any(|argument| argument == "--many-environments") {
        (0..12)
            .map(|index| format!("env-{index:02}"))
            .collect::<Vec<_>>()
    } else {
        ["dev", "prod", "stg"].map(str::to_owned).to_vec()
    };
    let mut state = EnvironmentSession::new(
        names
            .into_iter()
            .map(|name| Environment {
                tool: Tool::Terraform,
                availability: EnvironmentAvailability::Available(EnvironmentIdentity {
                    directory: PathBuf::from(format!("/example/{name}")),
                    workspace: "default".to_owned(),
                }),
            })
            .collect(),
        true,
    );
    let mut view = EnvironmentView::default();
    let mut next = Instant::now() + Duration::from_millis(500);
    let mut failed_once = false;
    let mut running = state.start_next();
    ratatui::run(|terminal| {
        loop {
            if Instant::now() >= next {
                if let Some(index) = running.take() {
                    let result = if index == 1 && !failed_once {
                        failed_once = true;
                        PlanResult::Error(
                            "Synthetic missing variable. Press r to retry this environment."
                                .to_owned(),
                        )
                    } else {
                        PlanResult::Ready {
                            review: Box::new(synthetic_environment_review(
                                state.plans()[index].directory(),
                                if index == 1 { 200 } else { 20 },
                            )),
                            changed: true,
                        }
                    };
                    state.complete(index, result, Vec::new());
                }
                running = state.start_next();
                next = Instant::now() + Duration::from_millis(750);
            }
            terminal.draw(|frame| view.render(frame, &state))?;
            if !event::poll(Duration::from_millis(100))? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            match view.handle_key(key, terminal.size()?, &state) {
                Some(EnvironmentInput::Quit | EnvironmentInput::Interrupt) => break,
                Some(EnvironmentInput::Retry(index)) => {
                    state.retry(index);
                }
                Some(EnvironmentInput::Review(index, action)) => {
                    state.update_review(index, *action, Instant::now());
                }
                None => {}
            }
        }
        Ok(())
    })
}

fn synthetic_environment_review(directory: &Path, count: usize) -> PlanReview {
    let changes: Vec<_> = (0..count)
        .map(|index| {
            synthetic_change(
                &format!("terraform_data.server[{index}]"),
                ResourceChangeKind::Update,
                vec![PlanAction::Update],
                "before",
                "after",
            )
        })
        .collect();
    let mut lines = vec![
        "Terraform will perform the following actions:".to_owned(),
        String::new(),
    ];
    let mut blocks = vec![PlanBlock::new(0..2, PlanBlockKind::Common)];
    for change in &changes {
        let start = lines.len();
        lines.extend([
            format!("# {} will be updated in-place", change.address),
            "~ input = before -> after".to_owned(),
            String::new(),
        ]);
        blocks.push(PlanBlock::with_addresses(
            start..lines.len(),
            PlanBlockKind::Resource,
            vec![change.address.clone()],
        ));
    }
    PlanReview::new(
        directory.to_owned(),
        "default".to_owned(),
        PlanDocument::with_blocks_and_line_kinds(lines.join("\n"), blocks, Vec::new()),
        Plan {
            resource_changes: changes,
            ..Plan::empty()
        },
        PlanMetadata::new(true),
        Vec::new(),
    )
    .with_apply_allowed(false)
    .with_apply_entry(false)
    .with_context(
        ExecutionContext::loading(directory)
            .with_workspace("default")
            .with_tool_version(Tool::Terraform, "1.9.0"),
    )
}

fn synthetic_execution_session(started: Instant) -> SyntheticSession {
    let mut execution = ExecutionState::applying_with_targets(
        started,
        ExecutionContext::loading("infra/prod").with_workspace("default"),
        synthetic_apply_targets(),
        Vec::new(),
    );
    execution.record(ExecutionEvent {
        received_at: started,
        kind: ExecutionEventKind::Phase(ExecutionPhase::Planning),
    });
    for kind in synthetic_apply_events() {
        execution.record(ExecutionEvent {
            received_at: started,
            kind,
        });
    }
    let mut execution_view = execution::ExecutionViewState::default();
    execution_view.initialize_target_selection(&execution.progress().display_target_indices(false));
    SyntheticSession::new(
        SessionState::Apply(Box::new(execution)),
        execution_view,
        Some(started + Duration::from_millis(750)),
    )
}

fn synthetic_apply_targets() -> Vec<ExecutionTargetSpec> {
    vec![
        ExecutionTargetSpec {
            address: "terraform_data.example".to_owned(),
            actions: vec![PlanAction::Update],
        },
        ExecutionTargetSpec {
            address: "terraform_data.cache".to_owned(),
            actions: vec![PlanAction::Create],
        },
        ExecutionTargetSpec {
            address: "terraform_data.old".to_owned(),
            actions: vec![PlanAction::Delete],
        },
    ]
}

fn synthetic_apply_events() -> Vec<ExecutionEventKind> {
    vec![
        ExecutionEventKind::Log(ExecutionLogLine {
            stream: EventStream::Stdout,
            text: "Applying saved plan...".to_owned(),
        }),
        resource_event(
            "terraform_data.example",
            ResourceEventKind::ApplyStart,
            ResourceAction::Update,
            "terraform_data.example: Modifying...",
        ),
        resource_event(
            "terraform_data.example",
            ResourceEventKind::ApplyComplete,
            ResourceAction::Update,
            "terraform_data.example: Modifications complete",
        ),
        resource_event(
            "terraform_data.cache",
            ResourceEventKind::ApplyStart,
            ResourceAction::Create,
            "terraform_data.cache: Creating...",
        ),
        resource_event(
            "terraform_data.cache",
            ResourceEventKind::ApplyComplete,
            ResourceAction::Create,
            "terraform_data.cache: Creation complete",
        ),
        resource_event(
            "terraform_data.old",
            ResourceEventKind::ApplyStart,
            ResourceAction::Delete,
            "terraform_data.old: Destroying...",
        ),
        resource_event(
            "terraform_data.old",
            ResourceEventKind::ApplyComplete,
            ResourceAction::Delete,
            "terraform_data.old: Destruction complete",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::backend::TestBackend;

    use super::*;
    use crate::{app::execution::ExecutionStage, runtime::event_loop::test_support::terminal_text};

    impl SyntheticSession {
        fn review() -> Self {
            Self::new(
                SessionState::Review(Box::new(synthetic_review())),
                execution::ExecutionViewState::default(),
                None,
            )
        }

        fn idle(&mut self, terminal: &mut Terminal<TestBackend>, now: Instant) -> String {
            assert!(
                !self
                    .step(terminal, None, now)
                    .expect("synthetic step should render")
            );
            terminal_text(terminal)
        }

        fn send(
            &mut self,
            terminal: &mut Terminal<TestBackend>,
            key: KeyEvent,
            now: Instant,
        ) -> bool {
            self.step(terminal, Some(&Event::Key(key)), now)
                .expect("synthetic input should be handled")
        }

        fn press(
            &mut self,
            terminal: &mut Terminal<TestBackend>,
            code: KeyCode,
            now: Instant,
        ) -> bool {
            self.send(terminal, KeyEvent::new(code, KeyModifiers::NONE), now)
        }

        fn type_confirmation(&mut self, terminal: &mut Terminal<TestBackend>, now: Instant) {
            let expected = self
                .state
                .apply_confirmation()
                .expect("apply confirmation should be open")
                .review()
                .confirmation_input();
            for character in expected.chars() {
                assert!(!self.press(terminal, KeyCode::Char(character), now));
            }
        }
    }

    fn terminal(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).expect("test terminal")
    }

    mod review {
        use super::*;

        #[test]
        fn raw_copy_shows_the_synthetic_copy_notice() {
            let now = Instant::now();
            let mut terminal = terminal(100, 30);
            let mut session = SyntheticSession::review();

            assert!(!session.press(&mut terminal, KeyCode::Char('y'), now));

            let text = terminal_text(&terminal);
            assert!(text.contains("Copied."), "{text}");
        }

        #[test]
        fn overview_footer_hints_return_when_the_copy_notice_expires() {
            let now = Instant::now();
            let notice_expired_at = now + Duration::from_secs(3);
            let mut never_copied_terminal = terminal(40, 16);
            let mut never_copied = SyntheticSession::review();
            assert!(!never_copied.press(&mut never_copied_terminal, KeyCode::Char('s'), now));
            let never_copied_text =
                never_copied.idle(&mut never_copied_terminal, notice_expired_at);
            let mut terminal = terminal(40, 16);
            let mut session = SyntheticSession::review();
            assert!(!session.press(&mut terminal, KeyCode::Char('s'), now));
            assert!(!session.press(&mut terminal, KeyCode::Char('y'), now));
            assert_ne!(terminal_text(&terminal), never_copied_text);

            let expired = session.idle(&mut terminal, notice_expired_at);

            assert_eq!(expired, never_copied_text);
        }

        #[test]
        fn overview_quit_is_confirmed_before_finishing() {
            let now = Instant::now();
            let mut terminal = terminal(100, 30);
            let mut session = SyntheticSession::review();
            assert!(!session.press(&mut terminal, KeyCode::Char('s'), now));
            assert!(session.state.overview().is_some());

            assert!(!session.press(&mut terminal, KeyCode::Char('q'), now));

            let text = terminal_text(&terminal);
            assert!(text.contains("Quit Terraleph?"), "{text}");
            assert!(session.press(&mut terminal, KeyCode::Enter, now));
        }

        #[test]
        fn narrow_confirmation_does_not_start_the_synthetic_apply() {
            let now = Instant::now();
            let mut wide = terminal(100, 30);
            let mut narrow = terminal(20, 5);
            let mut session = SyntheticSession::review();
            assert!(!session.press(&mut wide, KeyCode::Char('a'), now));

            session.type_confirmation(&mut narrow, now);
            assert!(!session.press(&mut narrow, KeyCode::Enter, now));

            assert!(session.state.apply_confirmation().is_some());
            assert_eq!(session.confirmation_view.input(), "");
            assert_eq!(session.complete_apply_at, None);
        }

        #[test]
        fn confirmed_apply_records_synthetic_events_until_the_timer() {
            let now = Instant::now();
            let mut terminal = terminal(100, 30);
            let mut session = SyntheticSession::review();
            assert!(!session.press(&mut terminal, KeyCode::Char('a'), now));
            session.type_confirmation(&mut terminal, now);

            assert!(!session.press(&mut terminal, KeyCode::Enter, now));

            let apply = session.state.apply().expect("synthetic apply should start");
            assert_eq!(apply.stage(), ExecutionStage::Applying);
            assert_eq!(apply.progress().targets().len(), 3);
            assert!(session.complete_apply_at.is_some());
        }

        #[test]
        fn cancelling_the_synthetic_apply_interrupts_it() {
            let now = Instant::now();
            let mut terminal = terminal(100, 30);
            let mut session = SyntheticSession::review();
            assert!(!session.press(&mut terminal, KeyCode::Char('a'), now));
            session.type_confirmation(&mut terminal, now);
            assert!(!session.press(&mut terminal, KeyCode::Enter, now));

            assert!(!session.send(
                &mut terminal,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                now,
            ));

            let apply = session
                .state
                .apply()
                .expect("synthetic apply should remain");
            assert_eq!(apply.stage(), ExecutionStage::ApplyInterrupted);
            assert_eq!(session.complete_apply_at, None);
        }
    }

    mod execution_example {
        use super::*;

        #[test]
        fn timed_apply_finishes_without_input() {
            let started = Instant::now();
            let mut terminal = terminal(100, 30);
            let mut session = synthetic_execution_session(started);
            let running = session.idle(&mut terminal, started);
            assert!(!running.contains("Apply complete"), "{running}");
            assert_eq!(
                session.poll_timeout(started + Duration::from_millis(700)),
                Duration::from_millis(50)
            );

            let finished = session.idle(&mut terminal, started + Duration::from_millis(750));

            let apply = session.state.apply().expect("apply should remain");
            assert_eq!(apply.stage(), ExecutionStage::ApplySucceeded);
            assert_eq!(session.complete_apply_at, None);
            assert!(finished.contains("Apply complete"), "{finished}");
        }

        #[test]
        fn finished_apply_asks_before_quitting() {
            let started = Instant::now();
            let finished_at = started + Duration::from_millis(750);
            let mut terminal = terminal(100, 30);
            let mut session = synthetic_execution_session(started);
            session.idle(&mut terminal, finished_at);
            assert!(!session.press(&mut terminal, KeyCode::Esc, finished_at));

            assert!(!session.press(&mut terminal, KeyCode::Char('q'), finished_at));

            let text = terminal_text(&terminal);
            assert!(text.contains("Quit Terraleph?"), "{text}");
            assert!(session.press(&mut terminal, KeyCode::Enter, finished_at));
        }
    }
}
