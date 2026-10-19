//! The lexical fallback (§6.3): a tolerant tokenizer, the object header recogniser and the
//! reference scanner for statements and procedural bodies that the parser cannot take.
//! Nothing here can fail; every function is linear in the text.

use crate::names::{is_system_relation, normalise_ident, routine_key, table_key};
use std::collections::BTreeSet;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TokKind {
    /// An unquoted word (keyword or identifier).
    Word,
    /// A quoted identifier: `"x"`, backticks or `[x]`.
    Quoted,
    /// A string literal (skipped by the scanner).
    Str,
    /// A dollar-quoted string; the inner byte range is scanned once.
    Dollar {
        inner_start: usize,
        inner_end: usize,
    },
    Num,
    Punct(u8),
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Tok {
    pub kind: TokKind,
    pub start: usize,
    pub end: usize,
}

impl Tok {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }

    fn is_kw(&self, src: &str, kw: &str) -> bool {
        self.kind == TokKind::Word && self.text(src).eq_ignore_ascii_case(kw)
    }

    fn is_name(&self) -> bool {
        matches!(self.kind, TokKind::Word | TokKind::Quoted)
    }

    fn is_punct(&self, c: u8) -> bool {
        self.kind == TokKind::Punct(c)
    }
}

fn ident_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// Tokenize `text`. Comments and whitespace are dropped. Total: unterminated quotes run to the
/// end of the text.
pub fn tokens(text: &str) -> Vec<Tok> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        if c == b'-' && b.get(i + 1) == Some(&b'-') {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'#'
            && (i == 0 || b[i - 1].is_ascii_whitespace() || b[i - 1] == b';')
            && !matches!(
                b.get(i + 1),
                Some(b'>' | b'-' | b'#' | b'=' | b'<' | b'~' | b'&' | b'|')
            )
        {
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && b.get(i + 1) == Some(&b'*') {
            let mut depth = 0;
            while i < n {
                if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                    depth += 1;
                    i += 2;
                } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            continue;
        }
        let kind = match c {
            b'\'' => {
                i = quoted_end(b, i, b'\'', true);
                TokKind::Str
            }
            b'"' | b'`' => {
                i = quoted_end(b, i, c, false);
                TokKind::Quoted
            }
            b'[' if bracket_ident(b, i) => {
                i = quoted_end(b, i, b']', false).max(i + 1);
                TokKind::Quoted
            }
            b'$' if i == 0 || !ident_char(b[i - 1]) => match dollar(b, i) {
                Some((inner_start, inner_end, end)) => {
                    i = end;
                    TokKind::Dollar {
                        inner_start,
                        inner_end,
                    }
                }
                None => {
                    i += 1;
                    while i < n && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    TokKind::Num
                }
            },
            c if ident_start(c) => {
                i += 1;
                while i < n && ident_char(b[i]) {
                    i += 1;
                }
                TokKind::Word
            }
            c if c.is_ascii_digit() => {
                i += 1;
                while i < n && (ident_char(b[i]) || b[i] == b'.') {
                    i += 1;
                }
                TokKind::Num
            }
            c => {
                i += 1;
                TokKind::Punct(c)
            }
        };
        // `[x]` quoting reuses the closing-quote search for `]`.
        out.push(Tok {
            kind,
            start,
            end: i.min(n),
        });
    }
    out
}

