//! Rule evaluation: pure and deterministic. A rule runs over the symbols of one fragment; what
//! it emits is stored in the shard like extractor output (ADR-0017).

use super::format::{FactSpec, Piece, Rule, Template, Var};
use katalogr_model::{
    Binding, Confidence, ContractName, ExportKey, Fact, Fragment, LinkKind, Name, OutRef,
    SymbolNode,
};
use std::collections::BTreeMap;

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

/// Run one rule over every symbol of `f`, adding bindings, references and facts.
pub fn apply_rule(profile: &str, rule: &Rule, f: &mut Fragment, scope: Option<&ScopeIndex>) {
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
    for i in 0..f.symbols.len() {
        let s = &f.symbols[i];
        if w.kind.as_ref().is_some_and(|k| *k != s.kind) {
            continue;
        }
        let last_ref = s.name.segments.last().map_or("", |g| g.name.as_str());
        let mut caps: Option<Vec<String>> = None;
        if let Some(n) = &w.name {
            match captures_of(&n.re, last_ref) {
                Some(c) => caps = Some(c),
                None => continue,
            }
        }
        if let Some(fact) = &w.fact {
            let Some(value) = s.facts.get(fact) else {
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
        // Only a symbol the rule matches needs its id.
        let (id, span, last) = (s.name.id(), s.span, last_ref.to_string());
        let caps = caps.unwrap_or_else(|| vec![last.clone()]);
        let names: Vec<String> = f.symbols[i]
            .name
            .segments
            .iter()
            .map(|g| g.name.clone())
            .collect();
        let vars = Vars {
            caps,
            name: &last,
            repo: &repo,
            path: &path,
            dir: dir_of(&path),
            facts: &f.symbols[i].facts,
            names: &names,
            symbols: &f.symbols,
            scope,
        };
        // Everything is expanded before anything is emitted: a rule whose template finds no
        // value does nothing for this symbol.
        let bind = match &rule.emit.bind {
            Some(b) => {
                let (Some(kind), Some(key)) = (vars.expand(&b.kind), vars.expand(&b.key)) else {
                    continue;
                };
                Some((ContractName { kind, key }, b.role))
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
            Some(facts) => {
                let m: Option<BTreeMap<String, Fact>> = facts
                    .iter()
                    .map(|(k, v)| Some((k.clone(), vars.fact(v)?)))
                    .collect();
                let Some(m) = m else { continue };
                Some(m)
            }
            None => None,
        };
        if let Some((contract, role)) = bind
            && contract.validate().is_ok()
        {
            f.bindings.push(Binding {
                symbol: id,
                contract,
                role,
                span: Some(span),
                confidence: Confidence::Declared,
                origin: Some(origin.clone()),
            });
        }
        if let Some((kind, candidates)) = reference {
            f.refs.push(OutRef {
                from: id,
                kind,
                span: Some(span),
                candidates,
                confidence: Confidence::Declared,
                origin: Some(origin.clone()),
            });
        }
        if let Some(add) = add {
            let target = &mut f.symbols[i].facts;
            for (k, v) in add {
                target.entry(k).or_insert(v);
            }
        }
    }
}
