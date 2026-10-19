//! The embedding SPI: host extractors offer string literals, guest extensions bind them
//! (ADR-0021). Technology-neutral: nothing here names a language or a protocol.

use crate::graph::{Span, SymbolNode};
use crate::name::{Id, Name};
use crate::relpath::RelPath;

/// A string literal a host extractor found in its file, offered to guest extensions.
/// Never stored: the engine consumes it before the shard is encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EmbeddedText {
    /// Innermost symbol of the host fragment whose span contains the literal, else the artifact id.
    pub owner: Id,
    /// The literal's span in the host file (delimiters included).
    pub span: Span,
    /// The literal's value after the host language's unescaping.
    pub text: String,
    /// Ascending byte offsets into `text` where an interpolated expression was removed
    /// (template literal `${..}`); empty for plain literals.
    pub holes: Vec<u32>,
    /// The call the literal is an argument of, when it is one (see `CallContext`); else None.
    pub call: Option<CallContext>,
}

/// The call site a literal is passed to. Neutral: it carries host-language text, never a
/// meaning. Contract for hosts (every host that sets it must follow all of it):
/// - Set it exactly when the literal is, by itself, one argument of a call: a function or path
///   call, a method call, a macro invocation, a constructor (`new`) call, or a tagged template.
///   "By itself" allows the argument to be the literal wrapped only in parentheses, a borrow
///   (`&`), or a type-only wrapper (`as`, `satisfies`, non-null `!`). A literal nested deeper
///   (inside an object, array, binary expression or nested call) gets None.
/// - `callee` is the callee as written, rendered by these rules, and nothing else:
///   - path or identifier call: its text with whitespace removed (`reqwest::get`, `fetch`);
///   - method call: `<recv>.<method>`, where `<recv>` is rendered only when the receiver is an
///     identifier, a path, `self`/`this`, or a field/member chain of those (`self.client`,
///     `this.http`); otherwise it is omitted and the text starts with `.` (`.get` for
///     `Client::new().get(..)`);
///   - macro invocation: the macro path followed by `!` (`sqlx::query!`);
///   - constructor call: `new ` followed by the callee rendered as for a path call (`new URL`);
///   - tagged template: the tag rendered as for a path or method call, with `arg` 0;
///   - generic arguments (`::<T>`, `<T>`) and comments are removed;
///   - at most `CALL_CALLEE_MAX` bytes, cut at a char boundary.
///
///   This is the same callee rule as FEAT-014's `render_call` at depth 0, so guests and REST
///   profiles see one spelling.
/// - `arg` is the zero-based position of the argument among the call's arguments; a method's
///   receiver is not counted; for a macro, top-level commas in its token tree are counted.
///
/// Guests may match `callee` text, but must treat it as untyped syntax: no host resolves names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallContext {
    pub callee: String,
    pub arg: u32,
}

/// Longest callee text kept, in bytes.
pub const CALL_CALLEE_MAX: usize = 256;

impl CallContext {
    /// Build one from already-rendered callee text: removes ASCII whitespace, truncates to
    /// `CALL_CALLEE_MAX` at a char boundary. None when the result is empty.
    ///
    /// A `new ` prefix (constructor call) keeps its one space: whitespace is removed from the
    /// callee text itself, so a constructor's `new` marker is written `new ` by the host and
    /// is preserved here.
    pub fn new(callee: &str, arg: u32) -> Option<CallContext> {
        let (prefix, rest) = match callee.strip_prefix("new ") {
            Some(rest) => ("new ", rest),
            None => ("", callee),
        };
        let mut s = String::with_capacity(callee.len());
        s.push_str(prefix);
        s.extend(rest.chars().filter(|c| !c.is_ascii_whitespace()));
        if s.len() > CALL_CALLEE_MAX {
            let mut end = CALL_CALLEE_MAX;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            s.truncate(end);
        }
        if s.is_empty() || s == "new " {
            return None;
        }
        Some(CallContext { callee: s, arg })
    }
}

/// What a guest receives for one embedded text.
#[derive(Copy, Clone, Debug)]
pub struct EmbeddedInput<'a> {
    pub repo: &'a str,
    pub host_path: &'a RelPath,
    pub host_extension: &'a str,
    pub owner: Id,
    pub span: Span,
    pub text: &'a str,
    pub holes: &'a [u32],
    pub call: Option<&'a CallContext>,
    /// This guest's embedded context for `repo`: exactly the bytes its `embedded_context`
    /// returned for its stored repository context (empty when it has none). The guest must not
    /// depend on anything else from the repository, because only these bytes key host revisions.
    pub context: &'a [u8],
}

/// Shortest offer without a call context, in bytes after trimming.
pub const EMBED_MIN_LEN: usize = 8;
/// Longest offer, in bytes; longer literals are not offered.
pub const EMBED_MAX_LEN: usize = 65_536;
/// Offers per file, in source order.
pub const EMBED_MAX_PER_FILE: usize = 2_000;

/// The innermost symbol whose span contains `at`: the latest start wins, then the earliest end,
/// then the smallest canonical name. `artifact` when none contains it. Containment is
/// inclusive on both ends.
pub fn innermost_owner(symbols: &[SymbolNode], artifact: Id, at: Span) -> Id {
    let mut best: Option<(&SymbolNode, String)> = None;
    for s in symbols {
        let sp = s.span;
        let contains = (sp.start_line, sp.start_col) <= (at.start_line, at.start_col)
            && (at.end_line, at.end_col) <= (sp.end_line, sp.end_col);
        if !contains {
            continue;
        }
        let canon = s.name.canonical();
        let better = match &best {
            None => true,
            Some((b, bc)) => {
                let bs = (b.span.start_line, b.span.start_col);
                let ss = (sp.start_line, sp.start_col);
                if ss != bs {
                    ss > bs
                } else {
                    let be = (b.span.end_line, b.span.end_col);
                    let se = (sp.end_line, sp.end_col);
                    if se != be { se < be } else { canon < *bc }
                }
            }
        };
        if better {
            best = Some((s, canon));
        }
    }
    best.map_or(artifact, |(s, _)| s.name.id())
}

/// Push `item` as an offer when its text passes the neutral limits, and fewer than
/// `EMBED_MAX_PER_FILE` offers are already in `out`. Returns whether it was pushed.
/// - Every offer: at most `EMBED_MAX_LEN` bytes, and not empty after trimming.
/// - An offer with `call == None` must also be at least `EMBED_MIN_LEN` bytes after trimming and
///   contain at least one ASCII whitespace byte (free-standing text is only worth offering when
///   it looks like a statement).
/// - An offer with `call == Some(_)` skips those two rules, so short call arguments without
///   whitespace (a URL path such as `/orders`, for FEAT-014) reach guests.
pub fn offer(out: &mut Vec<EmbeddedText>, item: EmbeddedText) -> bool {
    if out.len() >= EMBED_MAX_PER_FILE || item.text.len() > EMBED_MAX_LEN {
        return false;
    }
    let trimmed = item.text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if item.call.is_none()
        && (trimmed.len() < EMBED_MIN_LEN || !item.text.bytes().any(|b| b.is_ascii_whitespace()))
    {
        return false;
    }
    out.push(item);
    true
}
