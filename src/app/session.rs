use std::fmt::{Debug, Formatter};
use std::time::Instant;

use super::{
    copy::{self, CopyEffect, CopyFeedback, CopyResult, CopyTarget},
    environments::overview::SingleEnvironmentOverview,
    execution::{ApplyStatus, ExecutionEvent, ExecutionStage, ExecutionState, SuccessfulTarget},
    plan::PlanSummary,
    review::{PlanReview, PlanReviewMessage},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionOutcome {
    /// `changes` is `None` when the reviewed plan changes nothing.
    Reviewed {
        changes: Option<ReviewedChanges>,
    },
    NoChanges,
    ApplyCanceled,
    Applied {
        status: ApplyStatus,
        summary_line: Option<String>,
    },
    Failed(ExecutionStage),
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReviewedChanges {
    pub(crate) resources: PlanSummary,
    pub(crate) outputs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SessionState {
    Execution(Box<ExecutionState>),
    Review(Box<ReviewSessionState>),
    Apply(Box<ExecutionState>),
}

/// The reviewed plan, owned once while the user moves between its screens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReviewSessionState {
    review: PlanReview,
    // The review's plan, relations, and provider schemas do not change while it is reviewed,
    // so the Overview model lives exactly as long as the review and needs no invalidation.
    prepared_overview: SingleEnvironmentOverview,
    screen: ReviewScreen,
    copy_feedback: CopyFeedback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ReviewScreen {
    Raw(RawReviewScreen),
    Overview,
    ApplyConfirmation { return_to: RawReviewScreen },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RawReviewScreen {
    from_overview: bool,
    restored_search_query: Option<String>,
}

impl ReviewSessionState {
    #[must_use]
    pub(crate) fn new(review: PlanReview) -> Self {
        Self {
            prepared_overview: SingleEnvironmentOverview::new(&review),
            review,
            screen: ReviewScreen::Raw(RawReviewScreen::default()),
            copy_feedback: CopyFeedback::default(),
        }
    }

    #[must_use]
    pub(crate) const fn review(&self) -> &PlanReview {
        &self.review
    }

    #[must_use]
    pub(crate) const fn prepared_overview(&self) -> &SingleEnvironmentOverview {
        &self.prepared_overview
    }

    #[must_use]
    pub(crate) const fn screen(&self) -> &ReviewScreen {
        &self.screen
    }

    #[must_use]
    pub(crate) const fn is_from_overview(&self) -> bool {
        matches!(
            &self.screen,
            ReviewScreen::Raw(RawReviewScreen {
                from_overview: true,
                ..
            })
        )
    }

    #[must_use]
    pub(crate) const fn copy_feedback(&self) -> &CopyFeedback {
        &self.copy_feedback
    }

    // The apply confirmation offers no copy, so it keeps no copy notice either.
    const fn shows_copy_feedback(&self) -> bool {
        !matches!(self.screen, ReviewScreen::ApplyConfirmation { .. })
    }

    // Each screen starts without the previous screen's copy notice.
    fn show(&mut self, screen: ReviewScreen) {
        self.screen = screen;
        self.copy_feedback = CopyFeedback::default();
    }

    fn open_overview(&mut self) {
        let ReviewScreen::Raw(raw) = &mut self.screen else {
            return;
        };
        if let Some(query) = raw.restored_search_query.take() {
            self.review.set_search_query(query);
        }
        self.show(ReviewScreen::Overview);
    }

    fn open_raw_from_overview(&mut self) {
        let ReviewScreen::Overview = self.screen else {
            return;
        };
        let restored_search_query = self.review.search_query().to_owned();
        self.review.set_search_query(String::new());
        self.show(ReviewScreen::Raw(RawReviewScreen {
            from_overview: true,
            restored_search_query: Some(restored_search_query),
        }));
    }

    fn open_apply_confirmation(&mut self) {
        let ReviewScreen::Raw(raw) = &mut self.screen else {
            return;
        };
        let return_to = std::mem::take(raw);
        self.show(ReviewScreen::ApplyConfirmation { return_to });
    }

    fn cancel_apply_confirmation(&mut self) {
        let ReviewScreen::ApplyConfirmation { return_to } = &mut self.screen else {
            return;
        };
        let raw = std::mem::take(return_to);
        self.show(ReviewScreen::Raw(raw));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "review completion carries the complete plan into the session"
)]
pub(crate) enum Action {
    RequestCancellation,
    WorkerEvent(ExecutionEvent),
    ApplyWorkerEvent(ExecutionEvent),
    ReviewCompleted(PlanReview),
    ReviewFailed {
        message: String,
        interrupted: bool,
    },
    ReviewSearchChanged(String),
    OpenOverview,
    OpenReviewFromOverview,
    ReturnToOverview,
    OpenApplyConfirmation,
    ConfirmApply(String),
    CancelApply,
    ApplyCompleted {
        status: ApplyStatus,
        summary_line: Option<String>,
    },
    ApplyFailed {
        message: String,
    },
    WorkerDisconnected,
    Copy(CopyTarget),
    CopyCompleted {
        target: CopyTarget,
        result: CopyResult,
    },
    Quit,
}

pub(crate) enum Effect {
    CancelExecution,
    StartApply,
    PersistHistory(Vec<SuccessfulTarget>),
    WriteClipboard(CopyEffect),
    Finish(SessionOutcome),
}

impl Debug for Effect {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CancelExecution => formatter.write_str("CancelExecution"),
            Self::StartApply => formatter.write_str("StartApply"),
            Self::PersistHistory(_) => formatter.write_str("PersistHistory(<redacted>)"),
            Self::WriteClipboard(effect) => formatter
                .debug_tuple("WriteClipboard")
                .field(&effect.target())
                .field(&"<redacted>")
                .finish(),
            Self::Finish(outcome) => formatter.debug_tuple("Finish").field(outcome).finish(),
        }
    }
}

impl SessionState {
    #[must_use]
    pub(crate) fn new(execution: ExecutionState) -> Self {
        Self::Execution(Box::new(execution))
    }

    #[must_use]
    pub(crate) const fn copy_feedback(&self) -> Option<&CopyFeedback> {
        match self {
            Self::Execution(state) | Self::Apply(state) => Some(state.copy_feedback()),
            Self::Review(state) if state.shows_copy_feedback() => Some(state.copy_feedback()),
            Self::Review(_) => None,
        }
    }

    pub(crate) fn copy_feedback_mut(&mut self) -> Option<&mut CopyFeedback> {
        match self {
            Self::Execution(state) | Self::Apply(state) => Some(state.copy_feedback_mut()),
            Self::Review(state) if state.shows_copy_feedback() => Some(&mut state.copy_feedback),
            Self::Review(_) => None,
        }
    }

    #[must_use]
    pub(crate) const fn execution(&self) -> Option<&ExecutionState> {
        match self {
            Self::Execution(state) => Some(state),
            Self::Review(_) | Self::Apply(_) => None,
        }
    }

    /// The review session while it shows the raw plan.
    #[must_use]
    pub(crate) const fn review(&self) -> Option<&ReviewSessionState> {
        match self {
            Self::Review(state) if matches!(state.screen, ReviewScreen::Raw(_)) => Some(state),
            Self::Execution(_) | Self::Review(_) | Self::Apply(_) => None,
        }
    }

    /// The review session while it shows the overview.
    #[must_use]
    pub(crate) const fn overview(&self) -> Option<&ReviewSessionState> {
        match self {
            Self::Review(state) if matches!(state.screen, ReviewScreen::Overview) => Some(state),
            Self::Execution(_) | Self::Review(_) | Self::Apply(_) => None,
        }
    }

    /// The review session while it asks for apply confirmation.
    #[must_use]
    pub(crate) const fn apply_confirmation(&self) -> Option<&ReviewSessionState> {
        match self {
            Self::Review(state)
                if matches!(state.screen, ReviewScreen::ApplyConfirmation { .. }) =>
            {
                Some(state)
            }
            Self::Execution(_) | Self::Review(_) | Self::Apply(_) => None,
        }
    }

    #[must_use]
    pub(crate) const fn apply(&self) -> Option<&ExecutionState> {
        match self {
            Self::Apply(state) => Some(state),
            Self::Execution(_) | Self::Review(_) => None,
        }
    }

    #[must_use]
    pub(crate) fn from_message(message: PlanReviewMessage) -> Action {
        match message {
            PlanReviewMessage::Event(event) => Action::WorkerEvent(event),
            PlanReviewMessage::ApplyEvent(event) => Action::ApplyWorkerEvent(event),
            PlanReviewMessage::Completed(review) => Action::ReviewCompleted(review),
            PlanReviewMessage::Failed {
                message,
                interrupted,
            } => Action::ReviewFailed {
                message,
                interrupted,
            },
            PlanReviewMessage::ApplyCompleted {
                status,
                summary_line,
            } => Action::ApplyCompleted {
                status,
                summary_line,
            },
            PlanReviewMessage::ApplyFailed { message } => Action::ApplyFailed { message },
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the session reducer keeps all user-visible state transitions together"
)]
pub(crate) fn update(state: &mut SessionState, action: Action, now: Instant) -> Option<Effect> {
    match action {
        Action::RequestCancellation => {
            let (SessionState::Execution(execution) | SessionState::Apply(execution)) = state
            else {
                return None;
            };
            if execution.cancellation_requested() {
                return None;
            }
            execution.request_cancellation();
            Some(Effect::CancelExecution)
        }
        Action::WorkerEvent(event) => {
            if let SessionState::Execution(execution) = state {
                execution.record(event);
            }
            None
        }
        Action::ApplyWorkerEvent(event) => {
            if let SessionState::Apply(execution) = state {
                execution.record(event);
            }
            None
        }
        Action::ReviewCompleted(review) => {
            let SessionState::Execution(execution) = state else {
                return None;
            };
            if execution.cancellation_requested() {
                return Some(Effect::Finish(SessionOutcome::Interrupted));
            }
            if review.apply_entry() && !review.metadata().applyable() {
                return Some(Effect::Finish(SessionOutcome::NoChanges));
            }
            *state = SessionState::Review(Box::new(ReviewSessionState::new(review)));
            None
        }
        Action::ReviewFailed {
            message,
            interrupted,
        } => {
            let SessionState::Execution(execution) = state else {
                return None;
            };
            if interrupted || execution.cancellation_requested() {
                return Some(Effect::Finish(SessionOutcome::Interrupted));
            }
            execution.fail(message, now);
            None
        }
        Action::ReviewSearchChanged(query) => {
            if let SessionState::Review(review) = state
                && matches!(review.screen, ReviewScreen::Raw(_))
            {
                review.review.set_search_query(query);
            }
            None
        }
        Action::OpenOverview => {
            if let SessionState::Review(review) = state {
                review.open_overview();
            }
            None
        }
        Action::OpenReviewFromOverview => {
            if let SessionState::Review(review) = state {
                review.open_raw_from_overview();
            }
            None
        }
        Action::ReturnToOverview => {
            if let SessionState::Review(review) = state
                && review.is_from_overview()
            {
                review.open_overview();
            }
            None
        }
        Action::OpenApplyConfirmation => {
            if let SessionState::Review(review) = state
                && review.review.metadata().applyable()
            {
                review.open_apply_confirmation();
            }
            None
        }
        Action::ConfirmApply(input) => {
            let SessionState::Review(confirmation) = state else {
                return None;
            };
            if !matches!(confirmation.screen, ReviewScreen::ApplyConfirmation { .. })
                || input != confirmation.review.confirmation_input()
            {
                return None;
            }
            *state = SessionState::Apply(Box::new(ExecutionState::applying_with_previous(
                now,
                confirmation.review.context().clone(),
                confirmation.review.apply_targets(),
                confirmation.review.metadata().sensitive_values().to_vec(),
                confirmation.review.previous_durations(),
            )));
            Some(Effect::StartApply)
        }
        Action::CancelApply => {
            if let SessionState::Review(review) = state {
                review.cancel_apply_confirmation();
            }
            None
        }
        Action::ApplyCompleted {
            status,
            summary_line,
        } => {
            let SessionState::Apply(execution) = state else {
                return None;
            };
            execution.finish_apply(status, summary_line, None, now);
            Some(Effect::PersistHistory(execution.successful_history()))
        }
        Action::ApplyFailed { message } => {
            let SessionState::Apply(execution) = state else {
                return None;
            };
            execution.finish_apply(ApplyStatus::Failed, None, Some(message), now);
            Some(Effect::PersistHistory(execution.successful_history()))
        }
        Action::WorkerDisconnected => match state {
            SessionState::Execution(execution) if execution.cancellation_requested() => {
                Some(Effect::Finish(SessionOutcome::Interrupted))
            }
            SessionState::Execution(execution) if execution.stage() == ExecutionStage::Failed => {
                None
            }
            SessionState::Execution(execution) => {
                Some(Effect::Finish(SessionOutcome::Failed(execution.stage())))
            }
            SessionState::Apply(execution) if execution.result().is_none() => {
                execution.finish_apply(
                    ApplyStatus::Failed,
                    None,
                    Some("Apply worker disconnected.".to_owned()),
                    now,
                );
                Some(Effect::PersistHistory(execution.successful_history()))
            }
            SessionState::Review(_) | SessionState::Apply(_) => None,
        },
        Action::Copy(target) => match state {
            SessionState::Execution(execution) | SessionState::Apply(execution) => {
                execution.copy_effect(target)
            }
            SessionState::Review(review)
                if target == CopyTarget::Plan && review.shows_copy_feedback() =>
            {
                Some(copy::plan_effect(&review.review))
            }
            SessionState::Review(_) => None,
        }
        .map(Effect::WriteClipboard),
        Action::CopyCompleted { target, result } => {
            let flash = match state {
                SessionState::Execution(_) | SessionState::Apply(_) => {
                    target == CopyTarget::Execution
                }
                SessionState::Review(_) => target == CopyTarget::Plan,
            };
            if let Some(feedback) = state.copy_feedback_mut() {
                feedback.record(result, now, flash);
            }
            None
        }
        Action::Quit => match state {
            SessionState::Execution(execution) if execution.stage() == ExecutionStage::Failed => {
                Some(Effect::Finish(SessionOutcome::Failed(
                    execution.result().map_or(
                        ExecutionStage::Failed,
                        super::execution::ExecutionResult::phase,
                    ),
                )))
            }
            SessionState::Execution(_) => None,
            SessionState::Review(review) if review.review.apply_entry() => {
                Some(Effect::Finish(SessionOutcome::ApplyCanceled))
            }
            SessionState::Review(review) => Some(Effect::Finish(SessionOutcome::Reviewed {
                changes: review.review.has_changes().then(|| ReviewedChanges {
                    resources: review.review.summary(),
                    outputs: review.review.changed_outputs(),
                }),
            })),
            SessionState::Apply(execution) => execution.result().map(|result| {
                Effect::Finish(SessionOutcome::Applied {
                    status: match execution.stage() {
                        ExecutionStage::ApplySucceeded => ApplyStatus::Succeeded,
                        ExecutionStage::ApplyInterrupted => ApplyStatus::Interrupted,
                        _ => ApplyStatus::Failed,
                    },
                    summary_line: result.summary_line().map(str::to_owned),
                })
            }),
        },
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::{PlanReview, RawReviewScreen, ReviewScreen, ReviewSessionState};

    pub(crate) fn overview_session(review: PlanReview) -> ReviewSessionState {
        ReviewSessionState {
            screen: ReviewScreen::Overview,
            ..ReviewSessionState::new(review)
        }
    }

    pub(crate) fn apply_confirmation_session(review: PlanReview) -> ReviewSessionState {
        ReviewSessionState {
            screen: ReviewScreen::ApplyConfirmation {
                return_to: RawReviewScreen::default(),
            },
            ..ReviewSessionState::new(review)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use super::super::copy::CopyNotice;
    use super::super::execution::ExecutionContext;
    use super::super::plan::{
        Plan, PlanAction, ResourceChange, ResourceChangeKind,
        test_support::{output_change, resource_change},
    };
    use super::super::review::{
        PlanBlock, PlanBlockKind, PlanMetadata,
        test_support::{plan_document, plan_document_with_blocks},
    };
    use super::test_support::{apply_confirmation_session, overview_session};
    use super::*;
    use rstest::rstest;

    fn review() -> PlanReview {
        PlanReview::new(
            PathBuf::from("/project"),
            "default".to_owned(),
            plan_document("No changes.\n".to_owned()),
            Plan::empty(),
            PlanMetadata::new(false),
            Vec::new(),
        )
    }

    // Output-only, so an apply completes without per-resource progress events.
    fn applyable_review() -> PlanReview {
        PlanReview::new(
            PathBuf::from("/project"),
            "default".to_owned(),
            plan_document("Terraform will perform actions.\n".to_owned()),
            Plan {
                output_changes: vec![output_change("endpoint", PlanAction::Create)],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        )
    }

    #[test]
    fn overview_round_trip_clears_only_the_temporary_raw_filter() {
        let now = Instant::now();
        let mut review = applyable_review();
        review.set_search_query("api".to_owned());
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));
        update(&mut state, Action::ReviewCompleted(review), now);

        assert!(update(&mut state, Action::OpenOverview, now).is_none());
        let Some(overview) = state.overview() else {
            panic!("overview should be visible");
        };
        assert_eq!(overview.review().search_query(), "api");

        assert!(update(&mut state, Action::OpenReviewFromOverview, now).is_none());
        let Some(review) = state.review() else {
            panic!("raw review should be visible");
        };
        assert!(review.is_from_overview());
        assert!(review.review().search_query().is_empty());

        assert!(update(&mut state, Action::OpenOverview, now).is_none());
        assert_eq!(
            state
                .overview()
                .expect("overview should return")
                .review()
                .search_query(),
            "api"
        );

        assert!(update(&mut state, Action::OpenReviewFromOverview, now).is_none());
        assert!(update(&mut state, Action::ReturnToOverview, now).is_none());
        assert_eq!(
            state
                .overview()
                .expect("overview should be restored")
                .review()
                .search_query(),
            "api"
        );
    }

    #[test]
    fn cancelled_confirmation_restores_the_overview_detail_before_returning_to_the_overview() {
        let now = Instant::now();
        let mut review = applyable_review();
        review.set_search_query("api".to_owned());
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));
        update(&mut state, Action::ReviewCompleted(review), now);
        update(&mut state, Action::OpenOverview, now);
        update(
            &mut state,
            Action::CopyCompleted {
                target: CopyTarget::Plan,
                result: CopyResult::Written,
            },
            now,
        );
        assert!(
            state
                .copy_feedback()
                .is_some_and(|feedback| feedback.notice().is_some())
        );

        update(&mut state, Action::OpenReviewFromOverview, now);
        assert!(
            state
                .copy_feedback()
                .is_some_and(|feedback| feedback.notice().is_none())
        );
        update(
            &mut state,
            Action::ReviewSearchChanged("worker".to_owned()),
            now,
        );
        assert!(update(&mut state, Action::OpenApplyConfirmation, now).is_none());
        let confirmation = state
            .apply_confirmation()
            .expect("confirmation should open from the overview detail");
        assert_eq!(confirmation.review().search_query(), "worker");
        assert!(state.copy_feedback().is_none());
        assert!(update(&mut state, Action::Copy(CopyTarget::Plan), now).is_none());
        update(
            &mut state,
            Action::ReviewSearchChanged("ignored".to_owned()),
            now,
        );

        assert!(update(&mut state, Action::CancelApply, now).is_none());
        let raw = state
            .review()
            .expect("cancel should restore the raw review, not the overview");
        assert!(raw.is_from_overview());
        assert_eq!(raw.review().search_query(), "worker");

        assert!(update(&mut state, Action::ReturnToOverview, now).is_none());
        assert_eq!(
            state
                .overview()
                .expect("Esc should return to the overview")
                .review()
                .search_query(),
            "api"
        );
    }

    #[rstest]
    #[case::confirm_from_raw(ReviewSessionState::new, Action::ConfirmApply("yes".to_owned()))]
    #[case::cancel_from_raw(ReviewSessionState::new, Action::CancelApply)]
    #[case::raw_from_raw(ReviewSessionState::new, Action::OpenReviewFromOverview)]
    #[case::return_from_plan_raw(ReviewSessionState::new, Action::ReturnToOverview)]
    #[case::confirm_from_overview(overview_session, Action::ConfirmApply("yes".to_owned()))]
    #[case::overview_from_overview(overview_session, Action::OpenOverview)]
    #[case::return_from_overview(overview_session, Action::ReturnToOverview)]
    #[case::apply_from_overview(overview_session, Action::OpenApplyConfirmation)]
    #[case::cancel_from_overview(overview_session, Action::CancelApply)]
    #[case::search_from_overview(overview_session, Action::ReviewSearchChanged("x".to_owned()))]
    #[case::overview_from_confirmation(apply_confirmation_session, Action::OpenOverview)]
    #[case::raw_from_confirmation(apply_confirmation_session, Action::OpenReviewFromOverview)]
    #[case::return_from_confirmation(apply_confirmation_session, Action::ReturnToOverview)]
    #[case::apply_from_confirmation(apply_confirmation_session, Action::OpenApplyConfirmation)]
    #[case::search_from_confirmation(
        apply_confirmation_session,
        Action::ReviewSearchChanged("x".to_owned())
    )]
    fn review_actions_for_another_screen_change_nothing(
        #[case] screen: fn(PlanReview) -> ReviewSessionState,
        #[case] action: Action,
    ) {
        let mut review = applyable_review();
        review.set_search_query("api".to_owned());
        let mut state = SessionState::Review(Box::new(screen(review)));
        let before = state.clone();

        assert!(update(&mut state, action, Instant::now()).is_none());
        assert_eq!(state, before);
    }

    #[test]
    fn quitting_the_overview_of_an_apply_entry_cancels_the_apply() {
        let mut state = SessionState::Review(Box::new(overview_session(
            applyable_review().with_apply_entry(true),
        )));
        assert!(matches!(
            update(&mut state, Action::Quit, Instant::now()),
            Some(Effect::Finish(SessionOutcome::ApplyCanceled))
        ));
    }

    #[test]
    fn cancellation_is_requested_once_and_late_completion_stays_interrupted() {
        let now = Instant::now();
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));

        assert!(matches!(
            update(&mut state, Action::RequestCancellation, now),
            Some(Effect::CancelExecution)
        ));
        assert!(update(&mut state, Action::RequestCancellation, now).is_none());
        assert!(matches!(
            update(&mut state, Action::ReviewCompleted(review()), now),
            Some(Effect::Finish(SessionOutcome::Interrupted))
        ));
    }

    #[test]
    fn filtered_review_copies_the_full_document_and_quits_without_apply() {
        let now = Instant::now();
        let source = "Terraform will perform actions.\n\n".to_owned()
            + "  # terraform_data.api will be created\n"
            + "  + resource \"terraform_data\" \"api\" {\n"
            + "      input = \"api\"\n"
            + "    }\n\n"
            + "Plan: 1 to add, 0 to change, 0 to destroy.\n";
        let mut filtered = PlanReview::new(
            PathBuf::from("/project"),
            "default".to_owned(),
            plan_document_with_blocks(
                source.clone(),
                vec![
                    PlanBlock::new(0..2, PlanBlockKind::Common),
                    PlanBlock::with_addresses(
                        2..6,
                        PlanBlockKind::Resource,
                        vec!["terraform_data.api".to_owned()],
                    ),
                    PlanBlock::new(6..9, PlanBlockKind::Common),
                ],
            ),
            Plan {
                resource_changes: vec![resource_change(
                    "terraform_data.api",
                    ResourceChangeKind::Create,
                )],
                ..Plan::empty()
            },
            PlanMetadata::new(false),
            Vec::new(),
        );
        filtered.set_search_query("not-present".to_owned());
        let visible = filtered.document().filter(filtered.search_query());
        assert_eq!(visible.matching_resources(), 0);
        assert!(
            visible
                .lines_with_indices()
                .all(|(_, line)| !line.contains("terraform_data.api"))
        );

        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));
        update(&mut state, Action::ReviewCompleted(filtered), now);

        let Some(Effect::WriteClipboard(effect)) =
            update(&mut state, Action::Copy(CopyTarget::Plan), now)
        else {
            panic!("plan copy should be available");
        };
        assert_eq!(effect.text(), source);
        let Some(Effect::Finish(outcome)) = update(&mut state, Action::Quit, now) else {
            panic!("quitting the review should finish the session");
        };
        assert_eq!(
            outcome,
            SessionOutcome::Reviewed {
                changes: Some(ReviewedChanges {
                    resources: PlanSummary {
                        creates: 1,
                        ..PlanSummary::default()
                    },
                    outputs: 0,
                }),
            }
        );
        assert!(state.review().is_some());
    }

    #[test]
    fn copy_notice_replacement_resets_the_success_and_failure_deadlines() {
        let started_at = Instant::now();
        let mut state = SessionState::new(ExecutionState::with_context(
            started_at,
            ExecutionContext::loading("/project"),
        ));
        update(&mut state, Action::ReviewCompleted(review()), started_at);
        update(
            &mut state,
            Action::CopyCompleted {
                target: CopyTarget::Plan,
                result: CopyResult::Written,
            },
            started_at,
        );
        update(
            &mut state,
            Action::CopyCompleted {
                target: CopyTarget::Plan,
                result: CopyResult::Failed,
            },
            started_at + Duration::from_secs(1),
        );

        let SessionState::Review(review) = &state else {
            panic!("review should remain visible");
        };
        assert_eq!(
            review
                .copy_feedback()
                .notice_at(started_at + Duration::from_millis(3_999)),
            Some(CopyNotice::Failed)
        );
        assert_eq!(
            review
                .copy_feedback()
                .notice_at(started_at + Duration::from_secs(6)),
            None
        );
    }

    #[test]
    fn apply_requires_confirmation_and_cancel_preserves_the_review() {
        let now = Instant::now();
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));
        update(&mut state, Action::ReviewCompleted(applyable_review()), now);
        assert!(update(&mut state, Action::OpenApplyConfirmation, now).is_none());
        assert!(state.apply_confirmation().is_some());

        assert!(update(&mut state, Action::CancelApply, now).is_none());
        let Some(review) = state.review() else {
            panic!("cancel should restore the raw review");
        };
        assert_eq!(
            review.review().document().text(),
            "Terraform will perform actions.\n"
        );
    }

    #[test]
    fn quit_reports_only_applyable_drift() {
        struct DriftCase {
            name: &'static str,
            applyable: bool,
            expected: Option<ReviewedChanges>,
        }

        let now = Instant::now();
        for case in [
            DriftCase {
                name: "normal_plan",
                applyable: false,
                expected: None,
            },
            DriftCase {
                name: "refresh_only_plan",
                applyable: true,
                expected: Some(ReviewedChanges {
                    resources: PlanSummary::default(),
                    outputs: 0,
                }),
            },
        ] {
            let mut state = SessionState::new(ExecutionState::with_context(
                now,
                ExecutionContext::loading("/project"),
            ));
            let review = PlanReview::new(
                PathBuf::from("/project"),
                "default".to_owned(),
                plan_document("No changes.\n".to_owned()),
                Plan {
                    drifted_resources: vec!["terraform_data.drifted".to_owned()],
                    ..Plan::empty()
                },
                PlanMetadata::new(case.applyable),
                Vec::new(),
            );
            update(&mut state, Action::ReviewCompleted(review), now);

            let Some(Effect::Finish(outcome)) = update(&mut state, Action::Quit, now) else {
                panic!(
                    "case {}: quitting the review should finish the session",
                    case.name
                );
            };

            assert_eq!(
                outcome,
                SessionOutcome::Reviewed {
                    changes: case.expected,
                },
                "case: {}",
                case.name
            );
        }
    }

    #[test]
    fn quitting_a_plan_entry_confirmation_reports_the_review_without_apply() {
        let now = Instant::now();
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));
        update(&mut state, Action::ReviewCompleted(applyable_review()), now);
        update(&mut state, Action::OpenApplyConfirmation, now);

        let Some(Effect::Finish(outcome)) = update(&mut state, Action::Quit, now) else {
            panic!("quitting the review should finish the session");
        };
        assert_eq!(
            outcome,
            SessionOutcome::Reviewed {
                changes: Some(ReviewedChanges {
                    resources: PlanSummary::default(),
                    outputs: 1,
                }),
            }
        );

        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));
        update(
            &mut state,
            Action::ReviewCompleted(applyable_review().with_apply_entry(true)),
            now,
        );
        update(&mut state, Action::OpenApplyConfirmation, now);
        assert!(matches!(
            update(&mut state, Action::Quit, now),
            Some(Effect::Finish(SessionOutcome::ApplyCanceled))
        ));
    }

    #[test]
    fn apply_confirmation_starts_once_and_completion_can_quit_with_status() {
        let now = Instant::now();
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));
        update(&mut state, Action::ReviewCompleted(applyable_review()), now);
        update(&mut state, Action::OpenApplyConfirmation, now);

        assert!(matches!(
            update(&mut state, Action::ConfirmApply("yes".to_owned()), now),
            Some(Effect::StartApply)
        ));
        assert!(update(&mut state, Action::ConfirmApply("yes".to_owned()), now).is_none());
        update(
            &mut state,
            Action::ApplyCompleted {
                status: ApplyStatus::Succeeded,
                summary_line: Some("Apply complete! Resources: 1 added.".to_owned()),
            },
            now,
        );

        assert_eq!(
            state.apply().map(ExecutionState::stage),
            Some(ExecutionStage::ApplySucceeded)
        );
        assert!(matches!(
            update(&mut state, Action::Quit, now),
            Some(Effect::Finish(SessionOutcome::Applied {
                status: ApplyStatus::Succeeded,
                ..
            }))
        ));
    }

    #[test]
    fn apply_confirmation_uses_the_named_target_for_destructive_changes() {
        let now = Instant::now();
        let review = PlanReview::new(
            PathBuf::from("/repo/prod"),
            "default".to_owned(),
            plan_document("Terraform will perform actions.\n".to_owned()),
            Plan {
                resource_changes: vec![resource_change(
                    "terraform_data.old",
                    ResourceChangeKind::Delete,
                )],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        );
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/repo/prod"),
        ));
        update(&mut state, Action::ReviewCompleted(review), now);
        update(&mut state, Action::OpenApplyConfirmation, now);

        assert!(update(&mut state, Action::ConfirmApply("yes".to_owned()), now).is_none());
        assert!(state.apply_confirmation().is_some());
        assert!(matches!(
            update(&mut state, Action::ConfirmApply("prod".to_owned()), now),
            Some(Effect::StartApply)
        ));
    }

    #[cfg(unix)]
    #[rstest]
    #[case::destructive(b"/repo/infra-\xff", ResourceChangeKind::Delete)]
    #[case::production(b"/repo/prod/infra-\xff", ResourceChangeKind::Create)]
    fn non_utf8_target_confirms_only_with_its_escaped_name(
        #[case] directory: &[u8],
        #[case] kind: ResourceChangeKind,
    ) {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};
        let now = Instant::now();
        let directory = PathBuf::from(OsString::from_vec(directory.to_vec()));
        let review = PlanReview::new(
            directory.clone(),
            "default".to_owned(),
            plan_document("Terraform will perform actions.\n".to_owned()),
            Plan {
                resource_changes: vec![resource_change("terraform_data.target", kind)],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        );
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading(directory),
        ));
        update(&mut state, Action::ReviewCompleted(review), now);
        update(&mut state, Action::OpenApplyConfirmation, now);

        for rejected in ["yes", "infra-\u{fffd}", r"infra-\xfe", r"infra-\\xff"] {
            assert!(
                update(&mut state, Action::ConfirmApply(rejected.to_owned()), now).is_none(),
                "input: {rejected}"
            );
        }
        assert!(state.apply_confirmation().is_some());
        assert!(matches!(
            update(
                &mut state,
                Action::ConfirmApply(r"infra-\xff".to_owned()),
                now
            ),
            Some(Effect::StartApply)
        ));
    }

    #[test]
    fn quitting_a_review_without_changes_reports_no_changes() {
        let now = Instant::now();
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));
        update(&mut state, Action::ReviewCompleted(review()), now);

        let Some(Effect::Finish(outcome)) = update(&mut state, Action::Quit, now) else {
            panic!("quitting the review should finish the session");
        };
        assert_eq!(outcome, SessionOutcome::Reviewed { changes: None });
    }

    #[test]
    fn filtered_review_applies_the_complete_plan() {
        let now = Instant::now();
        let mut filtered = PlanReview::new(
            PathBuf::from("/project"),
            "default".to_owned(),
            plan_document("Terraform will perform actions.\n".to_owned()),
            Plan {
                resource_changes: vec![
                    resource_change("terraform_data.api", ResourceChangeKind::Update),
                    resource_change("terraform_data.worker", ResourceChangeKind::Create),
                ],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        )
        .with_apply_entry(true);
        filtered.set_search_query("not-present".to_owned());
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));
        update(&mut state, Action::ReviewCompleted(filtered), now);
        assert!(update(&mut state, Action::OpenApplyConfirmation, now).is_none());
        assert!(state.apply_confirmation().is_some());
        assert_eq!(
            state
                .apply_confirmation()
                .expect("apply confirmation should retain the review")
                .review()
                .search_query(),
            "not-present"
        );

        assert!(matches!(
            update(&mut state, Action::ConfirmApply("yes".to_owned()), now),
            Some(Effect::StartApply)
        ));
        let apply = state
            .apply()
            .expect("the full plan should enter apply state");
        let targets = apply.progress().targets();
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].address(), "terraform_data.api");
        assert_eq!(targets[0].actions(), &[PlanAction::Update]);
        assert_eq!(targets[1].address(), "terraform_data.worker");
        assert_eq!(targets[1].actions(), &[PlanAction::Create]);
    }

    #[test]
    fn apply_start_pairs_previous_durations_with_targets_in_plan_order() {
        let now = Instant::now();
        let api_duration = Duration::from_secs(3);
        let worker_duration = Duration::from_secs(7);
        let review = PlanReview::new(
            PathBuf::from("/project"),
            "default".to_owned(),
            plan_document("Terraform will perform actions.\n".to_owned()),
            Plan {
                resource_changes: vec![
                    resource_change("terraform_data.api", ResourceChangeKind::Update),
                    ResourceChange {
                        has_previous_address: true,
                        ..resource_change("terraform_data.moved", ResourceChangeKind::Move)
                    },
                    resource_change("terraform_data.worker", ResourceChangeKind::Create),
                ],
                ..Plan::empty()
            },
            PlanMetadata::new(true),
            Vec::new(),
        )
        .with_previous_durations(vec![Some(api_duration), Some(worker_duration)]);
        let mut state = SessionState::new(ExecutionState::with_context(
            now,
            ExecutionContext::loading("/project"),
        ));
        update(&mut state, Action::ReviewCompleted(review), now);
        update(&mut state, Action::OpenApplyConfirmation, now);

        update(&mut state, Action::ConfirmApply("yes".to_owned()), now);

        let targets = state
            .apply()
            .expect("confirmation should start apply")
            .progress()
            .targets();
        assert_eq!(
            targets
                .iter()
                .map(|target| (target.address(), target.previous()))
                .collect::<Vec<_>>(),
            [
                ("terraform_data.api", Some(api_duration)),
                ("terraform_data.worker", Some(worker_duration)),
            ]
        );
    }

    #[test]
    fn apply_cancellation_requests_the_worker_to_stop() {
        let now = Instant::now();
        let mut state = SessionState::Apply(Box::new(ExecutionState::applying(
            now,
            ExecutionContext::loading("/project"),
        )));

        assert!(matches!(
            update(&mut state, Action::RequestCancellation, now),
            Some(Effect::CancelExecution)
        ));
        assert!(state.apply().is_some_and(ExecutionState::is_cancelling));
    }
}
