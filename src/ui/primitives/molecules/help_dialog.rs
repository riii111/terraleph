use ratatui::{
    Frame,
    layout::Rect,
    style::Modifier,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};

use crate::ui::{
    primitives::{
        atoms::scrollbar,
        molecules::{dialog_scroll::DialogScroll, terminal_notice},
    },
    shell::footer,
    theme,
};

const MAX_WIDTH: u16 = 80;
const MAX_HEIGHT: u16 = 30;
const MIN_WIDTH: u16 = 20;
const MIN_HEIGHT: u16 = 6;
const HORIZONTAL_PADDING: u16 = 1;
// Borders, padding on both sides, and the vertical scrollbar column with its gap.
const CHROME_WIDTH: u16 = 6;

pub(crate) struct HelpAction {
    keys: &'static str,
    description: String,
}

impl HelpAction {
    pub(crate) fn new(keys: &'static str, description: impl Into<String>) -> Self {
        Self {
            keys,
            description: description.into(),
        }
    }
}

pub(crate) struct HelpSection {
    title: &'static str,
    actions: Vec<HelpAction>,
}

impl HelpSection {
    pub(crate) const fn new(title: &'static str, actions: Vec<HelpAction>) -> Self {
        Self { title, actions }
    }
}

// Rows never wrap so keys and descriptions stay aligned; wide rows scroll horizontally instead.
pub(crate) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &'static str,
    sections: &[HelpSection],
    scroll: &DialogScroll,
) {
    let lines = help_lines(sections);
    let content_width = lines.iter().map(Line::width).max().unwrap_or_default();
    let width = area.width.saturating_sub(2).min(MAX_WIDTH).min(
        u16::try_from(content_width)
            .unwrap_or(u16::MAX)
            .saturating_add(CHROME_WIDTH)
            .max(MIN_WIDTH),
    );
    let viewport_width = width.saturating_sub(CHROME_WIDTH);
    let overflows = content_width > usize::from(viewport_width);
    let inner_width = width.saturating_sub(2);
    let footer_width = inner_width.saturating_sub(HORIZONTAL_PADDING.saturating_mul(2));
    let mut hints = Vec::new();
    if overflows {
        hints.push(footer::hint(&["←", "→"], "scroll"));
    }
    hints.push(footer::hint(&["?", "Esc"], "close"));
    let footer_lines = footer::layout(hints, footer_width);
    let footer_height = u16::try_from(footer_lines.len()).unwrap_or(u16::MAX);
    let horizontal_bar_height = u16::from(overflows);
    let height = u16::try_from(lines.len())
        .unwrap_or(u16::MAX)
        .saturating_add(2)
        .saturating_add(horizontal_bar_height)
        .saturating_add(footer_height)
        .min(MAX_HEIGHT)
        .min(area.height.saturating_sub(2));
    let body_height = height
        .saturating_sub(2)
        .saturating_sub(footer_height)
        .saturating_sub(horizontal_bar_height);
    if width < MIN_WIDTH || area.height < MIN_HEIGHT || body_height == 0 {
        terminal_notice::render_wrapped(frame, area, "Terminal too small. Resize or press Esc.");
        return;
    }

    dim_background(frame, area);

    let dialog = Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, dialog);
    let block = Block::bordered()
        .border_style(theme::frame_style())
        .style(theme::body_style())
        .title(title)
        .title_style(theme::accent_style().add_modifier(Modifier::BOLD));
    let inner = block.inner(dialog);
    frame.render_widget(block, dialog);

    let content = Rect::new(
        inner.x.saturating_add(HORIZONTAL_PADDING),
        inner.y,
        viewport_width,
        body_height,
    );
    let footer_area = Rect::new(
        inner.x.saturating_add(HORIZONTAL_PADDING),
        inner.bottom().saturating_sub(footer_height),
        footer_width,
        footer_height,
    );
    let max_row = lines.len().saturating_sub(usize::from(content.height));
    let max_column = content_width.saturating_sub(usize::from(content.width));
    let row = scroll.clamp_for_render(u16::try_from(max_row).unwrap_or(u16::MAX));
    let column = scroll.clamp_column_for_render(u16::try_from(max_column).unwrap_or(u16::MAX));
    let total_rows = lines.len();
    frame.render_widget(
        Paragraph::new(lines)
            .style(theme::body_style())
            .scroll((row, column)),
        content,
    );
    scrollbar::render_vertical(
        frame,
        Rect::new(
            content.right().saturating_add(1),
            content.y,
            1,
            content.height,
        ),
        total_rows,
        usize::from(content.height),
        usize::from(row),
    );
    if overflows {
        scrollbar::render_horizontal(
            frame,
            Rect::new(content.x, content.bottom(), content.width, 1),
            content_width,
            usize::from(content.width),
            usize::from(column),
        );
    }
    footer::render(frame, footer_area, &footer_lines, None);
}

