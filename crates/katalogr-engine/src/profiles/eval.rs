//! Rule evaluation: pure and deterministic. A rule runs over the symbols of one fragment; what
//! it emits is stored in the shard like extractor output (ADR-0017).

use super::format::{FactSpec, On, Piece, Rule, Template, Var};
use katalogr_model::calls::{ArgRef, arg_ref_of};
use katalogr_model::{
    Binding, Confidence, ContractName, ExportKey, Fact, Fragment, Id, KeyNormaliser, LinkKind,
    Name, OutRef, RefBinding, Span, SymbolNode,
};
use std::collections::{BTreeMap, HashMap};

/// The symbols of one fragment by the list of their segment names (kinds ignored), for
/// `${scope:..}` lookups. Built once per fragment, only when a rule needs it. Keyed by a hash
/// of the names, so a lookup builds no key; a hit is checked against the names themselves.
pub struct ScopeIndex {
    /// (hash of the names, symbol index), sorted.
    entries: Vec<(u64, usize)>,
}

const SEED: u64 = 0xcbf2_9ce4_8422_2325;

/// Fold one name into a running hash (FNV-1a over the bytes, then a separator).
fn mix(mut h: u64, name: &str) -> u64 {
    for b in name.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= 0xff;
    h.wrapping_mul(0x0100_0000_01b3)
}

impl ScopeIndex {
    pub fn build(f: &Fragment) -> ScopeIndex {
        let mut entries: Vec<(u64, usize)> = f
            .symbols
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let h = s.name.segments.iter().fold(SEED, |h, g| mix(h, &g.name));
                (h, i)
            })
            .collect();
        entries.sort_unstable();
        ScopeIndex { entries }
    }

    /// The symbol whose segment names are `prefix` followed by `parts` (the first in fragment
    /// order wins; the lower ordinal wins a tie of names), given the hash of `prefix`.
    fn find<'s>(
        &self,
        symbols: &'s [SymbolNode],
        prefix_hash: u64,
        prefix: &[String],
        parts: &[String],
    ) -> Option<&'s SymbolNode> {
        let h = parts.iter().fold(prefix_hash, |h, p| mix(h, p));
        let start = self.entries.partition_point(|(eh, _)| *eh < h);
        let mut best: Option<&SymbolNode> = None;
        for &(eh, i) in &self.entries[start..] {
            if eh != h {
                break;
            }
            let sym = &symbols[i];
            let segs = &sym.name.segments;
            if segs.len() != prefix.len() + parts.len() {
                continue;
            }
            let same = segs
                .iter()
                .zip(prefix.iter().chain(parts.iter()))
                .all(|(g, n)| g.name == *n);
            if same && best.is_none_or(|b| sym.name.ordinal < b.name.ordinal) {
                best = Some(sym);
            }
        }
        best
    }
}

/// What a template can read for one matched symbol.
struct Vars<'a> {
    /// Index 0 is the whole match; groups that did not take part are empty.
    caps: Vec<String>,
    name: &'a str,
    repo: &'a str,
    path: &'a str,
    dir: &'a str,
    /// The matched symbol's facts.
    facts: &'a BTreeMap<String, Fact>,
    /// The matched symbol's segment names.
    names: &'a [String],
    symbols: &'a [SymbolNode],
    scope: Option<&'a ScopeIndex>,
}

