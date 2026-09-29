/// Returns the offset that shows the inclusive `lines` range in a window of `height` lines,
/// moving from `offset` only as far as needed. A range taller than the window shows its first
/// line.
pub(crate) const fn offset_showing_range(
    offset: usize,
    lines: (usize, usize),
    height: usize,
) -> usize {
    let (first, last) = lines;
    if last.saturating_sub(first).saturating_add(1) > height || first < offset {
        return first;
    }
    if last >= offset.saturating_add(height) {
        return last.saturating_add(1).saturating_sub(height);
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OffsetCase {
        name: &'static str,
        offset: usize,
        lines: (usize, usize),
        height: usize,
        expected: usize,
    }

    #[test]
    fn moves_the_offset_only_as_far_as_the_range_needs() {
        for case in [
            OffsetCase {
                name: "above_window",
                offset: 12,
                lines: (10, 11),
                height: 8,
                expected: 10,
            },
            OffsetCase {
                name: "below_window",
                offset: 0,
                lines: (10, 11),
                height: 8,
                expected: 4,
            },
            OffsetCase {
                name: "inside_window",
                offset: 6,
                lines: (10, 11),
                height: 8,
                expected: 6,
            },
            OffsetCase {
                name: "single_line_at_window_end",
                offset: 0,
                lines: (7, 7),
                height: 8,
                expected: 0,
            },
            OffsetCase {
                name: "single_line_past_window_end",
                offset: 0,
                lines: (8, 8),
                height: 8,
                expected: 1,
            },
            OffsetCase {
                name: "range_taller_than_window",
                offset: 3,
                lines: (10, 20),
                height: 4,
                expected: 10,
            },
        ] {
            assert_eq!(
                offset_showing_range(case.offset, case.lines, case.height),
                case.expected,
                "case: {}",
                case.name
            );
        }
    }
}
