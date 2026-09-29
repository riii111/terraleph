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
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case::above_window(12, (10, 11), 8, 10)]
    #[case::below_window(0, (10, 11), 8, 4)]
    #[case::inside_window(6, (10, 11), 8, 6)]
    #[case::single_line_at_window_end(0, (7, 7), 8, 0)]
    #[case::single_line_past_window_end(0, (8, 8), 8, 1)]
    #[case::range_taller_than_window(3, (10, 20), 4, 10)]
    fn moves_the_offset_only_as_far_as_the_range_needs(
        #[case] offset: usize,
        #[case] lines: (usize, usize),
        #[case] height: usize,
        #[case] expected: usize,
    ) {
        assert_eq!(offset_showing_range(offset, lines, height), expected);
    }
}
