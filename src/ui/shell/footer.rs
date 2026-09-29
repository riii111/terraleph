use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::ui::theme;

const SEPARATOR: &str = " | ";
const KEY_SEPARATOR: &str = "/";
const MAX_ROWS: usize = 2;

pub(crate) fn hint(alternative_keys: &[&'static str], description: &'static str) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, key) in alternative_keys.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(
                KEY_SEPARATOR,
                theme::footer_key_separator_style(),
            ));
        }
        spans.push(Span::styled(*key, theme::body_style()));
    }
    spans.push(Span::styled(
        format!(" {description}"),
        theme::footer_text_style(),
    ));
    Line::from(spans)
}

// A disabled hint keeps the same width so toggling it never reflows the footer.
pub(crate) fn disabled_hint(
    alternative_keys: &[&'static str],
    description: &'static str,
) -> Line<'static> {
    Line::from(
        hint(alternative_keys, description)
            .spans
            .into_iter()
            .map(|span| span.style(theme::footer_disabled_style()))
            .collect::<Vec<_>>(),
    )
}

pub(crate) fn overview_hint(
    alternative_keys: &[&'static str],
    description: &'static str,
) -> Line<'static> {
    let mut spans = Vec::new();
    for (index, key) in alternative_keys.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled(KEY_SEPARATOR, theme::overview_muted_style()));
        }
        spans.push(Span::styled(*key, theme::overview_footer_key_style()));
    }
    spans.push(Span::styled(
        format!(" {description}"),
        theme::overview_text_style(),
    ));
    Line::from(spans)
}

// The quit prompt replaces any copy notice, so it keeps the whole footer width. Screens also leave
// the notice undrawn while they ask, or a long notice would cut the prompt down to nothing.
pub(crate) fn quit_confirmation_lines(width: u16) -> Vec<Line<'static>> {
    let available = usize::from(width);
    let full = quit_confirmation_line();
    let compact = compact_quit_confirmation_line();
    let line = if full.width() <= available {
        full
    } else if compact.width() <= available {
        compact
    } else {
        minimal_quit_confirmation_line()
    };
    layout(vec![line], width)
}

pub(crate) fn pad_lines(mut lines: Vec<Line<'static>>, height: usize) -> Vec<Line<'static>> {
    lines.resize(height.max(1), Line::default());
    lines
}

fn quit_confirmation_line() -> Line<'static> {
    Line::from(vec![
        Span::styled(
            "Quit Terraleph?   ",
            theme::accent_style().add_modifier(Modifier::BOLD),
        ),
        Span::styled("[Enter]", theme::body_style().add_modifier(Modifier::BOLD)),
        Span::styled(" Quit   ", theme::footer_text_style()),
        Span::styled("[Esc]", theme::body_style().add_modifier(Modifier::BOLD)),
        Span::styled(" Cancel", theme::footer_text_style()),
    ])
}

fn compact_quit_confirmation_line() -> Line<'static> {
    Line::from(vec![
        Span::styled("Quit? ", theme::accent_style().add_modifier(Modifier::BOLD)),
        Span::styled("[Enter]", theme::body_style().add_modifier(Modifier::BOLD)),
        Span::styled(" quit ", theme::footer_text_style()),
        Span::styled("[Esc]", theme::body_style().add_modifier(Modifier::BOLD)),
        Span::styled(" cancel", theme::footer_text_style()),
    ])
}

fn minimal_quit_confirmation_line() -> Line<'static> {
    Line::from(vec![
        Span::styled("Quit? ", theme::accent_style().add_modifier(Modifier::BOLD)),
        Span::styled("[Enter]", theme::body_style().add_modifier(Modifier::BOLD)),
        Span::styled("/", theme::footer_key_separator_style()),
        Span::styled("[Esc]", theme::body_style().add_modifier(Modifier::BOLD)),
    ])
}

pub(crate) fn layout(items: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width);
    let mut rows = vec![Line::default()];
    let mut row_widths = vec![0usize];

    for item in items {
        let item_width = item.width();
        if item_width == 0 || item_width > width {
            continue;
        }

        let row_index = rows.len() - 1;
        let row = &mut rows[row_index];
        let separator_width = usize::from(!row.spans.is_empty()) * SEPARATOR.len();
        if row_widths[row_index] + separator_width + item_width <= width {
            if !row.spans.is_empty() {
                row.push_span(Span::styled(SEPARATOR, theme::footer_text_style()));
            }
            row.extend(item.spans);
            row_widths[row_index] += separator_width + item_width;
            continue;
        }

        if rows.len() == MAX_ROWS {
            continue;
        }

        rows.push(item);
        row_widths.push(item_width);
    }

    rows.into_iter()
        .filter(|row| !row.spans.is_empty())
        .collect()
}

