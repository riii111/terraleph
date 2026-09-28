use ratatui::text::Line;

use crate::app::session::ReviewSessionState;
use crate::ui::features::plan_review::PlanReviewViewState;
use crate::ui::shell::footer;

use super::{ReviewNavigation, content::PreparedContent};

pub(super) fn common_footer_height(
    applyable: bool,
    match_count: usize,
    width: u16,
    copy_notice: Option<&str>,
    showing: Option<&str>,
    navigation: ReviewNavigation,
) -> usize {
    [None, copy_notice, showing]
        .into_iter()
        .flat_map(|notice| {
            [
                footer_items(
                    false,
                    applyable,
                    match_count,
                    false,
                    navigation,
                    footer::available_width(width, notice),
                ),
                footer_items(
                    true,
                    applyable,
                    match_count,
                    true,
                    navigation,
                    footer::available_width(width, notice),
                ),
                footer_items(
                    false,
                    applyable,
                    match_count.max(2),
                    true,
                    navigation,
                    footer::available_width(width, notice),
                ),
                required_footer_items(false, match_count, false, navigation),
                required_footer_items(true, match_count, true, navigation),
                required_footer_items(false, match_count.max(2), true, navigation),
            ]
            .into_iter()
            .map(move |items| {
                footer::layout_prioritized(items, footer::available_width(width, notice)).len()
            })
        })
        .max()
        .unwrap_or(1)
}

const FILTER_STATUS_WIDTH: usize = 10;

pub(super) fn filter_footer_status(query: &str, match_count: usize, width: u16) -> Option<String> {
    if query.is_empty() {
        return None;
    }
    if width >= 72 {
        Some(match match_count {
            0 => "No matches".to_owned(),
            1 => "1 match".to_owned(),
            count => format!("{count} matches"),
        })
    } else {
        None
    }
}

const COMPACT_POSITION_STATUS_WIDTH: u16 = 72;

pub(super) fn position_status(position: usize, total: usize, width: u16) -> String {
    let total = total.max(1);
    let position = position.saturating_add(1).min(total);
    if width >= COMPACT_POSITION_STATUS_WIDTH {
        format!("Line {position}/{total}")
    } else {
        format!("L{position}/{total}")
    }
}

pub(super) fn review_footer_status(
    state: &ReviewSessionState,
    view: &PlanReviewViewState,
    content: &PreparedContent<'_>,
    width: u16,
) -> String {
    let position = position_status_for_content(
        content,
        view.scroll().0,
        state.review().document().text().split('\n').count(),
        width,
    );
    filter_footer_status(state.review().search_query(), content.matches.len(), width).map_or_else(
        || position.clone(),
        |message| review_footer_status_text(&message, &position),
    )
}

pub(super) fn position_status_for_content(
    content: &PreparedContent<'_>,
    position: usize,
    total: usize,
    width: u16,
) -> String {
    let source_position = content
        .sources
        .get(position)
        .and_then(|source| source.as_ref())
        .map_or(position, |source| source.line_number);
    position_status(source_position, total, width)
}

pub(super) fn review_footer_status_text(message: &str, position: &str) -> String {
    format!("{message:<FILTER_STATUS_WIDTH$}  {position}")
}

// Narrow footers drop apply first; outside a filter they also keep quit and help ahead of other hints.
pub(super) fn footer_items(
    searching: bool,
    applyable: bool,
    _match_count: usize,
    filtered: bool,
    navigation: ReviewNavigation,
    width: u16,
) -> Vec<(u8, Line<'static>)> {
    let apply = |description| applyable.then(|| (0, footer::hint(&["a"], description)));
    let mut items = if searching {
        vec![
            footer::hint(&["Enter"], "confirm"),
            footer::hint(&["Esc"], "cancel"),
        ]
        .into_iter()
        .map(|item| (1, item))
        .collect()
    } else if filtered {
        let mut items = [
            footer::hint(&["Esc"], "clear / edit"),
            footer::hint(&["y"], "copy all"),
            footer::hint(&["?"], "help"),
            footer::hint(&["q"], "quit"),
        ]
        .into_iter()
        .map(|item| (1, item))
        .collect::<Vec<_>>();
        items.extend(apply("apply all"));
        items
    } else {
        let mut items = if navigation == ReviewNavigation::Standalone && width >= 29 {
            vec![
                (1, footer::hint(&["s"], "overview")),
                (1, footer::hint(&["/"], "filter")),
            ]
        } else {
            vec![(1, footer::hint(&["/"], "filter"))]
        };
        items.extend(apply("apply"));
        items.extend([
            (3, footer::hint(&["?"], "help")),
            (4, footer::hint(&["q"], "quit")),
        ]);
        items
    };
    if navigation == ReviewNavigation::Environments && !searching && !filtered {
        items.insert(0, (2, footer::hint(&["Esc"], "overview")));
    }
    items
}

pub(super) fn required_footer_items(
    searching: bool,
    match_count: usize,
    filtered: bool,
    navigation: ReviewNavigation,
) -> Vec<(u8, Line<'static>)> {
    let mut items = if searching {
        vec![
            (1, footer::hint(&["Enter"], "confirm")),
            (1, footer::hint(&["Esc"], "cancel")),
        ]
    } else if filtered {
        let mut items = vec![
            (1, footer::hint(&["Esc"], "clear / edit")),
            (1, footer::hint(&["y"], "copy all")),
            (1, footer::hint(&["?"], "help")),
            (1, footer::hint(&["q"], "quit")),
        ];
        if match_count >= 2 {
            items.insert(1, (1, footer::hint(&["n/N"], "next/prev")));
        }
        items
    } else {
        vec![
            (1, footer::hint(&["/"], "filter")),
            (3, footer::hint(&["?"], "help")),
            (4, footer::hint(&["q"], "quit")),
        ]
    };
    if navigation == ReviewNavigation::Environments && !searching && !filtered {
        items.insert(0, (2, footer::hint(&["Esc"], "overview")));
    }
    items
}