/// Exclusive end of a quoted run opened at `from`. Doubled quotes escape; with `backslash`, so
/// does `\x`. For `]` the opener is `[`.
fn quoted_end(b: &[u8], from: usize, q: u8, backslash: bool) -> usize {
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

fn bracket_ident(b: &[u8], i: usize) -> bool {
    if i > 0 {
        let p = b[i - 1];
        if ident_char(p) || p == b']' || p == b')' {
            return false;
        }
    }
    let rest = &b[i + 1..];
    let limit = rest.len().min(256);
    match rest[..limit].iter().position(|c| *c == b']' || *c == b'\n') {
        Some(p) => rest[p] == b']',
        None => false,
    }
}

/// `(inner_start, inner_end, end)` of a dollar-quoted string opened at `from`.
fn dollar(b: &[u8], from: usize) -> Option<(usize, usize, usize)> {
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
    match b[body..].windows(tag.len()).position(|w| w == tag) {
        Some(p) => Some((body, body + p, body + p + tag.len())),
        None => Some((body, b.len(), b.len())),
    }
}

// --- header recognition --------------------------------------------------------------------

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    View,
    MaterializedView,
    Function,
    Procedure,
    Trigger,
    Policy,
    Anonymous,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Ddl {
    Create,
    Alter,
    Drop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: ObjectKind,
    /// The object name parts as written (quotes kept; key functions normalise them).
    pub name: Vec<String>,
    /// The table of a trigger or policy.
    pub on_table: Option<Vec<String>>,
    pub ddl: Ddl,
    /// Byte offset in the statement text where the body scan starts.
    pub body_start: usize,
}

/// `ident(.ident){0,2}` at token `i`; returns the parts and the next token index.
fn name_at(src: &str, t: &[Tok], mut i: usize) -> Option<(Vec<String>, usize)> {
    if !t.get(i)?.is_name() {
        return None;
    }
    let mut parts = vec![t[i].text(src).to_string()];
    i += 1;
    while parts.len() < 3
        && t.get(i).is_some_and(|x| x.is_punct(b'.'))
        && t.get(i + 1).is_some_and(Tok::is_name)
    {
        parts.push(t[i + 1].text(src).to_string());
        i += 2;
    }
    Some((parts, i))
}

/// Recognise the object a statement defines, alters or drops from its leading tokens.
pub fn recognise_header(text: &str) -> Option<Header> {
    let t = tokens(text);
    let w = |i: usize, kw: &str| t.get(i).is_some_and(|x| x.is_kw(text, kw));
    if w(0, "do") {
        return Some(Header {
            kind: ObjectKind::Anonymous,
            name: Vec::new(),
            on_table: None,
            ddl: Ddl::Create,
            body_start: t[0].end,
        });
    }
    let (ddl, mut i) = if w(0, "create") {
        (Ddl::Create, 1)
    } else if w(0, "alter") {
        (Ddl::Alter, 1)
    } else if w(0, "drop") {
        (Ddl::Drop, 1)
    } else {
        return None;
    };
    if ddl == Ddl::Create {
        // Modifiers before the kind.
        loop {
            if w(i, "or") && (w(i + 1, "replace") || w(i + 1, "alter")) {
                i += 2;
            } else if w(i, "definer") || w(i, "algorithm") {
                i += 1;
                if t.get(i).is_some_and(|x| x.is_punct(b'=')) {
                    i += 1;
                }
                i += 1; // the user or algorithm token
                if t.get(i).is_some_and(|x| x.is_punct(b'@')) {
                    i += 2;
                }
            } else if w(i, "sql") && w(i + 1, "security") {
                i += 3;
            } else if [
                "temp",
                "temporary",
                "global",
                "local",
                "unlogged",
                "recursive",
                "virtual",
                "constraint",
            ]
            .iter()
            .any(|k| w(i, k))
            {
                i += 1;
            } else {
                break;
            }
        }
    }
    let kind = if w(i, "table") {
        ObjectKind::Table
    } else if w(i, "view") {
        ObjectKind::View
    } else if w(i, "materialized") && w(i + 1, "view") {
        i += 1;
        ObjectKind::MaterializedView
    } else if ddl != Ddl::Alter && w(i, "function") {
        ObjectKind::Function
    } else if ddl != Ddl::Alter && w(i, "procedure") {
        ObjectKind::Procedure
    } else if ddl != Ddl::Alter && w(i, "trigger") {
        ObjectKind::Trigger
    } else if ddl != Ddl::Alter && w(i, "policy") {
        ObjectKind::Policy
    } else {
        return None;
    };
    if ddl == Ddl::Alter && kind != ObjectKind::Table {
        return None;
    }
    i += 1;
    // IF [NOT] EXISTS and ONLY, in either order
    for _ in 0..2 {
        if w(i, "if") {
            i += 1;
            if w(i, "not") {
                i += 1;
            }
            if w(i, "exists") {
                i += 1;
            }
        }
        if w(i, "only") {
            i += 1;
        }
    }
    let (name, mut next) = name_at(text, &t, i)?;
    let mut on_table = None;
    if matches!(kind, ObjectKind::Trigger | ObjectKind::Policy) {
        let mut j = next;
        while j < t.len() {
            if t[j].is_kw(text, "on") {
                on_table = name_at(text, &t, j + 1).map(|(n, _)| n);
                break;
            }
            j += 1;
        }
    }
    let end_of_name = t[next - 1].end;
    let mut body_start = end_of_name;
    if ddl == Ddl::Create
        && matches!(
            kind,
            ObjectKind::Function | ObjectKind::Procedure | ObjectKind::Trigger
        )
    {
        // After the first AS / BEGIN, at the first dollar-quoted string, or after EXECUTE ... .
        while next < t.len() {
            let x = &t[next];
            if x.is_kw(text, "as") {
                body_start = x.end;
                break;
            }
            if x.is_kw(text, "begin") || matches!(x.kind, TokKind::Dollar { .. }) {
                body_start = x.start;
                break;
            }
            if x.is_kw(text, "execute") {
                body_start = x.start;
                break;
            }
            next += 1;
        }
    }
    Some(Header {
        kind,
        name,
        on_table,
        ddl,
        body_start,
    })
}

// --- body scan -----------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Refs {
    /// Table keys.
    pub tables: BTreeSet<String>,
    /// Routine keys.
    pub routines: BTreeSet<String>,
}

