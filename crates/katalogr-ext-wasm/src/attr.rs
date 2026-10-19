//! `#[wasm_bindgen(...)]` attribute text, as stored in the `attrs` fact.

use katalogr_model::Fact;
use std::collections::{BTreeMap, BTreeSet};

/// The arguments of one `wasm_bindgen` attribute. `flags` are bare words (`constructor`,
/// `getter`, `method`, `start`, `skip`); `values` are `key = value` pairs with string quotes
/// removed (`js_name`, `js_class`, `js_namespace`, `module`, `raw_module`, `inline_js`,
/// `static_method_of`, `getter`, `setter`). A key given twice keeps its first value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WbArgs {
    pub flags: BTreeSet<String>,
    pub values: BTreeMap<String, String>,
}

impl WbArgs {
    pub fn flag(&self, f: &str) -> bool {
        self.flags.contains(f)
    }

    pub fn value(&self, k: &str) -> Option<&str> {
        self.values.get(k).map(String::as_str)
    }

    /// A flag, or a value, under `k` (`getter` and `getter = x` both count).
    pub fn has(&self, k: &str) -> bool {
        self.flag(k) || self.values.contains_key(k)
    }

    fn merge(&mut self, other: WbArgs) {
        self.flags.extend(other.flags);
        for (k, v) in other.values {
            self.values.entry(k).or_insert(v);
        }
    }
}

/// Split on commas at depth 0, outside string literals (`"..."` with `\"` escapes); each part
/// is trimmed and empty parts are dropped.
pub(crate) fn split_top(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut in_str, mut esc, mut start) = (0i32, false, false, 0usize);
    for (i, c) in s.char_indices() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(s[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(s[start..].trim());
    parts.retain(|p| !p.is_empty());
    parts
}

/// Strip one pair of `"`, unescape `\"` and `\\`. An array value `["a", "b"]` becomes `a.b`.
pub(crate) fn unquote(v: &str) -> String {
    let v = v.trim();
    if let Some(inner) = v.strip_prefix('[').and_then(|r| r.strip_suffix(']')) {
        return split_top(inner)
            .into_iter()
            .map(unquote)
            .collect::<Vec<_>>()
            .join(".");
    }
    match v.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
        Some(inner) => {
            let mut out = String::new();
            let mut chars = inner.chars();
            while let Some(c) = chars.next() {
                if c == '\\' {
                    if let Some(n) = chars.next() {
                        out.push(n);
                    }
                } else {
                    out.push(c);
                }
            }
            out
        }
        None => v.to_string(),
    }
}

/// Parse one attribute text (the `attrs` fact form: no `#[` `]`, whitespace normalised).
/// `None` when it is not a wasm_bindgen attribute. Accepts `wasm_bindgen`,
/// `wasm_bindgen(...)`, any path ending in `::wasm_bindgen`, and `cfg_attr(<pred>, ...)`
/// whose later arguments include one of these (the first such argument is used).
pub fn parse_attr(text: &str) -> Option<WbArgs> {
    let text = text.trim();
    let (head, inner) = match text.find('(') {
        Some(i) => {
            let rest = text[i + 1..].trim_end();
            (text[..i].trim(), Some(rest.strip_suffix(')')?))
        }
        None => (text, None),
    };
    let last = head.rsplit("::").next().unwrap_or(head).trim();
    if last == "wasm_bindgen" {
        let mut args = WbArgs::default();
        for part in inner.map(split_top).unwrap_or_default() {
            match part.split_once('=') {
                Some((k, v)) => {
                    args.values
                        .entry(k.trim().to_string())
                        .or_insert_with(|| unquote(v));
                }
                None => {
                    args.flags.insert(part.to_string());
                }
            }
        }
        Some(args)
    } else if head == "cfg_attr" {
        split_top(inner?).into_iter().skip(1).find_map(parse_attr)
    } else {
        None
    }
}

/// Merge every wasm_bindgen attribute of a list (an `attrs` fact): `None` when none is one.
/// Flags are unioned; for values, the first attribute that sets a key wins.
pub fn parse_attrs(attrs: &[Fact]) -> Option<WbArgs> {
    let mut out: Option<WbArgs> = None;
    for a in attrs {
        if let Fact::Str(s) = a
            && let Some(args) = parse_attr(s)
        {
            match &mut out {
                Some(o) => o.merge(args),
                None => out = Some(args),
            }
        }
    }
    out
}

#[test]
fn js_name_and_flags_parsed() {
    let a = parse_attr("wasm_bindgen(js_name = sayHi, constructor)").unwrap();
    assert_eq!(a.value("js_name"), Some("sayHi"));
    assert!(a.flag("constructor"));
    let a = parse_attr("wasm_bindgen(js_name = \"x\")").unwrap();
    assert_eq!(a.value("js_name"), Some("x"));
    assert!(parse_attr("wasm_bindgen::prelude::wasm_bindgen").is_some());
    assert!(parse_attr("wasm_bindgen").unwrap().flags.is_empty());
    let a = parse_attr("wasm_bindgen(js_namespace = [\"window\", \"document\"], getter)").unwrap();
    assert_eq!(a.value("js_namespace"), Some("window.document"));
    assert!(a.has("getter"));
    let a = parse_attr("wasm_bindgen(getter = value)").unwrap();
    assert!(a.has("getter") && !a.flag("getter"));
    assert_eq!(parse_attr("derive(Debug)"), None);
    let a = parse_attr("wasm_bindgen(js_name = a, js_name = b)").unwrap();
    assert_eq!(a.value("js_name"), Some("a"));
}

#[test]
fn cfg_attr_unwrapped() {
    let a = parse_attr("cfg_attr(target_arch = \"wasm32\", wasm_bindgen)").unwrap();
    assert!(a.flags.is_empty());
    let a =
        parse_attr("cfg_attr(feature = \"w\", derive(Debug), wasm_bindgen(js_name = k))").unwrap();
    assert_eq!(a.value("js_name"), Some("k"));
    assert_eq!(parse_attr("cfg_attr(test, derive(Debug))"), None);
    assert_eq!(parse_attr("cfg_attr(wasm_bindgen)"), None);
}

#[test]
fn attrs_merge() {
    let l = vec![
        Fact::Str("derive(Clone)".into()),
        Fact::Str("wasm_bindgen(js_name = a)".into()),
        Fact::Str("wasm_bindgen(js_name = b, start)".into()),
    ];
    let a = parse_attrs(&l).unwrap();
    assert_eq!(a.value("js_name"), Some("a"));
    assert!(a.flag("start"));
    assert!(parse_attrs(&[Fact::Str("derive(Clone)".into())]).is_none());
}
