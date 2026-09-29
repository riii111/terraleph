use ratatui::text::Span;

use crate::ui::theme;

// Every marker keeps two columns so the status words line up.
pub(crate) fn render() -> Span<'static> {
    Span::styled("✓ ", theme::overview_total_add_style())
}