const NOT_A_NAME: &[&str] = &[
    "select",
    "set",
    "on",
    "of",
    "where",
    "values",
    "using",
    "join",
    "inner",
    "left",
    "right",
    "full",
    "cross",
    "natural",
    "outer",
    "group",
    "order",
    "having",
    "limit",
    "offset",
    "union",
    "intersect",
    "except",
    "window",
    "for",
    "returning",
    "with",
    "fetch",
    "into",
    "as",
    "when",
    "then",
    "else",
    "end",
    "and",
    "or",
    "not",
    "null",
    "default",
    "table",
    "if",
    "exists",
    "from",
    "update",
    "delete",
    "insert",
    "only",
    "lateral",
    "current",
    "that",
    "this",
    "to",
    "by",
];

const ALIAS_STOP: &[&str] = &[
    "join",
    "inner",
    "left",
    "right",
    "full",
    "cross",
    "natural",
    "outer",
    "on",
    "using",
    "where",
    "group",
    "order",
    "having",
    "limit",
    "offset",
    "union",
    "intersect",
    "except",
    "window",
    "for",
    "set",
    "values",
    "returning",
    "with",
    "fetch",
    "when",
    "then",
    "else",
    "end",
    "and",
    "or",
    "select",
    "lateral",
    "tablesample",
];

fn lower_in(src: &str, tok: &Tok, list: &[&str]) -> bool {
    tok.kind == TokKind::Word && {
        let w = tok.text(src);
        list.iter().any(|k| w.eq_ignore_ascii_case(k))
    }
}

/// Lexical reference scan of a statement or a procedural body (§6.3). Never fails.
pub fn scan_body(text: &str, default_schema: &str) -> Refs {
    let mut refs = Refs::default();
    scan_into(text, default_schema, &mut refs, true);
    refs
}

