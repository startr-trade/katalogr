//! Byte offsets to 1-based `(line, column)` spans. Columns count characters; a span's end is
//! inclusive, like [`katalogr_model::Span`].
//!
//! A copy of `katalogr-ext-kanbanr/src/lines.rs`, with a cursor so that offsets asked for in
//! increasing order (the way every parser here asks) cost linear time even on one long line.

use katalogr_model::Span;
use std::cell::Cell;

pub struct Lines<'a> {
    text: &'a str,
    /// Byte offset of the start of each line.
    starts: Vec<usize>,
    ascii: bool,
    /// (line, byte offset, column) of the last answer, to continue counting from.
    cursor: Cell<(usize, usize, usize)>,
}

impl<'a> Lines<'a> {
    pub fn new(text: &'a str) -> Lines<'a> {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Lines {
            text,
            starts,
            ascii: text.is_ascii(),
            cursor: Cell::new((0, 0, 0)),
        }
    }

    /// 1-based line and column of a byte offset (clamped to the text).
    pub fn position(&self, offset: usize) -> (u32, u32) {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = self.starts.partition_point(|&s| s <= offset) - 1;
        let col = if self.ascii {
            offset - self.starts[line]
        } else {
            let (cl, co, cc) = self.cursor.get();
            let (from, base) = if cl == line && co <= offset {
                (co, cc)
            } else {
                (self.starts[line], 0)
            };
            let col = base + self.text[from..offset].chars().count();
            self.cursor.set((line, offset, col));
            col
        };
        (line as u32 + 1, col as u32 + 1)
    }

    /// The span of the half-open byte range `start..end`; the end is made inclusive.
    pub fn span(&self, start: usize, end: usize) -> Span {
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
        // Out-of-order asks stay correct.
        assert_eq!(l.position(5), (2, 2));
        assert_eq!(l.position(3), (2, 1));
    }
}
