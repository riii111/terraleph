use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Rect, Size};

use crate::app::{
    copy::CopyTarget,
    review::PlanReview,
    session::{Action, ReviewSessionState, SessionState},
};
use crate::ui::features::overview::{self, OverviewCommand};

use super::{
    PlanReviewInput, PlanReviewViewState, key_to_input, layout, layout_with_quit_confirmation,
    overview_detail_layout, unfiltered_row_for_source_line,
};

impl PlanReviewViewState {
    pub(crate) fn handle_key(
        &mut self,
        review: &ReviewSessionState,
        key: KeyEvent,
        size: Size,
    ) -> Option<Action> {
        if self.overlay().is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
                self.close_overlay();
            } else {
                self.overlay_scroll_mut().handle_key(key.code, 8);
            }
            return None;
        }
        match key_to_input(
            key,
            self.searching(),
            !review.review().search_query().is_empty(),
        ) {
            Some(PlanReviewInput::Quit) => Some(Action::Quit),
            Some(PlanReviewInput::Apply) => Some(Action::OpenApplyConfirmation),
            Some(PlanReviewInput::Copy) => Some(Action::Copy(CopyTarget::Plan)),
            Some(PlanReviewInput::OpenOverview) => {
                let content = overview::OverviewContent::project(
                    review,
                    self.overview().filter(),
                    self.overview().expanded(),
                );
                self.overview_mut().reconcile(u16::MAX, content.rows.len());
                Some(Action::OpenOverview)
            }
            Some(PlanReviewInput::SearchCancel)
                if !self.searching() && review.is_from_overview() =>
            {
                Some(Action::ReturnToOverview)
            }
            Some(input) => {
                let body = layout(Rect::from(size), self, review);
                self.apply_with_matches(
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
        }
    }

    pub(crate) fn handle_overview_key(
        &mut self,
        overview_state: &ReviewSessionState,
        key: KeyEvent,
        size: Size,
    ) -> Option<Action> {
        let command = self.overview_mut().handle_key(overview_state, key, size)?;
        Some(match command {
            OverviewCommand::Open(address) => {
                self.jump_to_overview_address(overview_state, address.as_deref(), size);
                Action::OpenReviewFromOverview
            }
            OverviewCommand::ViewPlan | OverviewCommand::Back => {
                self.jump_to_line(0, usize::MAX);
                Action::OpenReviewFromOverview
            }
            OverviewCommand::Copy => Action::Copy(CopyTarget::Plan),
            OverviewCommand::Quit => Action::Quit,
        })
    }

    // The apply confirmation body and the dialogs clamp against the current layout on their next
    // input or render, so only the review and Overview offsets need a resize correction.
    pub(crate) fn reconcile_resize(
        &mut self,
        state: &SessionState,
        area: Rect,
        quit_confirmation: bool,
    ) {
        if let Some(review) = state.review() {
            let layout = layout_with_quit_confirmation(area, self, review, quit_confirmation);
            self.reconcile(
                layout.body(),
                layout.max_vertical(),
                layout.max_horizontal(),
                layout.matches(),
            );
        }
        if let Some(overview_state) = state.overview() {
            overview::reconcile_view(area, overview_state, self.overview_mut());
        }
    }

    pub(crate) fn jump_to_source_line(&mut self, review: &PlanReview, line: usize) {
        let row = unfiltered_row_for_source_line(review, self, line);
        self.jump_to_line(row, usize::MAX);
    }

    fn jump_to_overview_address(
        &mut self,
        overview: &ReviewSessionState,
        address: Option<&str>,
        size: Size,
    ) {
        let line = address
            .and_then(|address| overview.review().document().block_for_address(address))
            .map_or(0, |block| block.lines().start);
        let row = unfiltered_row_for_source_line(overview.review(), self, line);
        let layout = overview_detail_layout(Rect::from(size), self, overview.review());
        self.jump_to_line(row, layout.max_vertical());
    }
}
