use std::rc::Rc;

use ratatui::{layout::Rect, style::Style, text::Line};

use crate::app::{copy::CopyNotice, review::PlanReview, session::ReviewSessionState};
use crate::ui::features::plan_review::{PlanReviewMatch, PlanReviewViewState};
use crate::ui::primitives::atoms::scrollbar;
use crate::ui::shell::{footer, header, layout as shell_layout};
use crate::ui::theme;

use super::{
    FooterMode, ReviewNavigation,
    content::PlanContent,
    review_footer::{
        common_footer_height, filter_footer_status, footer_items, position_status_for_content,
        required_footer_items, review_footer_status_text,
    },
    view_content,
};

pub(crate) struct PlanReviewLayout {
    pub(super) shell: shell_layout::ShellLayout,
    body: Rect,
    status: Rect,
    separator: Rect,
    pub(super) footer_status: Option<(String, Style)>,
    vertical_scrollbar: bool,
    horizontal_scrollbar: bool,
    max_vertical: usize,
    max_horizontal: usize,
    content: Rc<PlanContent>,
}

impl PlanReviewLayout {
    pub(crate) const fn body(&self) -> Rect {
        self.body
    }

    pub(crate) const fn status(&self) -> Rect {
        self.status
    }

    pub(crate) const fn separator(&self) -> Rect {
        self.separator
    }

    pub(crate) const fn vertical_scrollbar(&self) -> bool {
        self.vertical_scrollbar
    }

    pub(crate) const fn horizontal_scrollbar(&self) -> bool {
        self.horizontal_scrollbar
    }

    pub(crate) const fn max_vertical(&self) -> usize {
        self.max_vertical
    }

    pub(crate) const fn max_horizontal(&self) -> usize {
        self.max_horizontal
    }

    pub(crate) fn matches(&self) -> &[PlanReviewMatch] {
        self.content.matches()
    }

    pub(super) const fn content(&self) -> &Rc<PlanContent> {
        &self.content
    }
}

pub(crate) fn layout(
    area: Rect,
    view: &PlanReviewViewState,
    state: &ReviewSessionState,
) -> PlanReviewLayout {
    layout_with_quit_confirmation(area, view, state, false)
}

pub(crate) fn environment_layout(
    area: Rect,
    view: &PlanReviewViewState,
    state: &ReviewSessionState,
) -> PlanReviewLayout {
    layout_for_navigation(
        area,
        view,
        state,
        FooterMode::Actions,
        ReviewNavigation::Environments,
    )
}

pub(crate) fn layout_with_quit_confirmation(
    area: Rect,
    view: &PlanReviewViewState,
    state: &ReviewSessionState,
    quit_confirmation: bool,
) -> PlanReviewLayout {
    layout_for_navigation(
        area,
        view,
        state,
        if quit_confirmation {
            FooterMode::QuitConfirmation
        } else {
            FooterMode::Actions
        },
        ReviewNavigation::Standalone,
    )
}

fn layout_for_navigation(
    area: Rect,
    view: &PlanReviewViewState,
    state: &ReviewSessionState,
    footer_mode: FooterMode,
    navigation: ReviewNavigation,
) -> PlanReviewLayout {
    layout_with_content(
        area,
        view.searching(),
        state.review(),
        view_content(state, view),
        state.copy_feedback().notice(),
        footer_mode,
        navigation,
    )
}

