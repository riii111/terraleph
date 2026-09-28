use ratatui::style::Style;

use crate::{app::plan::PlanSummary, ui::theme};

pub(crate) struct ChangeCount {
    pub(crate) count: usize,
    pub(crate) text: String,
    pub(crate) style: Style,
}

/// Labels and colors each change count in one place, so the review header and
/// the apply confirmation always show a change kind in the same color.
pub(crate) fn change_counts(counts: PlanSummary) -> [ChangeCount; 4] {
    [
        ChangeCount {
            count: counts.creates,
            text: format!("+{} add", counts.creates),
            style: theme::success_style(),
        },
        ChangeCount {
            count: counts.updates,
            text: format!("~{} update", counts.updates),
            style: theme::warning_style(),
        },
        ChangeCount {
            count: counts.replaces,
            text: format!("{} replace", counts.replaces),
            style: theme::overview_total_replace_style(),
        },
        ChangeCount {
            count: counts.deletes,
            text: format!("-{} destroy", counts.deletes),
            style: theme::error_style(),
        },
    ]
}
