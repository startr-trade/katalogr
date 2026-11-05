//! A repo's position: a commit plus a hash of uncommitted changes (FEAT-084; chain decision CD-2
//! of FEAT-043). Pure, so wrappers and the API share it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One repo's position. Serialises as `{"repo","commit","dirty"}`, absent parts as `null`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct PositionRef {
    pub repo: String,
    /// 40 lower-case hex, or `None` for a directory with no commit.
    pub commit: Option<String>,
    /// 64 lower-case hex, or `None` when the checkout is clean.
    pub dirty: Option<String>,
}

/// A position text that does not parse. Callers report it as a usage error (exit 2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PositionError(pub String);

impl std::fmt::Display for PositionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad position: {}", self.0)
    }
}

impl std::error::Error for PositionError {}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl PositionRef {
    /// `<commit>`, `<commit>+<dirty>` or `none+<dirty>`. A position with neither part (which
    /// `parse` never yields) is `none`.
    pub fn text(&self) -> String {
        match (&self.commit, &self.dirty) {
            (Some(c), None) => c.clone(),
            (Some(c), Some(d)) => format!("{c}+{d}"),
            (None, Some(d)) => format!("none+{d}"),
            (None, None) => "none".to_string(),
        }
    }

    /// Parses the text form for `repo`. Bad hex or length is a [`PositionError`].
    pub fn parse(repo: &str, text: &str) -> Result<PositionRef, PositionError> {
        let bad = |why: &str| PositionError(format!("{why}: {text:?}"));
        let (head, dirty) = match text.split_once('+') {
            Some((h, d)) => (h, Some(d)),
            None => (text, None),
        };
        let commit = if head == "none" {
            if dirty.is_none() {
                return Err(bad("`none` needs a `+<dirty>` part"));
            }
            None
        } else if is_hex(head, 40) {
            Some(head.to_string())
        } else {
            return Err(bad("commit must be 40 lower-case hex digits or `none`"));
        };
        let dirty = match dirty {
            Some(d) if is_hex(d, 64) => Some(d.to_string()),
            Some(_) => return Err(bad("dirty part must be 64 lower-case hex digits")),
            None => None,
        };
        Ok(PositionRef {
            repo: repo.to_string(),
            commit,
            dirty,
        })
    }
}

/// A question, for its ID. `args` holds only what changes the answer: not `--json`, `--timeout`
/// or `--consistency`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewQuery {
    pub op: String,
    pub args: std::collections::BTreeMap<String, String>,
}

/// The format version in every view ID; a change of the ID's input bumps it.
pub const VIEW_FORMAT: u32 = 1;

fn quoted(s: &str) -> String {
    serde_json::to_string(s).expect("a string serialises")
}

/// The view ID of an answer (chain decision CD-3): `v1-` plus the first 32 hex characters of the
/// SHA-256 of the compact JSON, keys sorted,
/// `{"facts":F,"positions":[…],"query":{"args":{…},"op":"…"},"view_format":1}`. Pure and total.
/// `positions` must be sorted by repo. `facts` is the tool's own part (see the engine's
/// `facts_version`).
pub fn view_id(positions: &[PositionRef], facts: &str, q: &ViewQuery) -> String {
    use sha2::{Digest, Sha256};
    let opt = |v: &Option<String>| v.as_deref().map_or("null".to_string(), quoted);
    let mut s = String::with_capacity(256);
    s.push_str("{\"facts\":");
    s.push_str(&quoted(facts));
    s.push_str(",\"positions\":[");
    for (i, p) in positions.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!(
            "{{\"commit\":{},\"dirty\":{},\"repo\":{}}}",
            opt(&p.commit),
            opt(&p.dirty),
            quoted(&p.repo)
        ));
    }
    s.push_str("],\"query\":{\"args\":{");
    for (i, (k, v)) in q.args.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{}:{}", quoted(k), quoted(v)));
    }
    s.push_str("},\"op\":");
    s.push_str(&quoted(&q.op));
    s.push_str(&format!("}},\"view_format\":{VIEW_FORMAT}}}"));
    let digest = Sha256::digest(s.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("v1-{}", &hex[..32])
}
