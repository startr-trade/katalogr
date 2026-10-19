//! Rule evaluation: pure and deterministic. A rule runs over the symbols of one fragment; what
//! it emits is stored in the shard like extractor output (ADR-0017).

use super::format::{FactSpec, Piece, Rule, Template, Var};
use katalogr_model::{
    Binding, Confidence, ContractName, ExportKey, Fact, Fragment, LinkKind, Name, OutRef,
};
use std::collections::BTreeMap;

/// What a template can read for one matched symbol.
struct Vars<'a> {
    /// Index 0 is the whole match; groups that did not take part are empty.
    caps: Vec<String>,
    name: &'a str,
    repo: &'a str,
    path: &'a str,
    dir: &'a str,
}

impl Vars<'_> {
    fn expand(&self, t: &Template) -> String {
        let mut out = String::new();
        for p in &t.0 {
            match p {
                Piece::Lit(s) => out.push_str(s),
                Piece::Capture(n) => out.push_str(self.caps.get(*n).map_or("", String::as_str)),
                Piece::Var(Var::Name) => out.push_str(self.name),
                Piece::Var(Var::Repo) => out.push_str(self.repo),
                Piece::Var(Var::Path) => out.push_str(self.path),
                Piece::Var(Var::Dir) => out.push_str(self.dir),
            }
        }
        out
    }

    fn fact(&self, spec: &FactSpec) -> Fact {
        match spec {
            FactSpec::Str(t) => Fact::Str(self.expand(t)),
            FactSpec::Int(n) => Fact::Int(*n),
            FactSpec::Bool(b) => Fact::Bool(*b),
            FactSpec::List(l) => Fact::List(l.iter().map(|s| self.fact(s)).collect()),
            FactSpec::Map(m) => {
                Fact::Map(m.iter().map(|(k, v)| (k.clone(), self.fact(v))).collect())
            }
        }
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
pub fn apply_rule(profile: &str, rule: &Rule, f: &mut Fragment) {
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
        let (id, span, last) = {
            let s = &f.symbols[i];
            if w.kind.as_ref().is_some_and(|k| *k != s.kind) {
                continue;
            }
            let last = s.name.segments.last().map_or("", |g| g.name.as_str());
            (s.name.id(), s.span, last.to_string())
        };
        let mut caps: Vec<String> = vec![last.clone()];
        if let Some(n) = &w.name {
            match captures_of(&n.re, &last) {
                Some(c) => caps = c,
                None => continue,
            }
        }
        if let Some(fact) = &w.fact {
            let Some(value) = f.symbols[i].facts.get(fact) else {
                continue;
            };
            if let Some(m) = &w.matches {
                let Fact::Str(text) = value else { continue };
                match captures_of(&m.re, text) {
                    Some(c) => caps = c,
                    None => continue,
                }
            }
        }
        let vars = Vars {
            caps,
            name: &last,
            repo: &repo,
            path: &path,
            dir: dir_of(&path),
        };
        if let Some(b) = &rule.emit.bind {
            let contract = ContractName {
                kind: vars.expand(&b.kind),
                key: vars.expand(&b.key),
            };
            if contract.validate().is_ok() {
                f.bindings.push(Binding {
                    symbol: id,
                    contract,
                    role: b.role,
                    span: Some(span),
                    confidence: Confidence::Declared,
                    origin: Some(origin.clone()),
                });
            }
        }
        if let Some(r) = &rule.emit.reference {
            f.refs.push(OutRef {
                from: id,
                kind: LinkKind(r.kind.clone()),
                span: Some(span),
                candidates: r
                    .candidates
                    .iter()
                    .map(|c| ExportKey {
                        namespace: vars.expand(&c.ns),
                        key: vars.expand(&c.key),
                    })
                    .collect(),
                confidence: Confidence::Declared,
                origin: Some(origin.clone()),
            });
        }
        if let Some(facts) = &rule.emit.fact {
            let mut add: BTreeMap<String, Fact> = BTreeMap::new();
            for (k, v) in facts {
                add.insert(k.clone(), vars.fact(v));
            }
            let target = &mut f.symbols[i].facts;
            for (k, v) in add {
                target.entry(k).or_insert(v);
            }
        }
    }
}
