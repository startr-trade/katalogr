//! Byte offsets to 1-based `(line, column)` spans. Columns count characters; a span's end is
//! inclusive, like [`katalogr_model::Span`].

use katalogr_model::Span;

pub struct Lines<'a> {
    text: &'a str,
    /// Byte offset of the start of each line.
    starts: Vec<usize>,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str) -> Lines<'a> {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Lines { text, starts }
    }

    /// Byte offset of the start of the 1-based `line` (the end of the text when past the last).
    pub fn line_start(&self, line: u32) -> usize {
        self.starts
            .get((line as usize).saturating_sub(1))
            .copied()
            .unwrap_or(self.text.len())
    }

    /// 1-based line and column of a byte offset (clamped to the text).
    pub fn position(&self, offset: usize) -> (u32, u32) {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self.starts.partition_point(|&s| s <= offset) - 1;
        let col = self.text[self.starts[line]..offset].chars().count();
        (line as u32 + 1, col as u32 + 1)
    }

    /// The span of the half-open byte range `start..end`; the end is made inclusive.
    pub fn span(&self, start: u32, end: u32) -> Span {
        let (start, end) = (start as usize, end as usize);
        let (start_line, start_col) = self.position(start);
        let last = if end > start { end - 1 } else { start };
        let (end_line, end_col) = self.position(last);
        Span {
            start_line,
            start_col,
            end_line,
            end_col,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_and_inclusive_ends() {
        let l = Lines::new("ab\nçd\n");
        assert_eq!(l.position(0), (1, 1));
        assert_eq!(l.position(3), (2, 1));
        assert_eq!(l.position(5), (2, 2), "after a two-byte char");
        let s = l.span(0, 2);
        assert_eq!(
            (s.start_line, s.start_col, s.end_line, s.end_col),
            (1, 1, 1, 2)
        );
        let s = l.span(3, 7);
        assert_eq!(
            (s.start_line, s.start_col, s.end_line, s.end_col),
            (2, 1, 2, 3)
        );
    }
}
