use std::fmt::Display;

use ratatui::text::{Line, Span};

use crate::ui::theme;

pub(crate) fn restyle_lines(lines: impl IntoIterator<Item = impl Display>) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|line| Line::from(Span::styled(line.to_string(), theme::copy_flash_style())))
        .collect()
}

#[cfg(test)]
mod tests {
    use ratatui::style::Style;

    use super::*;

    #[test]
    fn replaces_span_styles_with_the_flash_style_and_keeps_the_text() {
        let lines = [Line::from(vec![
            Span::styled("plain ", Style::new().bold()),
            Span::raw("text"),
        ])];

        let flashed = restyle_lines(&lines);

        assert_eq!(flashed.len(), 1);
        assert_eq!(flashed[0].to_string(), "plain text");
        assert_eq!(flashed[0].spans.len(), 1);
        assert_eq!(flashed[0].spans[0].style, theme::copy_flash_style());
    }
}
