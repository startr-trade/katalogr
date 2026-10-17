//! Byte offset to line/column, for symbol and reference spans (1-based, end inclusive).

use katalogr_model::Span;

/// Line start offsets of one source text.
pub struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub fn new(text: &'a str) -> LineIndex<'a> {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        LineIndex { text, starts }
    }

    /// 1-based `(line, column)` of the character that starts at byte `offset`; columns count
    /// characters, not bytes.
    pub fn position(&self, offset: usize) -> (u32, u32) {
        let offset = offset.min(self.text.len());
        let line = self.starts.partition_point(|&s| s <= offset) - 1;
        let col = self.text[self.starts[line]..offset].chars().count();
        (line as u32 + 1, col as u32 + 1)
    }

    /// The span of the byte range `start..end` (end exclusive): the end is the position of the
    /// last character inside the range, so the span is end-inclusive. An empty range is the
    /// single position at `start`.
    pub fn span(&self, start: usize, end: usize) -> Span {
        let (start_line, start_col) = self.position(start);
        let last = if end > start {
            self.text[..end]
                .char_indices()
                .next_back()
                .map_or(start, |(i, _)| i)
        } else {
            start
        };
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
    fn positions_are_one_based_and_end_inclusive() {
        let text = "fn a() {}\nfn \u{e9}() {}\n";
        let ix = LineIndex::new(text);
        assert_eq!(ix.position(0), (1, 1));
        assert_eq!(ix.span(0, 9).end_col, 9);
        let s = ix.span(10, 20);
        assert_eq!((s.start_line, s.start_col), (2, 1));
        // `\u{e9}` is two bytes but one column
        assert_eq!((s.end_line, s.end_col), (2, 9));
        assert_eq!(ix.span(3, 3).end_col, 4);
    }
}