fn scan_into(text: &str, schema: &str, refs: &mut Refs, recurse: bool) {
    let t = tokens(text);
    let kw = |i: usize, k: &str| t.get(i).is_some_and(|x| x.is_kw(text, k));
    // CTE names: `<name> AS (` after WITH, RECURSIVE or a comma.
    let mut ctes: BTreeSet<String> = BTreeSet::new();
    for i in 1..t.len().saturating_sub(2) {
        if t[i].is_name()
            && kw(i + 1, "as")
            && t[i + 2].is_punct(b'(')
            && (kw(i - 1, "with") || kw(i - 1, "recursive") || t[i - 1].is_punct(b','))
        {
            ctes.insert(normalise_ident(t[i].text(text)));
        }
    }
    let mut seen_merge = false;
    let add_table = |parts: &[String], refs: &mut Refs| {
        let p: Vec<&str> = parts.iter().map(String::as_str).collect();
        if is_system_relation(&p) {
            return;
        }
        if p.len() == 1 {
            let n = normalise_ident(p[0]);
            if ctes.contains(&n) || n == "new" || n == "old" {
                return;
            }
        }
        refs.tables.insert(table_key(&p, schema));
    };
    // A table name at token `i`: skips ONLY and LATERAL; returns the next token index.
    let table_at = |mut i: usize, allow_paren: bool| -> Option<(Vec<String>, usize)> {
        while kw(i, "only") || kw(i, "lateral") {
            i += 1;
        }
        let first = t.get(i)?;
        if first.kind == TokKind::Word && lower_in(text, first, NOT_A_NAME) {
            return None;
        }
        let (parts, next) = name_at(text, &t, i)?;
        // A function call in table position (`FROM f(x)`) is not a table.
        if !allow_paren && t.get(next).is_some_and(|x| x.is_punct(b'(')) {
            return None;
        }
        Some((parts, next))
    };
    let mut i = 0;
    while i < t.len() {
        let tok = &t[i];
        match tok.kind {
            TokKind::Dollar {
                inner_start,
                inner_end,
            } if recurse => {
                scan_into(&text[inner_start..inner_end], schema, refs, false);
            }
            TokKind::Word => {
                let w = tok.text(text).to_ascii_lowercase();
                match w.as_str() {
                    "merge" => seen_merge = true,
                    "from" => {
                        let mut j = i + 1;
                        loop {
                            let Some((parts, mut next)) = table_at(j, false) else {
                                break;
                            };
                            add_table(&parts, refs);
                            // optional [AS] alias
                            if kw(next, "as") {
                                next += 2;
                            } else if t.get(next).is_some_and(|x| {
                                x.kind == TokKind::Word && !lower_in(text, x, ALIAS_STOP)
                            }) {
                                next += 1;
                            }
                            if t.get(next).is_some_and(|x| x.is_punct(b',')) {
                                j = next + 1;
                            } else {
                                break;
                            }
                        }
                    }
                    "join" => {
                        if let Some((parts, _)) = table_at(i + 1, false) {
                            add_table(&parts, refs);
                        }
                    }
                    "update" => {
                        let prev_blocks = i > 0
                            && lower_in(
                                text,
                                &t[i - 1],
                                &["for", "on", "do", "before", "after", "of", "key"],
                            );
                        if !prev_blocks && let Some((parts, _)) = table_at(i + 1, true) {
                            add_table(&parts, refs);
                        }
                    }
                    "into" => {
                        let near = (1..=2).any(|d| {
                            i >= d && lower_in(text, &t[i - d], &["insert", "replace", "merge"])
                        });
                        if near && let Some((parts, _)) = table_at(i + 1, true) {
                            add_table(&parts, refs);
                        }
                    }
                    "table" => {
                        if i > 0
                            && lower_in(text, &t[i - 1], &["truncate", "lock"])
                            && let Some((parts, _)) = table_at(i + 1, true)
                        {
                            add_table(&parts, refs);
                        }
                    }
                    "using" => {
                        if seen_merge && let Some((parts, _)) = table_at(i + 1, true) {
                            add_table(&parts, refs);
                        }
                    }
                    "call" | "perform" => {
                        if let Some((parts, next)) = name_at(text, &t, i + 1)
                            && t.get(next).is_some_and(|x| x.is_punct(b'('))
                        {
                            add_routine(&parts, schema, refs);
                        }
                    }
                    "execute" => {
                        if (kw(i + 1, "function") || kw(i + 1, "procedure"))
                            && let Some((parts, _)) = name_at(text, &t, i + 2)
                        {
                            add_routine(&parts, schema, refs);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        i += 1;
    }
}

fn add_routine(parts: &[String], schema: &str, refs: &mut Refs) {
    let p: Vec<&str> = parts.iter().map(String::as_str).collect();
    if is_system_relation(&p) {
        return;
    }
    refs.routines.insert(routine_key(&p, schema));
}
