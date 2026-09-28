mod apply_confirmation;
mod content;
mod layout;
mod overlay;
mod review_footer;
mod status;

use std::time::Instant;

use ratatui::{
    Frame,
    layout::Rect,
    widgets::{Block, Paragraph},
};

use crate::app::session::ReviewSessionState;
use crate::ui::primitives::{
    atoms::{scrollbar, separator},
    molecules::terminal_notice,
};
use crate::ui::shell::{footer, header};
use crate::ui::theme;

use super::PlanReviewViewState;

pub(crate) use apply_confirmation::{
    apply_confirmation_layout, apply_confirmation_redraw_at, render_apply_confirmation,
    render_apply_confirmation_dialog,
};
pub(crate) use layout::{
    environment_layout, layout, layout_with_quit_confirmation, overview_detail_layout,
};

use content::{content_lines_with_selection, flash_lines, prepare_view_content, visible_lines};
use layout::layout_with_content;
use overlay::render_overlay;
use review_footer::review_footer_status;
use status::render_status;

const MIN_WIDTH: u16 = 24;
const MIN_HEIGHT: u16 = 6;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReviewNavigation {
    Standalone,
    Environments,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FooterMode {
    Actions,
    QuitConfirmation,
    Suppressed,
}

pub(crate) fn render_environment(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &ReviewSessionState,
    view: &mut PlanReviewViewState,
    now: Instant,
) {
    if render_for_navigation(
        frame,
        state,
        view,
        now,
        FooterMode::Actions,
        ReviewNavigation::Environments,
        area,
    ) {
        render_overlay(
            frame,
            area,
            state.review(),
            view,
            ReviewNavigation::Environments,
        );
    }
}

pub(crate) fn render_environment_with_quit_confirmation(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &ReviewSessionState,
    view: &mut PlanReviewViewState,
    now: Instant,
    acquiring: bool,
) {
    if render_for_navigation(
        frame,
        state,
        view,
        now,
        if acquiring {
            FooterMode::Suppressed
        } else {
            FooterMode::QuitConfirmation
        },
        ReviewNavigation::Environments,
        area,
    ) {
        render_overlay(
            frame,
            area,
            state.review(),
            view,
            ReviewNavigation::Environments,
        );
    }
}

pub(crate) fn render_with_quit_confirmation(
    frame: &mut Frame<'_>,
    state: &ReviewSessionState,
    view: &PlanReviewViewState,
    now: Instant,
    quit_confirmation: bool,
) {
    // The clone only keeps body scroll reconciliation local; the overlay records its scroll
    // limit on the caller's view so the next key starts from the rendered offset.
    let mut reconciled = view.clone();
    if render_for_navigation(
        frame,
        state,
        &mut reconciled,
        now,
        if quit_confirmation {
            FooterMode::QuitConfirmation
        } else {
            FooterMode::Actions
        },
        ReviewNavigation::Standalone,
        frame.area(),
    ) {
        render_overlay(
            frame,
            frame.area(),
            state.review(),
            view,
            ReviewNavigation::Standalone,
        );
    }
}

// Returns false when only the size notice was drawn; overlays are drawn by the callers so the
// scroll limit is recorded on the view that receives the next key.
#[expect(
    clippy::too_many_lines,
    reason = "the plan renderer keeps the feature layout and content projection in one path"
)]
fn render_for_navigation(
    frame: &mut Frame<'_>,
    state: &ReviewSessionState,
    view: &mut PlanReviewViewState,
    now: Instant,
    footer_mode: FooterMode,
    navigation: ReviewNavigation,
    area: Rect,
) -> bool {
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        terminal_notice::render_wrapped(
            frame,
            area,
            terminal_notice_message(
                view.searching(),
                filter_active(view.searching(), state),
                footer_mode,
            ),
        );
        return false;
    }

    let filtered_view = filter_active(view.searching(), state);
    let content = prepare_view_content(state, filtered_view);
    let layout = layout_with_content(
        area,
        view.searching(),
        state.review(),
        &content,
        state.copy_feedback().notice_at(now),
        footer_mode,
        navigation,
    );
    if layout.body().width == 0 || layout.body().height == 0 {
        terminal_notice::render_wrapped(
            frame,
            area,
            terminal_notice_message(
                view.searching(),
                filter_active(view.searching(), state),
                footer_mode,
            ),
        );
        return false;
    }
    view.reconcile_scroll(layout.max_vertical(), layout.max_horizontal());
    header::render_plan_review(frame, layout.shell.header(), state.review());
    frame.render_widget(
        Block::new().style(theme::body_style()),
        layout.shell.content(),
    );
    render_status(frame, &layout, state, view);

    let content_metrics = content.metrics();
    let line_count = content_metrics.line_count;
    let max_line_width = content_metrics.max_width;
    let max_vertical = layout.max_vertical();
    let max_horizontal = layout.max_horizontal();
    let (vertical, horizontal) = view.scroll();
    let vertical = vertical.min(max_vertical);
    let horizontal = horizontal.min(max_horizontal);
    let lines = if state.copy_feedback().flash_active(now) {
        flash_lines(&content.lines)
    } else {
        content_lines_with_selection(
            &content,
            state.review().search_query(),
            view.selected()
                .and_then(|selected| content.matches.get(selected)),
        )
    };
    let body = layout.body();
    frame.render_widget(
        Paragraph::new(visible_lines(
            &lines,
            vertical,
            horizontal,
            body.height,
            body.width,
        ))
        .style(theme::body_style()),
        body,
    );
    let scrollbar_area = Rect::new(
        body.x,
        body.y,
        body.width
            .saturating_add(u16::from(layout.vertical_scrollbar())),
        body.height
            .saturating_add(u16::from(layout.horizontal_scrollbar())),
    );
    if layout.vertical_scrollbar() {
        scrollbar::render_vertical(
            frame,
            scrollbar_area,
            line_count,
            usize::from(body.height),
            vertical,
        );
    }
    if layout.horizontal_scrollbar() {
        scrollbar::render_horizontal(
            frame,
            scrollbar_area,
            max_line_width,
            usize::from(body.width),
            horizontal,
        );
    }
    let footer_status =
        if footer_mode != FooterMode::Actions || state.copy_feedback().notice_at(now).is_some() {
            layout.footer_status.clone()
        } else {
            Some((
                review_footer_status(state, view, &content, layout.shell.footer().width),
                theme::secondary_style(),
            ))
        };
    footer::render(
        frame,
        layout.shell.footer(),
        layout.shell.footer_lines(),
        footer_status
            .as_ref()
            .map(|(message, style)| (message.as_str(), *style)),
    );
    frame.render_widget(
        separator::render(layout.shell.footer_separator().width),
        layout.shell.footer_separator(),
    );
    true
}

fn filter_active(searching: bool, state: &ReviewSessionState) -> bool {
    searching || !state.review().search_query().is_empty()
}

fn terminal_notice_message(
    searching: bool,
    filtered: bool,
    footer_mode: FooterMode,
) -> &'static str {
    if footer_mode == FooterMode::QuitConfirmation {
        "Quit? Enter exit / Esc cancel"
    } else if footer_mode == FooterMode::Suppressed {
        "Stop? Enter stop / Esc continue"
    } else if searching {
        "Terminal too small. Resize or press Esc to cancel filter."
    } else if filtered {
        "Terminal too small. Resize or press Esc to clear filter."
    } else {
        "Terminal too small. Resize or press q to quit."
    }
}

#[cfg(test)]
mod tests;
