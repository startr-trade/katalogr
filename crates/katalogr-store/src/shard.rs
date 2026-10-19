//! Shard codec: deterministic JSON per ADR-0013.

use crate::error::StoreError;
use crate::guard::scan_for_absolute;
use camino::{Utf8Path, Utf8PathBuf};
use katalogr_model::{Fragment, Name, RelPath};
use serde_json::Value;

pub const FORMAT: u64 = 1;
pub const SUFFIX: &str = ".kshard.json";

/// Location of a shard relative to the project directory: `shards/<repo>/<path>.kshard.json`.
pub fn shard_path(repo: &str, path: &RelPath) -> Utf8PathBuf {
    Utf8PathBuf::from(format!("shards/{repo}/{}{SUFFIX}", path.as_str()))
}

/// Serialise a value with sorted keys, 2-space indentation, LF and one trailing newline.
pub(crate) fn to_sorted_json(v: &Value) -> String {
    let mut out = String::new();
    write_value(v, 0, &mut out);
    out.push('\n');
    out
}

fn indent(n: usize, out: &mut String) {
    for _ in 0..n {
        out.push_str("  ");
    }
}

fn write_value(v: &Value, depth: usize, out: &mut String) {
    match v {
        Value::Array(a) if !a.is_empty() => {
            out.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                indent(depth + 1, out);
                write_value(x, depth + 1, out);
                out.push_str(if i + 1 < a.len() { ",\n" } else { "\n" });
            }
            indent(depth, out);
            out.push(']');
        }
        Value::Object(m) if !m.is_empty() => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            out.push_str("{\n");
            for (i, k) in keys.iter().enumerate() {
                indent(depth + 1, out);
                out.push_str(&serde_json::to_string(k).expect("string"));
                out.push_str(": ");
                write_value(&m[k.as_str()], depth + 1, out);
                out.push_str(if i + 1 < keys.len() { ",\n" } else { "\n" });
            }
            indent(depth, out);
            out.push('}');
        }
        other => out.push_str(&serde_json::to_string(other).expect("value")),
    }
}

fn json_err(e: serde_json::Error) -> StoreError {
    StoreError::BadShard(e.to_string())
}

/// Sort a copy of the fragment's vectors: symbols by canonical name; links by (from, kind, to);
/// refs by (from, kind, first candidate); exports by key; bindings by (contract, role, symbol). Ties break on the serialised element.
fn sorted(f: &Fragment) -> Fragment {
    let mut g = f.clone();
    let ser = |x: &dyn erased::Ser| x.json();
    g.symbols
        .sort_by_cached_key(|s| (s.name.canonical(), ser(s)));
    g.links
        .sort_by_cached_key(|l| (l.from, l.kind.clone(), l.to, ser(l)));
    g.refs.sort_by_cached_key(|r| {
        (
            r.from,
            r.kind.clone(),
            r.candidates.first().cloned(),
            ser(r),
        )
    });
    g.exports
        .sort_by_cached_key(|e| (e.key.clone(), e.target, ser(e)));
    g.bindings
        .sort_by_cached_key(|b| (b.contract.canonical(), b.role, b.symbol, ser(b)));
    g
}

mod erased {
    pub trait Ser {
        fn json(&self) -> String;
    }
    impl<T: serde::Serialize> Ser for T {
        fn json(&self) -> String {
            serde_json::to_string(self).unwrap_or_default()
        }
    }
}

/// R-3/R-4/R-5. Deterministic bytes; runs the absolute-path guard (home directory only) first.
pub fn encode(f: &Fragment) -> Result<Vec<u8>, StoreError> {
    encode_with_root(f, None)
}

/// As [`encode`], also refusing strings that contain the kindex absolute root.
pub fn encode_with_root(
    f: &Fragment,
    kindex_root: Option<&Utf8Path>,
) -> Result<Vec<u8>, StoreError> {
    if f.provenance.revision.is_empty() {
        return Err(StoreError::BadShard("provenance.revision is empty".into()));
    }
    scan_for_absolute(f, kindex_root)?;
    let mut v = serde_json::to_value(sorted(f)).map_err(json_err)?;
    v.as_object_mut()
        .expect("fragment is an object")
        .insert("format".into(), Value::from(FORMAT));
    Ok(to_sorted_json(&v).into_bytes())
}

/// Rejects a missing or different `format`.
pub fn decode(bytes: &[u8]) -> Result<Fragment, StoreError> {
    let mut v: Value = serde_json::from_slice(bytes).map_err(json_err)?;
    let obj = v
        .as_object_mut()
        .ok_or_else(|| StoreError::BadShard("not an object".into()))?;
    match obj.remove("format") {
        Some(n) if n.as_u64() == Some(FORMAT) => {}
        Some(other) => return Err(StoreError::ShardFormat(other.to_string())),
        None => return Err(StoreError::ShardFormat("missing".into())),
    }
    serde_json::from_value(v).map_err(json_err)
}
