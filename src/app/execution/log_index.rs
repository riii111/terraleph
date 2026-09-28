use unicode_width::UnicodeWidthStr;

// Rendered-line layout of a log, kept up to date as entries are appended so a view can find the
// entries behind any line range without walking the whole log. An entry renders one line per
// `str::lines` item, and widths match the terminal cell width the renderer measures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct LogLineIndex {
    // The first rendered line of each entry, in append order.
    starts: Vec<usize>,
    line_count: usize,
    max_width: usize,
}

impl LogLineIndex {
    pub(super) fn push(&mut self, text: &str) {
        self.starts.push(self.line_count);
        for line in text.lines() {
            self.line_count += 1;
            self.max_width = self.max_width.max(line.width());
        }
    }

    #[must_use]
    pub(crate) const fn line_count(&self) -> usize {
        self.line_count
    }

    #[must_use]
    pub(crate) const fn max_width(&self) -> usize {
        self.max_width
    }

    // The entry holding `line`, as its position in append order, and the line's offset within
    // that entry.
    #[must_use]
    pub(crate) fn locate(&self, line: usize) -> Option<(usize, usize)> {
        if line >= self.line_count {
            return None;
        }
        // Entries without lines share their start with the next entry, so the last entry that
        // starts at or before `line` is the one that renders it.
        let entry = self.starts.partition_point(|start| *start <= line) - 1;
        Some((entry, line - self.starts[entry]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(entries: &[&str]) -> LogLineIndex {
        let mut index = LogLineIndex::default();
        for entry in entries {
            index.push(entry);
        }
        index
    }

    #[test]
    fn counts_rendered_lines_and_the_widest_one() {
        let index = index(&["first", "second\nthird line", "", "全角", "tail\r\n"]);

        assert_eq!(index.line_count(), 5);
        assert_eq!(index.max_width(), "third line".len());
        assert_eq!(index.locate(0), Some((0, 0)));
        assert_eq!(index.locate(1), Some((1, 0)));
        assert_eq!(index.locate(2), Some((1, 1)));
        assert_eq!(index.locate(3), Some((3, 0)));
        assert_eq!(index.locate(4), Some((4, 0)));
        assert_eq!(index.locate(5), None);
    }

    #[test]
    fn measures_wide_characters_by_terminal_cells() {
        assert_eq!(index(&["全角文字です"]).max_width(), 12);
        assert_eq!(index(&[""]).line_count(), 0);
        assert_eq!(index(&[""]).locate(0), None);
    }
}