fn help_lines(sections: &[HelpSection]) -> Vec<Line<'_>> {
    let key_width = sections
        .iter()
        .flat_map(|section| &section.actions)
        .map(|action| Line::from(action.keys).width())
        .max()
        .unwrap_or_default();
    let mut lines = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        lines.push(Line::styled(
            section.title,
            theme::accent_style().add_modifier(Modifier::BOLD),
        ));
        for action in &section.actions {
            let padding = key_width.saturating_sub(Line::from(action.keys).width()) + 1;
            lines.push(Line::from(vec![
                Span::styled(action.keys, theme::accent_style()),
                Span::raw(" ".repeat(padding)),
                Span::raw(action.description.as_str()),
            ]));
        }
    }
    lines
}

fn dim_background(frame: &mut Frame<'_>, area: Rect) {
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = frame.buffer_mut().cell_mut((x, y)) {
                cell.set_style(cell.style().add_modifier(Modifier::DIM));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::ui::test_support::{buffer_text, render_to_buffer};

    use super::{DialogScroll, HelpAction, HelpSection, MAX_HEIGHT, MAX_WIDTH, render};

    fn render_text(size: (u16, u16), sections: &[HelpSection], scroll: &DialogScroll) -> String {
        buffer_text(&render_to_buffer(size, |frame| {
            render(frame, frame.area(), "Help", sections, scroll);
        }))
    }

    #[test]
    fn rows_keep_keys_and_descriptions_on_one_line_and_scroll_wide_rows_horizontally() {
        let description = "no differences in Ready plans; unknown values may differ";
        let sections = [
            HelpSection::new(
                "Overview",
                vec![
                    HelpAction::new("Space", "toggle a selected ▸/▾ group row"),
                    HelpAction::new("Enter", "open plan detail"),
                ],
            ),
            HelpSection::new(
                "Comparison",
                vec![HelpAction::new("Same changes", description)],
            ),
        ];
        let mut scroll = DialogScroll::default();

        let narrow = render_text((40, 24), &sections, &scroll);
        let lines = narrow.lines().collect::<Vec<_>>();
        let row = |key: &str| {
            *lines
                .iter()
                .find(|line| line.contains(key))
                .unwrap_or_else(|| panic!("{key} should be visible\n{narrow}"))
        };
        assert!(row("Space").contains("toggle a"), "{narrow}");
        assert!(row("Same changes").contains("no differ"), "{narrow}");
        assert!(!narrow.contains("values may differ"), "{narrow}");
        assert!(narrow.contains('◀'), "{narrow}");

        for _ in 0..20 {
            scroll.scroll_right();
        }
        let scrolled = render_text((40, 24), &sections, &scroll);
        assert!(scrolled.contains("values may differ"), "{scrolled}");

        let wide = render_text((120, 40), &sections, &DialogScroll::default());
        assert!(wide.contains(description), "{wide}");
        assert!(!wide.contains('◀'), "{wide}");
    }

    #[test]
    fn dialog_size_stays_within_the_maximum_on_large_terminals() {
        let long = "x".repeat(200);
        let many = (0..100)
            .map(|_| HelpAction::new("k", "row"))
            .chain([HelpAction::new("wide", long)])
            .collect();
        let sections = [HelpSection::new("Keys", many)];

        let text = render_text((200, 80), &sections, &DialogScroll::default());
        let lines = text.lines().collect::<Vec<_>>();
        let top = lines
            .iter()
            .position(|line| line.contains("┌Help"))
            .expect("dialog top border");
        let bottom = lines
            .iter()
            .rposition(|line| line.contains('└'))
            .expect("dialog bottom border");
        let top_line = lines[top].chars().collect::<Vec<_>>();
        let left = top_line.iter().position(|&symbol| symbol == '┌');
        let right = top_line.iter().rposition(|&symbol| symbol == '┐');

        assert_eq!(bottom - top + 1, usize::from(MAX_HEIGHT), "{text}");
        assert_eq!(
            left.zip(right).map(|(left, right)| right - left + 1),
            Some(usize::from(MAX_WIDTH)),
            "{text}"
        );
    }

    #[test]
    fn short_terminals_show_the_notice_instead_of_an_empty_body() {
        let sections = [HelpSection::new(
            "Keys",
            vec![HelpAction::new("wide", "x".repeat(100))],
        )];

        let text = render_text((40, 6), &sections, &DialogScroll::default());

        assert!(text.contains("Terminal too small"), "{text}");
        assert!(!text.contains("┌Help"), "{text}");
    }
}
