use ratatui::style::{Color, Modifier, Style};

pub(crate) fn footer_key_separator_style() -> Style {
    Style::default().fg(Color::Rgb(0x90, 0x90, 0x90))
}

pub(crate) fn footer_disabled_style() -> Style {
    Style::default().fg(Color::Rgb(0x6c, 0x70, 0x78))
}

pub(crate) fn footer_text_style() -> Style {
    Style::default().fg(Color::Rgb(0xc0, 0xb8, 0xb8))
}

pub(crate) fn body_style() -> Style {
    Style::default().fg(Color::Rgb(0xe9, 0xdb, 0xdb))
}

pub(crate) fn overview_text_style() -> Style {
    Style::default().fg(Color::Reset).bg(Color::Reset)
}

pub(crate) fn overview_muted_style() -> Style {
    Style::default().fg(Color::DarkGray).bg(Color::Reset)
}

pub(crate) fn overview_selection_marker_style() -> Style {
    Style::default().fg(Color::Green).bg(Color::Reset)
}

pub(crate) fn overview_section_heading_style() -> Style {
    overview_text_style().add_modifier(Modifier::BOLD)
}

pub(crate) fn overview_pane_title_style() -> Style {
    overview_text_style().add_modifier(Modifier::BOLD)
}

pub(crate) fn overview_header_selected_style() -> Style {
    Style::default()
        .fg(Color::Reset)
        .bg(Color::Reset)
        .add_modifier(Modifier::UNDERLINED)
}

pub(crate) fn overview_total_add_style() -> Style {
    Style::default().fg(Color::Green).bg(Color::Reset)
}

pub(crate) fn overview_total_update_style() -> Style {
    Style::default().fg(Color::Yellow).bg(Color::Reset)
}

pub(crate) fn overview_total_destroy_style() -> Style {
    Style::default().fg(Color::Red).bg(Color::Reset)
}

pub(crate) fn overview_total_replace_style() -> Style {
    Style::default().fg(Color::Magenta).bg(Color::Reset)
}

pub(crate) fn overview_warning_style() -> Style {
    Style::default().fg(Color::Yellow).bg(Color::Reset)
}

pub(crate) fn relation_warning_style() -> Style {
    overview_warning_style().add_modifier(Modifier::BOLD)
}

pub(crate) fn relation_difference_style() -> Style {
    overview_text_style().add_modifier(Modifier::BOLD)
}

pub(crate) fn relation_frame_style(focused: bool) -> Style {
    Style::default()
        .fg(if focused {
            Color::Cyan
        } else {
            Color::DarkGray
        })
        .bg(Color::Reset)
}

pub(crate) fn overview_footer_key_style() -> Style {
    Style::default().fg(Color::Yellow).bg(Color::Reset)
}

pub(crate) fn secondary_style() -> Style {
    Style::default().fg(Color::Rgb(0xc0, 0xb8, 0xb8))
}

pub(crate) fn accent_style() -> Style {
    Style::default().fg(Color::Rgb(0xf4, 0x9e, 0x4c))
}

pub(crate) fn search_match_style() -> Style {
    Style::default()
        .fg(Color::Rgb(0x11, 0x14, 0x19))
        .bg(Color::Rgb(0xf4, 0x9e, 0x4c))
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn search_cursor_style() -> Style {
    Style::default()
        .fg(Color::Rgb(0x11, 0x14, 0x19))
        .bg(Color::Rgb(0xf4, 0x9e, 0x4c))
}

pub(crate) fn selected_search_match_style() -> Style {
    Style::default()
        .fg(Color::Rgb(0x11, 0x14, 0x19))
        .bg(Color::Rgb(0xff, 0xd0, 0x8a))
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
}

pub(crate) fn copy_flash_style() -> Style {
    Style::default()
        .fg(Color::Rgb(0x11, 0x14, 0x19))
        .bg(Color::Rgb(0xf4, 0x9e, 0x4c))
}

pub(crate) fn frame_style() -> Style {
    Style::default().fg(Color::Rgb(0x76, 0x7a, 0x84))
}

pub(crate) fn separator_style() -> Style {
    Style::default().fg(Color::Rgb(0x85, 0x8b, 0x94))
}

pub(crate) fn scrollbar_thumb_style() -> Style {
    Style::default().fg(Color::Rgb(0xc0, 0xb8, 0xb0))
}

pub(crate) fn scrollbar_track_style() -> Style {
    Style::default().fg(Color::Rgb(0x50, 0x52, 0x5e))
}

pub(crate) fn warning_style() -> Style {
    Style::default()
        .fg(Color::Rgb(0xeb, 0xcb, 0x8b))
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn error_style() -> Style {
    Style::default()
        .fg(Color::Rgb(0xbf, 0x61, 0x6a))
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn success_style() -> Style {
    Style::default()
        .fg(Color::Rgb(0xa3, 0xbe, 0x8c))
        .add_modifier(Modifier::BOLD)
}

pub(crate) fn plan_line_style(line: &str) -> Style {
    let line = line.trim_start();
    if line.starts_with("-/+") || line.starts_with("+/-") {
        return overview_total_replace_style();
    }
    plan_marker_style(line.chars().next())
}

pub(crate) fn plan_marker_style(marker: Option<char>) -> Style {
    match marker {
        Some('+') => Style::default().fg(Color::Rgb(0xa3, 0xbe, 0x8c)),
        Some('-') => Style::default().fg(Color::Rgb(0xbf, 0x61, 0x6a)),
        Some('~') => Style::default().fg(Color::Rgb(0xeb, 0xcb, 0x8b)),
        _ => body_style(),
    }
}

pub(crate) fn plan_resource_header_style() -> Style {
    body_style().add_modifier(Modifier::BOLD)
}

pub(crate) const fn plan_hidden_value_style(line_style: Style) -> Style {
    line_style.add_modifier(Modifier::DIM)
}

pub(crate) const fn plan_change_arrow_style(line_style: Style) -> Style {
    line_style.add_modifier(Modifier::BOLD)
}