impl Vars<'_> {
    /// None when a `Fact` or `Scope` piece finds nothing.
    fn expand(&self, t: &Template) -> Option<String> {
        let mut out = String::new();
        for p in &t.0 {
            match p {
                Piece::Lit(s) => out.push_str(s),
                Piece::Capture(n) => out.push_str(self.caps.get(*n).map_or("", String::as_str)),
                Piece::Var(Var::Name) => out.push_str(self.name),
                Piece::Var(Var::Repo) => out.push_str(self.repo),
                Piece::Var(Var::Path) => out.push_str(self.path),
                Piece::Var(Var::Dir) => out.push_str(self.dir),
                Piece::Fact(name) => match self.facts.get(name) {
                    Some(Fact::Str(s)) => out.push_str(s),
                    _ => return None,
                },
                Piece::Scope(parts) => out.push_str(self.scoped(parts)?),
            }
        }
        Some(out)
    }

    /// The `value` of the symbol whose segment names are the matched symbol's first `k`
    /// names followed by `parts`, for the largest `k` that finds one with a string `value`.
    fn scoped(&self, parts: &[String]) -> Option<&str> {
        let index = self.scope?;
        let mut hashes = Vec::with_capacity(self.names.len() + 1);
        hashes.push(SEED);
        for n in self.names {
            hashes.push(mix(*hashes.last().expect("seeded"), n));
        }
        for k in (0..=self.names.len()).rev() {
            if let Some(sym) = index.find(self.symbols, hashes[k], &self.names[..k], parts)
                && let Some(Fact::Str(v)) = sym.facts.get("value")
            {
                return Some(v);
            }
        }
        None
    }

    fn fact(&self, spec: &FactSpec) -> Option<Fact> {
        Some(match spec {
            FactSpec::Str(t) => Fact::Str(self.expand(t)?),
            FactSpec::Int(n) => Fact::Int(*n),
            FactSpec::Bool(b) => Fact::Bool(*b),
            FactSpec::List(l) => Fact::List(l.iter().map(|s| self.fact(s)).collect::<Option<_>>()?),
            FactSpec::Map(m) => Fact::Map(
                m.iter()
                    .map(|(k, v)| Some((k.clone(), self.fact(v)?)))
                    .collect::<Option<_>>()?,
            ),
        })
    }
}

fn captures_of(re: &regex::Regex, text: &str) -> Option<Vec<String>> {
    let c = re.captures(text)?;
    Some(
        (0..c.len())
            .map(|i| {
                c.get(i)
                    .map_or_else(String::new, |m| m.as_str().to_string())
            })
            .collect(),
    )
}

/// The directory part of a repository-relative path (`""` at the root).
fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// The origin text recorded on everything a rule emits.
pub fn origin(profile: &str, rule: &Rule) -> String {
    format!("profile:{profile}#{}", rule.id)
}

/// Per-fragment lookups built once and shared by every rule.
pub struct Cx {
    scope: Option<ScopeIndex>,
    /// Symbols by their name relative to the artifact (the canonical name after the first `#`),
    /// built only when a rule has a `holder`.
    locals: Option<HashMap<String, Id>>,
    /// The artifact's `str_call_cands` fact, through which `str_calls` entries name their export
    /// candidates; read only when a rule has a `holder`.
    cands: Option<Fact>,
}

impl Cx {
    pub fn new(scope: Option<ScopeIndex>, f: &Fragment, holders: bool) -> Cx {
        let locals = holders.then(|| {
            let mut m: HashMap<String, Id> = HashMap::new();
            for s in &f.symbols {
                let canon = s.name.canonical();
                if let Some((_, rel)) = canon.split_once('#') {
                    m.entry(rel.to_string()).or_insert_with(|| s.name.id());
                }
            }
            m
        });
        let cands = holders
            .then(|| {
                f.artifact
                    .facts
                    .get(katalogr_model::calls::STR_CALL_CANDS_FACT)
            })
            .flatten()
            .cloned();
        Cx {
            scope,
            locals,
            cands,
        }
    }
}

/// One element of a list fact: its text and span.
fn element_of(e: &Fact) -> Option<(&str, Option<Span>)> {
    match e {
        Fact::Str(s) => Some((s, None)),
        Fact::Map(m) => {
            let Some(Fact::Str(text)) = m.get("text") else {
                return None;
            };
            let int = |k: &str| match m.get(k) {
                Some(Fact::Int(n)) => u32::try_from(*n).ok(),
                _ => None,
            };
            // Only the start is stored; the element's span is the point where it starts.
            let span = match (int("line"), int("col")) {
                (Some(a), Some(b)) => Some(Span {
                    start_line: a,
                    start_col: b,
                    end_line: a,
                    end_col: b,
                }),
                _ => None,
            };
            Some((text, span))
        }
        _ => None,
    }
}

/// The matched node: kind, last name, facts, segment names, id and span.
type Node<'a> = (
    &'a str,
    String,
    &'a BTreeMap<String, Fact>,
    Vec<String>,
    Id,
    Option<Span>,
);

/// One evaluation of a rule: the element, the regex captures and the span to emit at.
type Pass<'a> = (Option<&'a Fact>, Option<Vec<String>>, Option<Span>);

