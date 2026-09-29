use ratatui::{Frame, layout::Rect};

use crate::app::review::PlanReview;
use crate::ui::features::plan_review::{PlanReviewOverlay, PlanReviewViewState};
use crate::ui::primitives::molecules::{context_dialog, help_dialog};

use super::ReviewNavigation;

pub(super) fn render_overlay(
    frame: &mut Frame<'_>,
    area: Rect,
    review: &PlanReview,
    view: &PlanReviewViewState,
    navigation: ReviewNavigation,
) {
    let Some(overlay) = view.overlay() else {
        return;
    };
    match overlay {
        PlanReviewOverlay::Help => help_dialog::render(
            frame,
            area,
            "Help",
            &plan_help_sections(
                review,
                navigation,
                !view.searching() && !review.search_query().is_empty(),
            ),
            view.overlay_scroll(),
        ),
        PlanReviewOverlay::Context => {
            context_dialog::render(frame, area, review.context(), view.overlay_scroll());
        }
    }
}

pub(super) fn plan_help_sections(
    review: &PlanReview,
    navigation: ReviewNavigation,
    filter_confirmed: bool,
) -> Vec<help_dialog::HelpSection> {
    let mut move_actions = Vec::new();
    if navigation == ReviewNavigation::Environments {
        move_actions.push(help_dialog::HelpAction::new(
            "[ / ]",
            "previous / next environment",
        ));
    }
    move_actions.extend([
        help_dialog::HelpAction::new("↑ / ↓ / j / k", "scroll vertically"),
        help_dialog::HelpAction::new("← / → / h / l", "scroll horizontally"),
        help_dialog::HelpAction::new("PgUp / PgDn", "scroll one page"),
        help_dialog::HelpAction::new("Home / End", "go to the top or bottom"),
    ]);
    if navigation == ReviewNavigation::Standalone {
        move_actions.push(help_dialog::HelpAction::new("s", "overview"));
    } else {
        move_actions.push(help_dialog::HelpAction::new("0 / s", "return to overview"));
    }

    let mut review_actions = vec![help_dialog::HelpAction::new(
        "/",
        if review.search_query().is_empty() {
            "filter the full plan"
        } else {
            "edit the full-plan filter"
        },
    )];
    if !review.search_query().is_empty() {
        review_actions.push(help_dialog::HelpAction::new(
            "n / N",
            "next or previous match",
        ));
    }
    if filter_confirmed {
        review_actions.push(help_dialog::HelpAction::new(
            "Esc",
            "close Help; press Esc again to clear filter",
        ));
    }
    let mut action_items = vec![
        help_dialog::HelpAction::new("c", "show execution context"),
        help_dialog::HelpAction::new("y", "copy the full plan"),
    ];
    if review.metadata().applyable() {
        action_items.push(help_dialog::HelpAction::new("a", "apply the full plan"));
    }
    vec![
        help_dialog::HelpSection::new("Navigation", move_actions),
        help_dialog::HelpSection::new("Review", review_actions),
        help_dialog::HelpSection::new("Actions", action_items),
        help_dialog::HelpSection::new("Exit", vec![help_dialog::HelpAction::new("q", "quit")]),
    ]
}
