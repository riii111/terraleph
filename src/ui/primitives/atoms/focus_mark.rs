use ratatui::text::Span;

use crate::ui::theme;

pub(crate) fn render(focused: bool) -> Span<'static> {
    Span::styled(
        if focused { "* " } else { "  " },
        theme::relation_frame_style(focused),
    )
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::focused(true, "* ")]
    #[case::unfocused(false, "  ")]
    fn marks_only_the_focused_pane(#[case] focused: bool, #[case] expected: &str) {
        let mark = render(focused);

        assert_eq!(mark.content, expected);
        assert_eq!(mark.style, theme::relation_frame_style(focused));
    }
}