/// Run one rule over the nodes it visits in `f`, adding bindings, references and facts.
pub fn apply_rule(
    profile: &str,
    rule: &Rule,
    f: &mut Fragment,
    cx: &mut Cx,
    norms: &[&'static dyn KeyNormaliser],
) {
    let w = &rule.when;
    if w.extension.as_ref().is_some_and(|e| *e != f.extension) {
        return;
    }
    let path = f.artifact.name.path.as_str().to_string();
    if w.path.as_ref().is_some_and(|g| !g.matcher.is_match(&path)) {
        return;
    }
    let repo = f.provenance.repo.clone();
    let origin = origin(profile, rule);
    let on = w.on.unwrap_or(On::Symbols);
    let artifact_id = f.artifact.name.id();
    let file_name = path.rsplit('/').next().unwrap_or("").to_string();
    let targets: Vec<Option<usize>> = (if matches!(on, On::Symbols | On::All) {
        0..f.symbols.len()
    } else {
        0..0
    })
    .map(Some)
    .chain(matches!(on, On::Artifact | On::All).then_some(None))
    .collect();
    for t in targets {
        // The matched node: its kind, last name, facts, segment names and id.
        let (kind, last, facts, names, id, node_span): Node = match t {
            Some(i) => {
                let s = &f.symbols[i];
                (
                    s.kind.as_str(),
                    s.name
                        .segments
                        .last()
                        .map_or(String::new(), |g| g.name.clone()),
                    &s.facts,
                    s.name.segments.iter().map(|g| g.name.clone()).collect(),
                    s.name.id(),
                    Some(s.span),
                )
            }
            None => (
                "artifact",
                file_name.clone(),
                &f.artifact.facts,
                Vec::new(),
                artifact_id,
                None,
            ),
        };
        if w.kind.as_ref().is_some_and(|k| k != kind) {
            continue;
        }
        let mut name_caps: Option<Vec<String>> = None;
        if let Some(n) = &w.name {
            match captures_of(&n.re, &last) {
                Some(c) => name_caps = Some(c),
                None => continue,
            }
        }
        // The passes: one per element of an `each` list, else exactly one.
        let mut passes: Vec<Pass> = Vec::new();
        if let Some(each) = &w.each {
            let Some(Fact::List(items)) = facts.get(each) else {
                continue;
            };
            for el in items {
                let Some((text, span)) = element_of(el) else {
                    continue;
                };
                let caps = match &w.matches {
                    Some(m) => match captures_of(&m.re, text) {
                        Some(c) => Some(c),
                        None => continue,
                    },
                    None => name_caps.clone(),
                };
                passes.push((Some(el), caps, span.or(node_span)));
            }
        } else {
            let mut caps = name_caps;
            if let Some(fact) = &w.fact {
                let Some(value) = facts.get(fact) else {
                    continue;
                };
                if let Some(m) = &w.matches {
                    let Fact::Str(text) = value else { continue };
                    match captures_of(&m.re, text) {
                        Some(c) => caps = Some(c),
                        None => continue,
                    }
                }
            }
            passes.push((None, caps, node_span));
        }
        let mut new_bindings: Vec<Binding> = Vec::new();
        let mut new_refs: Vec<OutRef> = Vec::new();
        let mut new_facts: BTreeMap<String, Fact> = BTreeMap::new();
        for (element, caps, span) in passes {
            let vars = Vars {
                caps: caps.unwrap_or_else(|| vec![last.clone()]),
                name: &last,
                repo: &repo,
                path: &path,
                dir: dir_of(&path),
                facts,
                names: &names,
                symbols: &f.symbols,
                scope: cx.scope.as_ref(),
            };
            // Everything is expanded before anything is emitted: a rule whose template finds no
            // value (or whose normaliser finds no key) does nothing for this pass.
            let bind = match &rule.emit.bind {
                Some(b) => {
                    let (Some(kind), Some(mut key)) = (vars.expand(&b.kind), vars.expand(&b.key))
                    else {
                        continue;
                    };
                    if let Some(nid) = &b.normalise {
                        let Some(n) = norms.iter().find(|n| n.id() == nid) else {
                            continue;
                        };
                        let Some(k) = n.normalise(&key) else {
                            continue;
                        };
                        key = k;
                    }
                    let holder = match &b.holder {
                        Some(h) => match vars.expand(h) {
                            Some(h) => Some(h),
                            None => continue,
                        },
                        None => None,
                    };
                    Some((ContractName { kind, key }, b, holder))
                }
                None => None,
            };
            let reference = match &rule.emit.reference {
                Some(r) => {
                    let candidates: Option<Vec<ExportKey>> = r
                        .candidates
                        .iter()
                        .map(|c| {
                            Some(ExportKey {
                                namespace: vars.expand(&c.ns)?,
                                key: vars.expand(&c.key)?,
                            })
                        })
                        .collect();
                    let Some(candidates) = candidates else {
                        continue;
                    };
                    Some((LinkKind(r.kind.clone()), candidates))
                }
                None => None,
            };
            let add: Option<BTreeMap<String, Fact>> = match &rule.emit.fact {
                Some(fs) => {
                    let m: Option<BTreeMap<String, Fact>> = fs
                        .iter()
                        .map(|(k, v)| Some((k.clone(), vars.fact(v)?)))
                        .collect();
                    let Some(m) = m else { continue };
                    Some(m)
                }
                None => None,
            };
            if let Some((contract, b, holder)) = bind
                && contract.validate().is_ok()
            {
                let confidence = b.confidence.unwrap_or(Confidence::Declared);
                let held = hold(
                    holder.as_deref(),
                    element,
                    cx.locals.as_ref(),
                    cx.cands.as_ref(),
                    id,
                    confidence,
                );
                match held {
                    Held::Symbol(symbol, confidence) => new_bindings.push(Binding {
                        symbol,
                        contract,
                        role: b.role,
                        span,
                        confidence,
                        origin: Some(origin.clone()),
                    }),
                    Held::Reference(candidates, confidence) => new_refs.push(OutRef {
                        from: id,
                        kind: LinkKind(b.role.as_str().to_string()),
                        span,
                        candidates,
                        confidence,
                        origin: Some(origin.clone()),
                        binds: Some(RefBinding {
                            contract,
                            role: b.role,
                        }),
                    }),
                }
            }
            if let Some((kind, candidates)) = reference {
                new_refs.push(OutRef {
                    from: id,
                    kind,
                    span,
                    candidates,
                    confidence: Confidence::Declared,
                    origin: Some(origin.clone()),
                    binds: None,
                });
            }
            if let Some(add) = add {
                for (k, v) in add {
                    new_facts.entry(k).or_insert(v);
                }
            }
        }
        f.bindings.extend(new_bindings);
        f.refs.extend(new_refs);
        let target = match t {
            Some(i) => &mut f.symbols[i].facts,
            None => &mut f.artifact.facts,
        };
        for (k, v) in new_facts {
            target.entry(k).or_insert(v);
        }
    }
}

/// Who holds a binding emitted by a rule.
enum Held {
    /// A symbol of the fragment.
    Symbol(Id, Confidence),
    /// Whatever the export candidates resolve to (a bound reference, ADR-0022).
    Reference(Vec<ExportKey>, Confidence),
}

/// Decide the holder of a binding (ADR-0020, section 6.7). Without a `holder` the matched node
/// holds it. With one, the argument it names is looked up in the element's `refs` entry: a local
/// symbol holds it directly, export candidates hold it through a reference, and anything else
/// (an empty name, no entry, a name absent from the fragment) falls back to the matched node,
/// inferred.
fn hold(
    holder: Option<&str>,
    element: Option<&Fact>,
    locals: Option<&HashMap<String, Id>>,
    cands: Option<&Fact>,
    target: Id,
    confidence: Confidence,
) -> Held {
    let Some(h) = holder else {
        return Held::Symbol(target, confidence);
    };
    let fallback = Held::Symbol(target, Confidence::Inferred);
    if h.is_empty() {
        return fallback;
    }
    let Some((r, inferred)) = element.and_then(|e| arg_ref_of(e, cands, h)) else {
        return fallback;
    };
    let confidence = if inferred {
        Confidence::Inferred
    } else {
        confidence
    };
    match r {
        ArgRef::Local(rel) => match locals.and_then(|m| m.get(&rel)) {
            Some(id) => Held::Symbol(*id, confidence),
            None => fallback,
        },
        ArgRef::Candidates(c) => Held::Reference(c, confidence),
    }
}