pub(crate) fn layout_prioritized(
    items: Vec<(u8, Line<'static>)>,
    width: u16,
) -> Vec<Line<'static>> {
    let mut priority_order = (0..items.len()).collect::<Vec<_>>();
    priority_order.sort_by_key(|index| std::cmp::Reverse(items[*index].0));
    let mut selected = vec![false; items.len()];
    for index in priority_order {
        selected[index] = true;
        if !fits_in_rows(
            items
                .iter()
                .enumerate()
                .filter(|(position, _)| selected[*position])
                .map(|(_, (_, line))| line),
            usize::from(width),
        ) {
            selected[index] = false;
        }
    }
    layout(
        items
            .into_iter()
            .enumerate()
            .filter_map(|(index, (_, line))| selected[index].then_some(line))
            .collect(),
        width,
    )
}

fn fits_in_rows<'a>(items: impl Iterator<Item = &'a Line<'static>>, width: usize) -> bool {
    let mut rows = 1;
    let mut row_width = 0_usize;
    for item in items {
        let item_width = item.width();
        if item_width == 0 || item_width > width {
            return false;
        }
        let separator_width = usize::from(row_width > 0) * SEPARATOR.len();
        if row_width + separator_width + item_width <= width {
            row_width += separator_width + item_width;
        } else if rows < MAX_ROWS {
            rows += 1;
            row_width = item_width;
        } else {
            return false;
        }
    }
    true
}

pub(crate) fn layout_with_notice(
    items: Vec<Line<'static>>,
    width: u16,
    notice: Option<&str>,
) -> Vec<Line<'static>> {
    layout(items, available_width(width, notice))
}

pub(crate) fn available_width(width: u16, notice: Option<&str>) -> u16 {
    let Some(notice) = notice else {
        return width;
    };
    let notice_width = u16::try_from(Line::from(notice).width()).unwrap_or(u16::MAX);
    width.saturating_sub(notice_width.saturating_add(1))
}