/// Layout of the raw plan opened from the overview, which starts unfiltered and without a copy
/// notice.
pub(crate) fn overview_detail_layout(
    area: Rect,
    view: &PlanReviewViewState,
    review: &PlanReview,
) -> PlanReviewLayout {
    layout_with_content(
        area,
        false,
        review,
        view.content_cache().get(review, false, ""),
        None,
        FooterMode::Actions,
        ReviewNavigation::Standalone,
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "the plan layout keeps all width, height, footer, and scroll calculations together"
)]
pub(super) fn layout_with_content(
    area: Rect,
    searching: bool,
    review: &PlanReview,
    content: Rc<PlanContent>,
    copy_notice: Option<CopyNotice>,
    footer_mode: FooterMode,
    navigation: ReviewNavigation,
) -> PlanReviewLayout {
    let panel_width = area.width;
    let content_metrics = content.metrics();
    let applyable = review.metadata().applyable();
    let filter_visible = searching || !content.filter_query().is_empty();
    let showing =
        filter_footer_status(content.filter_query(), content.matches().len(), panel_width);
    let footer_status = match footer_mode {
        FooterMode::QuitConfirmation => None,
        FooterMode::Suppressed => copy_notice.map(|notice| {
            (
                notice.message().to_owned(),
                if matches!(notice, CopyNotice::Failed) {
                    theme::error_style()
                } else {
                    theme::accent_style()
                },
            )
        }),
        FooterMode::Actions => copy_notice
            .map(|notice| {
                (
                    notice.message().to_owned(),
                    if matches!(notice, CopyNotice::Failed) {
                        theme::error_style()
                    } else {
                        theme::accent_style()
                    },
                )
            })
            .or_else(|| {
                if filter_visible {
                    showing.as_ref().map(|message| {
                        (
                            review_footer_status_text(
                                message,
                                &position_status_for_content(
                                    &content,
                                    0,
                                    review.document().line_count(),
                                    panel_width,
                                ),
                            ),
                            theme::secondary_style(),
                        )
                    })
                } else {
                    Some((
                        position_status_for_content(
                            &content,
                            0,
                            review.document().line_count(),
                            panel_width,
                        ),
                        theme::secondary_style(),
                    ))
                }
            }),
    };
    let footer_message = footer_status.as_ref().map(|(message, _)| message.as_str());
    let available_footer_width = footer::available_width(panel_width, footer_message);
    let normal_footer_lines = footer::layout_prioritized(
        footer_items(
            searching,
            applyable,
            content.matches().len(),
            filter_visible,
            navigation,
            available_footer_width,
        ),
        available_footer_width,
    );
    let normal_required = footer::layout_prioritized(
        required_footer_items(
            searching,
            content.matches().len(),
            filter_visible,
            navigation,
        ),
        available_footer_width,
    );
    let footer_height = common_footer_height(
        applyable,
        content.matches().len(),
        panel_width,
        copy_notice.map(CopyNotice::message),
        showing.as_deref(),
        navigation,
    );
    let frame_footer_lines = footer::pad_lines(normal_footer_lines, footer_height);
    let frame_required = footer::pad_lines(normal_required, footer_height);
    let confirmation_lines =
        || footer::pad_lines(footer::quit_confirmation_lines(panel_width), footer_height);
    let suppressed_lines = || footer::pad_lines(vec![Line::default()], footer_height);
    let footer_lines = match footer_mode {
        FooterMode::Actions => frame_footer_lines,
        FooterMode::QuitConfirmation => confirmation_lines(),
        FooterMode::Suppressed => suppressed_lines(),
    };
    let required = match footer_mode {
        FooterMode::Actions => frame_required,
        FooterMode::QuitConfirmation => confirmation_lines(),
        FooterMode::Suppressed => suppressed_lines(),
    };
    let shell = shell_layout::full_width_layout_with_header_height(
        area,
        footer_lines,
        required,
        header::plan_review_height(review, area.width),
    );
    let inner = shell.content_inner();
    let status_height = u16::from(inner.height > 3);
    let fixed_status_height = status_height + 1;
    let status = Rect::new(inner.x, inner.y, inner.width, status_height);
    let separator = Rect::new(
        inner.x,
        inner.y.saturating_add(status_height),
        inner.width,
        1,
    );
    let available = Rect::new(
        inner.x,
        inner.y.saturating_add(fixed_status_height),
        inner.width,
        inner.height.saturating_sub(fixed_status_height),
    );
    let (vertical_scrollbar, horizontal_scrollbar) = scrollbar::reservations(
        content_metrics.line_count,
        content_metrics.max_width,
        available,
    );
    let body = Rect::new(
        available.x,
        available.y,
        available
            .width
            .saturating_sub(u16::from(vertical_scrollbar)),
        available
            .height
            .saturating_sub(u16::from(horizontal_scrollbar)),
    );
    let (max_vertical, max_horizontal) =
        limits(content_metrics.line_count, content_metrics.max_width, body);
    PlanReviewLayout {
        shell,
        body,
        status,
        separator,
        footer_status,
        vertical_scrollbar,
        horizontal_scrollbar,
        max_vertical,
        max_horizontal,
        content,
    }
}

fn limits(line_count: usize, line_width: usize, body: Rect) -> (usize, usize) {
    (
        line_count.saturating_sub(usize::from(body.height)),
        line_width.saturating_sub(usize::from(body.width)),
    )
}
