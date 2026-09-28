use std::path::Path;

use ratatui::buffer::CellWidth;
use ratatui::style::Style;
use ratatui::text::Line;

use crate::app::execution::{ExecutionContext, ExecutionContextValue, directory_display_name};

pub(crate) fn target(path: &Path) -> String {
    directory_display_name(path)
}

pub(crate) fn relative_directory(path: &Path, launch_root: Option<&Path>) -> String {
    launch_root
        .and_then(|root| path.strip_prefix(root).ok())
        .map_or_else(
            || path.display().to_string(),
            |relative| {
                if relative.as_os_str().is_empty() {
                    ".".to_owned()
                } else {
                    format!("./{}", relative.display())
                }
            },
        )
}

pub(crate) fn context_lines(context: &ExecutionContext) -> Vec<Line<'static>> {
    let workspace = match context.workspace() {
        ExecutionContextValue::Known(value) => value.clone(),
        ExecutionContextValue::Loading => "loading...".to_owned(),
    };
    let version = match context.tool_version() {
        ExecutionContextValue::Known(value) => value.clone(),
        ExecutionContextValue::Loading => "loading...".to_owned(),
    };
    let mut lines = vec![
        Line::from("Execution directory"),
        Line::from(format!("  {}", context.cwd_path().display())),
        Line::from(format!("Workspace: {workspace}")),
        Line::from(format!("Tool: {} {version}", context.tool_name())),
        Line::from("Variable sources"),
    ];
    let sources = context.variable_sources();
    for path in sources.automatic_files() {
        lines.push(Line::from(format!("  auto: {}", path.display())));
    }
    for path in sources.explicit_files() {
        lines.push(Line::from(format!("  -var-file: {}", path.display())));
    }
    if sources.has_var_argument() {
        lines.push(Line::from("  -var: provided"));
    }
    for name in sources.environment_variables() {
        lines.push(Line::from(format!("  {name}")));
    }
    if lines
        .last()
        .is_some_and(|line| line.to_string() == "Variable sources")
    {
        lines.push(Line::from("  none detected"));
    }
    lines
}

pub(crate) fn truncate_middle(value: &str, max_width: usize) -> String {
    let width = display_width(value);
    if width <= max_width {
        return value.to_owned();
    }
    if max_width <= 3 {
        return ".".repeat(max_width);
    }

    let remaining = max_width - 3;
    let prefix_width = remaining.div_ceil(2);
    let suffix_width = remaining - prefix_width;
    format!(
        "{}...{}",
        take_from_start(value, prefix_width),
        take_from_end(value, suffix_width)
    )
}

pub(crate) fn display_width(value: &str) -> usize {
    Line::from(value)
        .styled_graphemes(Style::default())
        .map(|grapheme| usize::from(grapheme.symbol.cell_width()))
        .sum()
}

pub(super) fn take_from_start(value: &str, max_width: usize) -> String {
    let mut result = String::new();
    let mut width = 0;
    for grapheme in Line::from(value).styled_graphemes(Style::default()) {
        let grapheme_width = usize::from(grapheme.symbol.cell_width());
        if width + grapheme_width > max_width {
            break;
        }
        result.push_str(grapheme.symbol);
        width += grapheme_width;
    }
    result
}

fn take_from_end(value: &str, max_width: usize) -> String {
    let graphemes = Line::from(value)
        .styled_graphemes(Style::default())
        .map(|grapheme| grapheme.symbol.to_owned())
        .collect::<Vec<_>>();
    let mut result = String::new();
    let mut width = 0;
    for grapheme in graphemes.iter().rev() {
        let grapheme_width = Line::from(grapheme.as_str()).width();
        if width + grapheme_width > max_width {
            break;
        }
        result.insert_str(0, grapheme);
        width += grapheme_width;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn middle_truncation_preserves_both_ends() {
        assert_eq!(truncate_middle("abcdef", 5), "a...f");
        assert_eq!(truncate_middle("abcdef", 3), "...");
    }
}
