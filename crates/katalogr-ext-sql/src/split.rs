//! Statement splitting (§6.1): one linear pass over the bytes that knows quotes, comments,
//! dollar quotes, `DELIMITER` lines and `BEGIN ... END` blocks. It is the only code that
//! decides statement boundaries.

use crate::dialect::Dialect;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawStatement {
    pub start: usize,
    /// Byte range end, exclusive.
    pub end: usize,
    pub delimiter: String,
}

/// Split a file into statements with the dialect unknown (backslash escapes and `#` comments
/// as in MySQL). Comment-only and client-command segments are dropped.
pub fn split_statements(text: &str) -> Vec<RawStatement> {
    split_statements_in(text, None)
}

/// Like [`split_statements`], with the file's dialect when known: backslash escapes in
/// strings and `#` comments apply to MySQL and to an unknown dialect only.
pub fn split_statements_in(text: &str, dialect: Option<Dialect>) -> Vec<RawStatement> {
    let mysql_like = matches!(dialect, None | Some(Dialect::MySql));
    Splitter {
        b: text.as_bytes(),
        backslash: mysql_like,
        hash_comments: mysql_like,
    }
    .run()
}

struct Splitter<'a> {
    b: &'a [u8],
    backslash: bool,
    hash_comments: bool,
}

