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
    use super::*;

    struct MarkCase {
        name: &'static str,
        focused: bool,
        expected: &'static str,
    }

    #[test]
    fn marks_only_the_focused_pane() {
        for case in [
            MarkCase {
                name: "focused",
                focused: true,
                expected: "* ",
            },
            MarkCase {
                name: "unfocused",
                focused: false,
                expected: "  ",
            },
        ] {
            let mark = render(case.focused);

            assert_eq!(mark.content, case.expected, "case: {}", case.name);
            assert_eq!(
                mark.style,
                theme::relation_frame_style(case.focused),
                "case: {}",
                case.name
            );
        }
    }
}