pub(crate) fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    lines: &[Line<'static>],
    notice: Option<(&str, Style)>,
) {
    frame.render_widget(
        Paragraph::new(lines.to_owned()).style(theme::footer_text_style()),
        area,
    );
    let Some((message, style)) = notice else {
        return;
    };
    let notice_width = u16::try_from(Line::from(message).width())
        .unwrap_or(u16::MAX)
        .min(area.width);
    if notice_width == 0 || area.height == 0 {
        return;
    }
    let notice_area = Rect::new(
        area.right().saturating_sub(notice_width),
        area.y
            .saturating_add(u16::try_from(lines.len().saturating_sub(1)).unwrap_or(u16::MAX)),
        notice_width,
        1,
    );
    frame.render_widget(
        Paragraph::new(message)
            .style(style)
            .alignment(Alignment::Right),
        notice_area,
    );
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::{Buffer, Cell};
    use ratatui::style::Color;
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::single_row(80)]
    #[case::wrapped_rows(16)]
    fn rendered_keys_and_descriptions_use_rgb_colors_independent_of_ansi_palette(
        #[case] width: u16,
    ) {
        let backend = ratatui::backend::TestBackend::new(width, 2);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    &layout(
                        vec![hint(&["[", "]"], "prev/next"), hint(&["/"], "search")],
                        width,
                    ),
                    None,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_buffer_text_style(
            buffer,
            "[",
            0,
            Color::Rgb(0xe9, 0xdb, 0xdb),
            Color::Reset,
            Modifier::empty(),
        );
        assert_buffer_text_style(
            buffer,
            "]",
            0,
            Color::Rgb(0xe9, 0xdb, 0xdb),
            Color::Reset,
            Modifier::empty(),
        );
        assert_buffer_text_style(
            buffer,
            "/",
            0,
            Color::Rgb(0x90, 0x90, 0x90),
            Color::Reset,
            Modifier::empty(),
        );
        assert_buffer_text_style(
            buffer,
            "/",
            2,
            Color::Rgb(0xe9, 0xdb, 0xdb),
            Color::Reset,
            Modifier::empty(),
        );
        assert_buffer_text_style(
            buffer,
            " prev/next",
            0,
            Color::Rgb(0xc0, 0xb8, 0xb8),
            Color::Reset,
            Modifier::empty(),
        );
        assert_buffer_text_style(
            buffer,
            " search",
            0,
            Color::Rgb(0xc0, 0xb8, 0xb8),
            Color::Reset,
            Modifier::empty(),
        );
        if width == 80 {
            assert_buffer_text_style(
                buffer,
                " | ",
                0,
                Color::Rgb(0xc0, 0xb8, 0xb8),
                Color::Reset,
                Modifier::empty(),
            );
        }
    }

    #[test]
    fn notice_stays_at_the_right_edge_and_reserves_left_footer_space() {
        let width = 32;
        let notice = "Copied.";
        let lines = layout_with_notice(
            vec![
                hint(&["Ctrl-C"], "cancel"),
                hint(&["y"], "copy result"),
                hint(&["End"], "follow latest"),
            ],
            width,
            Some(notice),
        );
        assert!(
            lines
                .iter()
                .all(|line| { line.width() <= usize::from(available_width(width, Some(notice))) })
        );

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 2)).unwrap();
        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    &lines,
                    Some((notice, theme::accent_style())),
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let notice_width = notice.chars().count();
        for y in 0..buffer.area().height {
            for x in 0..width {
                let found = notice.chars().enumerate().all(|(offset, character)| {
                    let Some(cell) = buffer.cell((x + u16::try_from(offset).unwrap(), y)) else {
                        return false;
                    };

                    cell.symbol() == character.to_string()
                });

                if found {
                    assert_eq!(usize::from(x) + notice_width, usize::from(width));
                    return;
                }
            }
        }
        panic!("footer notice should be rendered");
    }

    fn assert_buffer_text_style(
        buffer: &Buffer,
        text: &str,
        occurrence: usize,
        foreground: Color,
        background: Color,
        modifier: Modifier,
    ) {
        for cell in buffer_text_cells(buffer, text, occurrence) {
            assert_eq!(cell.fg, foreground);
            assert_eq!(cell.bg, background);
            assert_eq!(cell.modifier, modifier);
        }
    }

    fn buffer_text_cells<'a>(buffer: &'a Buffer, text: &str, occurrence: usize) -> Vec<&'a Cell> {
        let mut matches = 0;
        let area = buffer.area();
        for y in area.y..area.bottom() {
            let symbols = (area.x..area.right())
                .map(|x| buffer.cell((x, y)).expect("footer cell").symbol())
                .collect::<Vec<_>>();
            for start in 0..symbols.len() {
                if !symbols[start..]
                    .iter()
                    .copied()
                    .collect::<String>()
                    .starts_with(text)
                {
                    continue;
                }
                if matches == occurrence {
                    return (0..text.chars().count())
                        .map(|offset| {
                            buffer
                                .cell((area.x + u16::try_from(start + offset).unwrap(), y))
                                .expect("footer cell")
                        })
                        .collect();
                }
                matches += 1;
            }
        }
        panic!("text occurrence not found: {text} #{occurrence}");
    }

    #[test]
    fn alternative_keys_use_the_same_separator() {
        let cases: &[(&str, &[&str], &str)] = &[
            ("single", &["Enter"], "Enter confirm"),
            ("two", &["Enter", "Space"], "Enter/Space confirm"),
        ];

        for (name, keys, expected) in cases {
            assert_eq!(hint(keys, "confirm").to_string(), *expected, "case: {name}");
        }
    }

    #[test]
    fn quit_confirmation_fits_the_prompt_and_emphasizes_the_question_and_both_keys() {
        struct QuitPromptCase {
            name: &'static str,
            width: u16,
            expected: &'static str,
        }

        for case in [
            QuitPromptCase {
                name: "full",
                width: 80,
                expected: "Quit Terraleph?   [Enter] Quit   [Esc] Cancel",
            },
            QuitPromptCase {
                name: "compact",
                width: 32,
                expected: "Quit? [Enter] quit [Esc] cancel",
            },
            QuitPromptCase {
                name: "minimal",
                width: 24,
                expected: "Quit? [Enter]/[Esc]",
            },
        ] {
            let lines = quit_confirmation_lines(case.width);
            let backend = ratatui::backend::TestBackend::new(case.width, 2);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|frame| {
                    render(frame, frame.area(), &lines, None);
                })
                .unwrap();

            assert_eq!(
                lines.iter().map(Line::to_string).collect::<Vec<_>>(),
                vec![case.expected.to_owned()],
                "case: {}",
                case.name
            );
            let buffer = terminal.backend().buffer();
            for (text, foreground) in [
                ("Quit", Color::Rgb(0xf4, 0x9e, 0x4c)),
                ("[Enter]", Color::Rgb(0xe9, 0xdb, 0xdb)),
                ("[Esc]", Color::Rgb(0xe9, 0xdb, 0xdb)),
            ] {
                for cell in buffer_text_cells(buffer, text, 0) {
                    assert_eq!(
                        (cell.fg, cell.bg, cell.modifier),
                        (foreground, Color::Reset, Modifier::BOLD),
                        "case: {}, text: {text}",
                        case.name
                    );
                }
            }
        }
    }

    #[rstest]
    #[case::single_line(40, vec!["q quit", "Esc back"], vec!["q quit | Esc back"])]
    #[case::two_lines(16, vec!["q quit", "Esc back", "y copy result"], vec!["q quit", "Esc back"])]
    #[case::wide_item_is_skipped(8, vec!["q quit", "longer than width"], vec!["q quit"])]
    fn lays_out_complete_items_in_at_most_two_rows(
        #[case] width: u16,
        #[case] items: Vec<&str>,
        #[case] expected: Vec<&str>,
    ) {
        let actual = layout(
            items
                .into_iter()
                .map(|item| Line::from(item.to_owned()))
                .collect(),
            width,
        );

        assert_eq!(
            actual.iter().map(Line::to_string).collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn prioritized_layout_keeps_help_and_quit_after_earlier_operations() {
        let lines = layout_prioritized(
            vec![
                (50, Line::from("Enter open selected row")),
                (110, Line::from("? help")),
                (120, Line::from("q quit")),
                (40, Line::from("b toggle envs")),
            ],
            14,
        );

        assert_eq!(
            lines.iter().map(Line::to_string).collect::<Vec<_>>(),
            ["? help", "q quit"]
        );
    }
}