fn is_ident(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

fn is_space(c: u8) -> bool {
    c.is_ascii_whitespace()
}

impl Splitter<'_> {
    fn run(&self) -> Vec<RawStatement> {
        let b = self.b;
        let n = b.len();
        let mut out = Vec::new();
        let mut delim: Vec<u8> = b";".to_vec();
        let mut i = 0;
        let mut start = 0usize;
        let mut has_token = false;
        let mut last_nonspace = 0usize;
        let mut depth: i32 = 0;
        let mut block_stmt: Option<bool> = None;

        macro_rules! reset {
            () => {{
                has_token = false;
                depth = 0;
                block_stmt = None;
            }};
        }

        while i < n {
            let c = b[i];
            if is_space(c) {
                i += 1;
                continue;
            }
            // Statement-start commands.
            if !has_token {
                if c == b'\\' {
                    i = self.line_end(i);
                    continue;
                }
                if (c == b'D' || c == b'd') && self.word_at(i).eq_ignore_ascii_case(b"delimiter") {
                    let after = i + "delimiter".len();
                    if after < n && (b[after] == b' ' || b[after] == b'\t') {
                        let eol = self.line_end(after);
                        let rest = &b[after..eol];
                        let word: Vec<u8> = rest
                            .iter()
                            .copied()
                            .skip_while(|c| is_space(*c))
                            .take_while(|c| !is_space(*c))
                            .collect();
                        if !word.is_empty() {
                            delim = word;
                        }
                        i = eol;
                        continue;
                    }
                }
            }
            // Comments.
            if c == b'-' && b.get(i + 1) == Some(&b'-') {
                let e = self.line_end(i);
                last_nonspace = last_nonspace.max(trim_end(b, i, e));
                i = e;
                continue;
            }
            if c == b'#'
                && self.hash_comments
                && (i == 0 || is_space(b[i - 1]) || b[i - 1] == b';')
                && !matches!(
                    b.get(i + 1),
                    Some(b'>' | b'-' | b'#' | b'=' | b'<' | b'~' | b'&' | b'|')
                )
            {
                let e = self.line_end(i);
                last_nonspace = last_nonspace.max(trim_end(b, i, e));
                i = e;
                continue;
            }
            if c == b'/' && b.get(i + 1) == Some(&b'*') {
                let e = self.block_comment_end(i);
                last_nonspace = last_nonspace.max(e);
                i = e;
                continue;
            }
            // The delimiter ends the statement outside blocks.
            if depth <= 0 && b[i..].starts_with(&delim) {
                if has_token {
                    out.push(RawStatement {
                        start,
                        end: last_nonspace,
                        delimiter: String::from_utf8_lossy(&delim).into_owned(),
                    });
                }
                reset!();
                i += delim.len();
                continue;
            }
            if !has_token {
                has_token = true;
                start = i;
            }
            // A token: find its end, tracking blocks.
            let tok_end = match c {
                b'\'' => self.quoted_end(i, b'\'', self.backslash || self.is_escape_string(i)),
                b'"' => self.quoted_end(i, b'"', false),
                b'`' => self.quoted_end(i, b'`', false),
                b'[' if self.bracket_ident(i) => self.bracket_end(i),
                b'$' if i == 0 || !is_ident(b[i - 1]) => self.dollar_end(i).unwrap_or(i + 1),
                c if is_ident(c) && c != b'$' => {
                    let e = self.word_end(i);
                    let w = &b[i..e];
                    let keyword = w.eq_ignore_ascii_case(b"begin")
                        || w.eq_ignore_ascii_case(b"case")
                        || w.eq_ignore_ascii_case(b"end");
                    if keyword && depth_applies(&delim, &mut block_stmt, b, start, i) {
                        if w.eq_ignore_ascii_case(b"begin") || w.eq_ignore_ascii_case(b"case") {
                            depth += 1;
                        } else if w.eq_ignore_ascii_case(b"end") {
                            let (next, ne) = self.next_word(e);
                            if next.eq_ignore_ascii_case(b"case") {
                                depth -= 1;
                                last_nonspace = ne;
                                i = ne;
                                continue;
                            } else if !(next.eq_ignore_ascii_case(b"if")
                                || next.eq_ignore_ascii_case(b"loop")
                                || next.eq_ignore_ascii_case(b"while")
                                || next.eq_ignore_ascii_case(b"repeat"))
                            {
                                depth -= 1;
                            }
                        }
                    }
                    e
                }
                _ => i + 1,
            };
            last_nonspace = tok_end;
            i = tok_end;
        }
        if has_token {
            out.push(RawStatement {
                start,
                end: last_nonspace,
                delimiter: String::from_utf8_lossy(&delim).into_owned(),
            });
        }
        out
    }

    fn line_end(&self, from: usize) -> usize {
        self.b[from..]
            .iter()
            .position(|c| *c == b'\n')
            .map_or(self.b.len(), |p| from + p)
    }

    fn word_end(&self, from: usize) -> usize {
        let mut e = from;
        while e < self.b.len() && is_ident(self.b[e]) {
            e += 1;
        }
        e
    }

    fn word_at(&self, from: usize) -> &[u8] {
        &self.b[from..self.word_end(from)]
    }

    /// The next word after `from` (skipping spaces), and the offset after it; empty if none.
    fn next_word(&self, from: usize) -> (&[u8], usize) {
        let mut s = from;
        while s < self.b.len() && is_space(self.b[s]) {
            s += 1;
        }
        let e = self.word_end(s);
        (&self.b[s..e], e)
    }

    fn block_comment_end(&self, from: usize) -> usize {
        let b = self.b;
        let mut depth = 0;
        let mut i = from;
        while i < b.len() {
            if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                depth += 1;
                i += 2;
            } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                depth -= 1;
                i += 2;
                if depth == 0 {
                    return i;
                }
            } else {
                i += 1;
            }
        }
        b.len()
    }

    /// `E'...'` strings use backslash escapes in every dialect.
    fn is_escape_string(&self, quote: usize) -> bool {
        quote > 0
            && matches!(self.b[quote - 1], b'e' | b'E')
            && (quote == 1 || !is_ident(self.b[quote - 2]))
    }

    /// End (exclusive) of a quoted run starting at `from` (the opening quote). A doubled quote
    /// is an escape; with `backslash`, so is `\x`.
    fn quoted_end(&self, from: usize, q: u8, backslash: bool) -> usize {
        let b = self.b;
        let mut i = from + 1;
        while i < b.len() {
            let c = b[i];
            if backslash && c == b'\\' {
                i += 2;
            } else if c == q {
                if b.get(i + 1) == Some(&q) {
                    i += 2;
                } else {
                    return i + 1;
                }
            } else {
                i += 1;
            }
        }
        b.len()
    }

    /// `[` starts a quoted identifier at an identifier position (not a subscript).
    fn bracket_ident(&self, i: usize) -> bool {
        if i > 0 {
            let p = self.b[i - 1];
            if is_ident(p) || p == b']' || p == b')' {
                return false;
            }
        }
        let rest = &self.b[i + 1..];
        let limit = rest.len().min(256);
        match rest[..limit].iter().position(|c| *c == b']' || *c == b'\n') {
            Some(p) => rest[p] == b']',
            None => false,
        }
    }

    fn bracket_end(&self, from: usize) -> usize {
        let b = self.b;
        let mut i = from + 1;
        while i < b.len() {
            if b[i] == b']' {
                if b.get(i + 1) == Some(&b']') {
                    i += 2;
                    continue;
                }
                return i + 1;
            }
            i += 1;
        }
        b.len()
    }

    /// End of a dollar-quoted string opened at `from`; None when `$` does not open one.
    fn dollar_end(&self, from: usize) -> Option<usize> {
        let b = self.b;
        let mut j = from + 1;
        if j < b.len() && (b[j].is_ascii_alphabetic() || b[j] == b'_') {
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
        }
        if b.get(j) != Some(&b'$') {
            return None;
        }
        let tag = &b[from..=j];
        let body = j + 1;
        match find(&b[body..], tag) {
            Some(p) => Some(body + p + tag.len()),
            None => Some(b.len()),
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Last non-space byte offset (exclusive) within `from..to`.
fn trim_end(b: &[u8], from: usize, to: usize) -> usize {
    let mut e = to;
    while e > from && is_space(b[e - 1]) {
        e -= 1;
    }
    e
}

/// Block depth is tracked only while the delimiter is `;` and the statement creates a trigger,
/// procedure or function. Decided once per statement, from its leading words.
fn depth_applies(
    delim: &[u8],
    cached: &mut Option<bool>,
    b: &[u8],
    start: usize,
    at: usize,
) -> bool {
    if delim != b";" {
        return false;
    }
    if at == start {
        return false;
    }
    *cached.get_or_insert_with(|| creates_routine(&b[start..at.min(start + 400)]))
}

fn creates_routine(head: &[u8]) -> bool {
    let mut words = Vec::new();
    let mut i = 0;
    while i < head.len() && words.len() < 14 {
        let c = head[i];
        if c == b'\'' || c == b'"' || c == b'`' {
            i += 1;
            while i < head.len() && head[i] != c {
                i += 1;
            }
            i += 1;
        } else if c.is_ascii_alphabetic() || c == b'_' {
            let s = i;
            while i < head.len() && (head[i].is_ascii_alphanumeric() || head[i] == b'_') {
                i += 1;
            }
            words.push(head[s..i].to_ascii_lowercase());
        } else {
            i += 1;
        }
    }
    if words.first().map(Vec::as_slice) != Some(b"create") {
        return false;
    }
    for w in &words[1..] {
        match w.as_slice() {
            b"trigger" | b"procedure" | b"function" => return true,
            b"table" | b"view" | b"index" | b"schema" | b"database" | b"type" | b"sequence"
            | b"policy" | b"extension" | b"domain" => return false,
            _ => {}
        }
    }
    false
}
