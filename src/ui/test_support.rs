use std::{env, fmt::Write, fs, path::PathBuf};

use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier};
use ratatui::{Frame, Terminal};

pub(super) fn render_to_buffer(
    (width, height): (u16, u16),
    render: impl FnOnce(&mut Frame<'_>),
) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal should be created");
    terminal.draw(render).expect("test frame should render");
    terminal.backend().buffer().clone()
}

pub(super) fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area();
    (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .filter_map(|x| buffer.cell((x, y)))
                .map(Cell::symbol)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// Rows inside the dialog frame whose top border starts with `┌{title}`, up to its footer.
// Scrollbar thumbs and arrows read as the track so rows compare by content only.
pub(super) fn dialog_body_rows(buffer: &Buffer, title: &str) -> Vec<String> {
    let area = buffer.area();
    let symbol = |x: u16, y: u16| buffer.cell((x, y)).expect("dialog cell").symbol();
    let marker = format!("┌{title}");
    let (top, left) = (area.y..area.bottom())
        .find_map(|y| {
            let symbols = (area.x..area.right())
                .map(|x| symbol(x, y))
                .collect::<Vec<_>>();
            (0..symbols.len())
                .find(|&start| symbols[start..].concat().starts_with(&marker))
                .map(|start| (y, area.x + u16::try_from(start).expect("dialog column")))
        })
        .unwrap_or_else(|| panic!("{marker} should be visible\n{}", buffer_text(buffer)));
    let right = (left + 1..area.right())
        .find(|&x| symbol(x, top) == "┐")
        .expect("dialog top-right corner");
    (top + 1..area.bottom())
        .map(|y| {
            (left + 1..right)
                .map(|x| match symbol(x, y) {
                    "┃" | "▲" | "▼" => "│",
                    other => other,
                })
                .collect::<String>()
        })
        .take_while(|row| !row.contains("close"))
        .collect()
}

pub(super) fn assert_dialog_scrolled_up(
    case: &str,
    before: &[String],
    after: &[String],
    lines: usize,
) {
    assert_eq!(
        before.len(),
        after.len(),
        "case: {case}: dialog height changed"
    );
    assert!(
        before.len() > lines,
        "case: {case}: dialog body should be taller than {lines} rows"
    );
    assert_eq!(
        after[lines..],
        before[..before.len() - lines],
        "case: {case}: dialog should scroll up by {lines} rows\nbefore:\n{}\nafter:\n{}",
        before.join("\n"),
        after.join("\n"),
    );
}

pub(super) fn buffer_visual_snapshot(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut snapshot = format!("{}x{}\n", area.width, area.height);
    for y in area.y..area.bottom() {
        let mut runs = Vec::new();
        let mut run_start = area.x;
        let mut run_style = None;
        let mut run_text = String::new();
        for x in area.x..area.right() {
            let cell = buffer.cell((x, y)).expect("snapshot cell");
            let style = (cell.fg, cell.bg, cell.modifier);
            if run_style.is_some_and(|active| active != style) {
                let (foreground, background, modifier) = run_style.expect("active style");
                runs.push(format!(
                    "{run_start}..{x} fg={foreground:?} bg={background:?} modifier={modifier:?} text={run_text:?}"
                ));
                run_text.clear();
                run_start = x;
            }
            run_style = Some(style);
            run_text.push_str(cell.symbol());
        }
        if let Some((foreground, background, modifier)) = run_style {
            runs.push(format!(
                "{run_start}..{} fg={foreground:?} bg={background:?} modifier={modifier:?} text={run_text:?}",
                area.right()
            ));
        }
        let _ = writeln!(snapshot, "row {y}: {}", runs.join(" | "));
    }
    snapshot
}

fn buffer_terminal_capture(buffer: &Buffer) -> String {
    let area = buffer.area();
    let mut capture = String::new();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let cell = buffer.cell((x, y)).expect("capture cell");
            capture.push_str("\x1b[0m");
            capture.push_str(&foreground_escape(cell.fg));
            capture.push_str(&background_escape(cell.bg));
            capture.push_str(&modifier_escape(cell.modifier));
            capture.push_str(cell.symbol());
        }
        capture.push_str("\x1b[0m\n");
    }
    capture
}

pub(super) fn write_buffer_captures(name: &str, buffer: &Buffer) {
    let Some(directory) = env::var_os("TERRALEPH_PREVIEW_CAPTURE_DIR").map(PathBuf::from) else {
        return;
    };
    fs::create_dir_all(&directory).expect("capture directory should be writable");
    fs::write(
        directory.join(format!("{name}.ansi")),
        buffer_terminal_capture(buffer),
    )
    .expect("ANSI capture should be writable");
    fs::write(directory.join(format!("{name}.txt")), buffer_text(buffer))
        .expect("text capture should be writable");
}

fn foreground_escape(color: Color) -> String {
    match color {
        Color::Rgb(red, green, blue) => format!("\x1b[38;2;{red};{green};{blue}m"),
        _ => "\x1b[39m".to_owned(),
    }
}

fn background_escape(color: Color) -> String {
    match color {
        Color::Rgb(red, green, blue) => format!("\x1b[48;2;{red};{green};{blue}m"),
        _ => "\x1b[49m".to_owned(),
    }
}

fn modifier_escape(modifier: Modifier) -> String {
    let mut escape = String::new();
    for (flag, code) in [
        (Modifier::BOLD, 1),
        (Modifier::DIM, 2),
        (Modifier::ITALIC, 3),
        (Modifier::UNDERLINED, 4),
        (Modifier::SLOW_BLINK, 5),
        (Modifier::RAPID_BLINK, 6),
        (Modifier::REVERSED, 7),
        (Modifier::HIDDEN, 8),
        (Modifier::CROSSED_OUT, 9),
    ] {
        if modifier.contains(flag) {
            let _ = write!(escape, "\x1b[{code}m");
        }
    }
    escape
}

pub(super) fn assert_shell_frame_and_footer(
    buffer: &Buffer,
    content: Rect,
    footer: Rect,
    footer_marker: &str,
) {
    assert_eq!(content.y + content.height, footer.y);
    assert!(content.height >= 2);
    assert_eq!(
        buffer.cell((content.x, content.y)).expect("frame cell").fg,
        Color::Rgb(0x76, 0x7a, 0x84)
    );
    let footer_text = (footer.y..footer.bottom())
        .flat_map(|y| (footer.x..footer.right()).filter_map(move |x| buffer.cell((x, y))))
        .map(Cell::symbol)
        .collect::<String>();
    assert!(footer_text.contains(footer_marker), "{footer_text}");
}
