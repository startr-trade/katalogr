//! The Rust-side pass (FEAT-016 sections 6.1 and 6.2): wasm-bindgen exports become `provides`
//! bindings to `wasm_export` contracts, and `#[wasm_bindgen] extern` items reference the JS
//! they import. It reads the facts ext-rust already records (`attrs`, `extern_attrs`,
//! `signature`, `vis`) and never parses Rust.

use crate::attr::{WbArgs, parse_attrs, split_top};
use crate::pkg::{contract, is_glue_only, join_norm, normalise};
use crate::{FACT_IMPORT, FACT_JS};
use katalogr_model::{
    Binding, Confidence, ContractRole, ExportKey, Fact, Fragment, Id, LinkKind, Name, OutRef,
    SymbolNode,
};
use std::collections::HashMap;

/// The crate that owns a Rust file, as ext-rust's context knows it.
#[derive(Copy, Clone, Debug)]
pub struct CrateRef<'a> {
    /// Cargo package name.
    pub name: &'a str,
    /// Repo-relative crate directory (`""` at the root).
    pub dir: &'a str,
}

const NS_EXPORT: &str = "ts-export";
const NS_PACKAGE: &str = "ts-package";

fn list<'a>(s: &'a SymbolNode, key: &str) -> &'a [Fact] {
    match s.facts.get(key) {
        Some(Fact::List(l)) => l,
        _ => &[],
    }
}

fn text<'a>(s: &'a SymbolNode, key: &str) -> Option<&'a str> {
    match s.facts.get(key) {
        Some(Fact::Str(t)) => Some(t),
        _ => None,
    }
}

/// True when any symbol's `attrs` or `extern_attrs` fact mentions `wasm_bindgen` (a cheap
/// guard; `apply` is skipped otherwise).
pub fn mentions_wasm_bindgen(f: &Fragment) -> bool {
    f.symbols.iter().any(|s| {
        ["attrs", "extern_attrs"].iter().any(|k| {
            list(s, k)
                .iter()
                .any(|a| matches!(a, Fact::Str(t) if t.contains("wasm_bindgen")))
        })
    })
}

/// Strip generics and take the last `::` segment: `a::Foo<T>` is `Foo`.
fn last_segment(t: &str) -> Option<String> {
    let t = t.split('<').next().unwrap_or(t).trim();
    let seg = t.rsplit("::").next().unwrap_or(t).trim();
    (!seg.is_empty()).then(|| seg.to_string())
}

/// From a signature `fn m(this: &Foo, ..)`, the last path segment of the first parameter's
/// type with `&`, `&mut`, lifetimes and generics stripped.
fn this_type(signature: &str) -> Option<String> {
    let open = signature.find('(')?;
    let rest = &signature[open + 1..];
    let close = rest.rfind(')')?;
    let first = split_top(&rest[..close]).into_iter().next()?;
    let ty = first.split_once(':')?.1.trim();
    let mut ty = ty.trim_start_matches('&').trim();
    if ty.starts_with('\'') {
        ty = ty.split_once(' ').map_or("", |x| x.1).trim();
    }
    let ty = ty.strip_prefix("mut ").unwrap_or(ty).trim();
    last_segment(ty)
}

/// The last path segment after `->`, generics stripped.
fn return_type(signature: &str) -> Option<String> {
    let after = signature.rsplit_once("->")?.1;
    let after = after.split(" where ").next().unwrap_or(after);
    last_segment(after)
}

fn strip_set(name: &str) -> &str {
    name.strip_prefix("set_").unwrap_or(name)
}

/// The member name for a getter, setter or plain method, from its own arguments.
fn member_name(margs: &WbArgs, name: &str) -> String {
    if margs.has("getter") {
        margs
            .value("getter")
            .or_else(|| margs.value("js_name"))
            .unwrap_or(name)
            .to_string()
    } else if margs.has("setter") {
        margs
            .value("setter")
            .or_else(|| margs.value("js_name"))
            .unwrap_or_else(|| strip_set(name))
            .to_string()
    } else {
        margs.value("js_name").unwrap_or(name).to_string()
    }
}

fn sym_name(s: &SymbolNode) -> &str {
    s.name.segments.last().map_or("", |g| g.name.as_str())
}

fn in_impl(s: &SymbolNode) -> bool {
    s.name.segments.iter().any(|g| g.kind == "impl")
}

/// Add the wasm-bindgen bindings, references and facts to a Rust fragment. With
/// `krate == None` (an orphan file) it adds facts only. Idempotent: a symbol that already
/// carries its fact is skipped, so running it twice adds nothing the second time.
pub fn apply(f: &mut Fragment, krate: Option<CrateRef<'_>>) {
    let ids: Vec<Id> = f.symbols.iter().map(|s| s.name.id()).collect();
    let index: HashMap<Id, usize> = ids.iter().enumerate().map(|(i, id)| (*id, i)).collect();
    let mut children: HashMap<Id, Vec<usize>> = HashMap::new();
    for l in &f.links {
        if l.kind.0 == "contains"
            && let Some(&c) = index.get(&l.to)
        {
            children.entry(l.from).or_default().push(c);
        }
    }
    for v in children.values_mut() {
        v.sort_unstable();
    }
    let key = krate.map(|k| normalise(k.name));

    let mut facts: Vec<(usize, &'static str, String)> = Vec::new();
    let mut binds: Vec<(usize, String)> = Vec::new();
    let mut refs: Vec<OutRef> = Vec::new();

    for (i, s) in f.symbols.iter().enumerate() {
        if s.facts.contains_key(FACT_JS) || s.facts.contains_key(FACT_IMPORT) {
            continue;
        }
        let attrs = list(s, "attrs");
        if s.facts.contains_key("extern_attrs") {
            if let Some(r) = extern_item(s, i, krate, &mut facts) {
                refs.push(OutRef { from: ids[i], ..r });
            }
            continue;
        }
        let Some(args) = parse_attrs(attrs) else {
            continue;
        };
        match s.kind.as_str() {
            "fn" | "struct" | "enum" if !in_impl(s) => {
                if args.flag("skip") || args.flag("typescript_custom_section") {
                    continue;
                }
                if args.flag("start") && s.kind == "fn" {
                    facts.push((i, FACT_JS, "(start)".to_string()));
                    continue;
                }
                let p = args
                    .value("js_name")
                    .unwrap_or_else(|| sym_name(s))
                    .to_string();
                if is_glue_only(&p) {
                    continue;
                }
                facts.push((i, FACT_JS, p.clone()));
                binds.push((i, p.clone()));
                if s.kind == "enum" {
                    for &c in children.get(&ids[i]).into_iter().flatten() {
                        let v = &f.symbols[c];
                        if v.kind == "variant" {
                            let vp = format!("{p}.{}", sym_name(v));
                            facts.push((c, FACT_JS, vp.clone()));
                            binds.push((c, vp));
                        }
                    }
                }
            }
            "impl" => {
                let name = sym_name(s);
                if name.contains(" for ") {
                    continue;
                }
                let Some(class) = args
                    .value("js_class")
                    .map(str::to_string)
                    .or_else(|| last_segment(name))
                else {
                    continue;
                };
                for &c in children.get(&ids[i]).into_iter().flatten() {
                    let m = &f.symbols[c];
                    if m.kind != "fn"
                        || m.facts.contains_key(FACT_JS)
                        || !text(m, "vis").is_some_and(|v| v.starts_with("pub"))
                    {
                        continue;
                    }
                    let margs = parse_attrs(list(m, "attrs")).unwrap_or_default();
                    if margs.flag("skip") {
                        continue;
                    }
                    let p = if margs.flag("constructor") {
                        class.clone()
                    } else {
                        format!("{class}.{}", member_name(&margs, sym_name(m)))
                    };
                    if is_glue_only(&class) {
                        continue;
                    }
                    facts.push((c, FACT_JS, p.clone()));
                    binds.push((c, p));
                }
            }
            _ => {}
        }
    }

    for (i, name, v) in facts {
        f.symbols[i].facts.insert(name.to_string(), Fact::Str(v));
    }
    if let Some(key) = key {
        for (i, p) in binds {
            let span = f.symbols[i].span;
            f.bindings.push(Binding {
                symbol: ids[i],
                contract: contract(&key, &p),
                role: ContractRole::Provides,
                span: Some(span),
                confidence: Confidence::Extracted,
                origin: None,
            });
        }
        f.refs.extend(refs);
    }
}

/// Section 6.2 for one extern item: pushes its `wasm_import` fact and returns the reference
/// (with a placeholder `from`, which the caller replaces).
fn extern_item(
    s: &SymbolNode,
    i: usize,
    krate: Option<CrateRef<'_>>,
    facts: &mut Vec<(usize, &'static str, String)>,
) -> Option<OutRef> {
    let block = parse_attrs(list(s, "extern_attrs"))?;
    let item = parse_attrs(list(s, "attrs")).unwrap_or_default();
    let eff = |k: &str| item.value(k).or_else(|| block.value(k)).map(str::to_string);
    let (module, raw_module, inline_js, ns) = (
        eff("module"),
        eff("raw_module"),
        item.has("inline_js") || block.has("inline_js"),
        eff("js_namespace"),
    );
    let name = sym_name(s);
    let signature = text(s, "signature").unwrap_or("");
    let own = item.value("js_name").unwrap_or(name);

    let path = if let Some(c) = item.value("static_method_of") {
        format!("{c}.{own}")
    } else if item.flag("method") {
        let class = item
            .value("js_class")
            .map(str::to_string)
            .or_else(|| this_type(signature));
        match class {
            Some(c) => format!("{c}.{}", member_name(&item, name)),
            None => member_name(&item, name),
        }
    } else if item.flag("constructor") {
        item.value("js_class")
            .map(str::to_string)
            .or_else(|| return_type(signature))
            .unwrap_or_else(|| own.to_string())
    } else {
        own.to_string()
    };
    let head = path.split('.').next().unwrap_or(&path).to_string();
    let fact_path = match &ns {
        Some(n) => format!("{n}.{path}"),
        None => path.clone(),
    };
    let fact = if let Some(m) = &module {
        format!("{m}:{fact_path}")
    } else if let Some(m) = &raw_module {
        format!("raw:{m}:{fact_path}")
    } else if inline_js {
        format!("inline:{fact_path}")
    } else {
        format!("global:{fact_path}")
    };
    facts.push((i, FACT_IMPORT, fact));

    let kind = if s.kind == "fn" {
        "calls"
    } else {
        "references"
    };
    let make = |kind: &str, candidates: Vec<ExportKey>, confidence| OutRef {
        from: Id([0; 16]),
        kind: LinkKind(kind.to_string()),
        span: Some(s.span),
        candidates,
        confidence,
        origin: None,
        binds: None,
    };
    let export = |file: String| ExportKey {
        namespace: NS_EXPORT.to_string(),
        key: format!("{file}#{head}"),
    };
    if inline_js && module.is_none() {
        return None;
    }
    let dir = krate?.dir;
    if let Some(m) = &module {
        if let Some(rest) = m.strip_prefix('/') {
            let file = join_norm(dir, rest)?;
            return Some(make(kind, vec![export(file)], Confidence::Extracted));
        }
        if m.starts_with('.') {
            return None;
        }
        // A bare name: an npm package, with any subpath dropped.
        let mut parts = m.split('/');
        let first = parts.next().unwrap_or("");
        let pkg = if first.starts_with('@') {
            format!("{first}/{}", parts.next().unwrap_or(""))
        } else {
            first.to_string()
        };
        return Some(make(
            "imports",
            vec![ExportKey {
                namespace: NS_PACKAGE.to_string(),
                key: pkg,
            }],
            Confidence::Extracted,
        ));
    }
    if let Some(r) = &raw_module {
        let rel = r.trim_start_matches("./").trim_start_matches('/');
        let cands: Vec<ExportKey> = [format!("{}/{rel}", crate::DEFAULT_OUT_DIR), rel.to_string()]
            .iter()
            .filter_map(|p| join_norm(dir, p))
            .map(export)
            .collect();
        return Some(make(kind, cands, Confidence::Inferred));
    }
    None
}

#[cfg(test)]
mod jsname {
    use super::*;
    use katalogr_model::{ArtifactName, ArtifactNode, Link, Provenance, Segment, Span, SymbolName};
    use std::collections::BTreeMap;

    pub(super) struct B {
        pub f: Fragment,
    }

    pub(super) fn span() -> Span {
        Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 2,
        }
    }

    impl B {
        pub fn new() -> B {
            let artifact = ArtifactName::new("fx", "src/lib.rs").unwrap();
            B {
                f: Fragment {
                    provenance: Provenance {
                        repo: "fx".into(),
                        path: artifact.path.clone(),
                        revision: "r".into(),
                    },
                    extension: "rust".into(),
                    artifact: ArtifactNode {
                        name: artifact,
                        facts: BTreeMap::new(),
                    },
                    symbols: Vec::new(),
                    links: Vec::new(),
                    refs: Vec::new(),
                    exports: Vec::new(),
                    bindings: Vec::new(),
                },
            }
        }

        /// Add a symbol below the symbol `parent` (an index), returning its index.
        pub fn sym(
            &mut self,
            parent: Option<usize>,
            kind: &str,
            name: &str,
            attrs: &[&str],
            vis: &str,
            sig: &str,
        ) -> usize {
            let mut segments = match parent {
                Some(p) => self.f.symbols[p].name.segments.clone(),
                None => Vec::new(),
            };
            segments.push(Segment {
                kind: kind.into(),
                name: name.into(),
            });
            let ordinal = 1 + self
                .f
                .symbols
                .iter()
                .filter(|s| s.name.segments == segments)
                .count() as u32;
            let name = SymbolName {
                artifact: self.f.artifact.name.clone(),
                segments,
                ordinal,
            };
            let mut facts = BTreeMap::new();
            facts.insert("vis".to_string(), Fact::Str(vis.into()));
            facts.insert("signature".to_string(), Fact::Str(sig.into()));
            if !attrs.is_empty() {
                facts.insert(
                    "attrs".to_string(),
                    Fact::List(attrs.iter().map(|a| Fact::Str(a.to_string())).collect()),
                );
            }
            let id = name.id();
            if let Some(p) = parent {
                let from = self.f.symbols[p].name.id();
                self.f.links.push(Link {
                    from,
                    to: id,
                    kind: LinkKind("contains".into()),
                    confidence: Confidence::Extracted,
                    span: Some(span()),
                    provenance: self.f.provenance.clone(),
                });
            }
            self.f.symbols.push(SymbolNode {
                name,
                kind: kind.into(),
                span: span(),
                facts,
            });
            self.f.symbols.len() - 1
        }

        pub fn js(&self, i: usize) -> Option<&str> {
            text(&self.f.symbols[i], FACT_JS)
        }
    }

    fn keys(f: &Fragment) -> Vec<String> {
        f.bindings.iter().map(|b| b.contract.key.clone()).collect()
    }

    #[test]
    fn impl_members_named() {
        let mut b = B::new();
        let f1 = b.sym(
            None,
            "fn",
            "say_hi",
            &["wasm_bindgen(js_name = sayHi)"],
            "pub",
            "pub fn say_hi()",
        );
        let imp = b.sym(
            None,
            "impl",
            "Counter",
            &["wasm_bindgen"],
            "private",
            "impl Counter",
        );
        let new = b.sym(
            Some(imp),
            "fn",
            "new",
            &["wasm_bindgen(constructor)"],
            "pub",
            "pub fn new() -> Counter",
        );
        let inc = b.sym(
            Some(imp),
            "fn",
            "increment",
            &[],
            "pub",
            "pub fn increment(&mut self)",
        );
        let get = b.sym(
            Some(imp),
            "fn",
            "value",
            &["wasm_bindgen(getter)"],
            "pub",
            "pub fn value(&self)",
        );
        let set = b.sym(
            Some(imp),
            "fn",
            "set_value",
            &["wasm_bindgen(setter)"],
            "pub",
            "pub fn set_value(&mut self)",
        );
        let helper = b.sym(Some(imp), "fn", "helper", &[], "private", "fn helper()");
        let skipped = b.sym(
            Some(imp),
            "fn",
            "hidden",
            &["wasm_bindgen(skip)"],
            "pub",
            "pub fn hidden()",
        );
        let imp2 = b.sym(
            None,
            "impl",
            "Default for Counter",
            &["wasm_bindgen"],
            "private",
            "impl Default for Counter",
        );
        let d = b.sym(Some(imp2), "fn", "default", &[], "pub", "fn default()");
        let imp3 = b.sym(
            None,
            "impl",
            "RsThing<T>",
            &["wasm_bindgen(js_class = Thing)"],
            "private",
            "impl RsThing<T>",
        );
        let m3 = b.sym(Some(imp3), "fn", "go", &[], "pub", "pub fn go()");
        let imp4 = b.sym(
            None,
            "impl",
            "other::Plain<T>",
            &["wasm_bindgen"],
            "private",
            "impl",
        );
        let m4 = b.sym(
            Some(imp4),
            "fn",
            "run",
            &["wasm_bindgen(js_name = \"runIt\")"],
            "pub",
            "pub fn run()",
        );
        let st = b.sym(
            None,
            "fn",
            "boot",
            &["wasm_bindgen(start)"],
            "private",
            "fn boot()",
        );
        let en = b.sym(
            None,
            "enum",
            "Color",
            &["wasm_bindgen"],
            "pub",
            "pub enum Color",
        );
        let red = b.sym(Some(en), "variant", "Red", &[], "private", "Red");
        let plain = b.sym(None, "fn", "plain", &["inline"], "pub", "pub fn plain()");
        let other = b.sym(
            None,
            "fn",
            "free",
            &["wasm_bindgen"],
            "pub",
            "pub fn free()",
        );
        apply(
            &mut b.f,
            Some(CrateRef {
                name: "Demo-Wasm",
                dir: "",
            }),
        );
        assert_eq!(b.js(f1), Some("sayHi"));
        assert_eq!(b.js(new), Some("Counter"));
        assert_eq!(b.js(inc), Some("Counter.increment"));
        assert_eq!(b.js(get), Some("Counter.value"));
        assert_eq!(b.js(set), Some("Counter.value"));
        assert_eq!(b.js(helper), None);
        assert_eq!(b.js(skipped), None);
        assert_eq!(b.js(d), None);
        assert_eq!(b.js(m3), Some("Thing.go"));
        assert_eq!(b.js(m4), Some("Plain.runIt"));
        assert_eq!(b.js(st), Some("(start)"));
        assert_eq!(b.js(en), Some("Color"));
        assert_eq!(b.js(red), Some("Color.Red"));
        assert_eq!(b.js(plain), None);
        assert_eq!(b.js(other), None, "glue-only name");
        let k = keys(&b.f);
        for want in [
            "demo_wasm/sayHi",
            "demo_wasm/Counter",
            "demo_wasm/Counter.increment",
            "demo_wasm/Counter.value",
            "demo_wasm/Thing.go",
            "demo_wasm/Plain.runIt",
            "demo_wasm/Color",
            "demo_wasm/Color.Red",
        ] {
            assert!(k.contains(&want.to_string()), "{want} in {k:?}");
        }
        assert_eq!(k.len(), 9, "{k:?}"); // Counter.value is bound by getter and setter
        assert!(
            b.f.bindings
                .iter()
                .all(|x| x.role == ContractRole::Provides)
        );
    }

    #[test]
    fn no_crate_gives_facts_only() {
        let mut b = B::new();
        let f1 = b.sym(
            None,
            "fn",
            "greet",
            &["wasm_bindgen"],
            "pub",
            "pub fn greet()",
        );
        apply(&mut b.f, None);
        assert_eq!(b.js(f1), Some("greet"));
        assert!(b.f.bindings.is_empty());
    }

    #[test]
    fn idempotent() {
        let mut b = B::new();
        b.sym(
            None,
            "fn",
            "greet",
            &["wasm_bindgen"],
            "pub",
            "pub fn greet()",
        );
        let imp = b.sym(None, "impl", "C", &["wasm_bindgen"], "private", "impl C");
        b.sym(Some(imp), "fn", "m", &[], "pub", "pub fn m()");
        let e = b.sym(
            None,
            "fn",
            "now",
            &["wasm_bindgen"],
            "private",
            "fn now() -> f64",
        );
        b.f.symbols[e].facts.insert(
            "extern_attrs".into(),
            Fact::List(vec![Fact::Str(
                "wasm_bindgen(module = \"/js/host.js\")".into(),
            )]),
        );
        let k = Some(CrateRef { name: "c", dir: "" });
        apply(&mut b.f, k);
        let once = b.f.clone();
        apply(&mut b.f, k);
        assert_eq!(once, b.f);
        assert!(!once.bindings.is_empty() && !once.refs.is_empty());
    }

    #[test]
    fn extern_paths_and_refs() {
        let mut b = B::new();
        let block = "wasm_bindgen(module = \"/js/host.js\")";
        let mk = |b: &mut B, kind: &str, name: &str, attrs: &[&str], sig: &str, ea: &str| {
            let i = b.sym(None, kind, name, attrs, "private", sig);
            b.f.symbols[i].facts.insert(
                "extern_attrs".into(),
                Fact::List(vec![Fact::Str(ea.into())]),
            );
            i
        };
        let now = mk(&mut b, "fn", "now", &[], "fn now() -> f64", block);
        let clock = mk(&mut b, "type", "Clock", &[], "type Clock", block);
        let tick = mk(
            &mut b,
            "fn",
            "tick",
            &["wasm_bindgen(method)"],
            "fn tick(this: &Clock)",
            block,
        );
        let mk2 = mk(
            &mut b,
            "fn",
            "make",
            &["wasm_bindgen(constructor)"],
            "fn make() -> Clock",
            block,
        );
        let log = mk(
            &mut b,
            "fn",
            "log",
            &["wasm_bindgen(js_namespace = console)"],
            "fn log(s: &str)",
            "wasm_bindgen",
        );
        let alert = mk(
            &mut b,
            "fn",
            "alert",
            &[],
            "fn alert(s: &str)",
            "wasm_bindgen",
        );
        let dep = mk(
            &mut b,
            "fn",
            "dep",
            &[],
            "fn dep()",
            "wasm_bindgen(module = \"@acme/lib/sub\")",
        );
        let raw = mk(
            &mut b,
            "fn",
            "r",
            &[],
            "fn r()",
            "wasm_bindgen(raw_module = \"./x.js\")",
        );
        let stat = mk(
            &mut b,
            "fn",
            "util",
            &["wasm_bindgen(static_method_of = Clock, js_name = parse)"],
            "fn util()",
            block,
        );
        apply(
            &mut b.f,
            Some(CrateRef {
                name: "c",
                dir: "crates/c",
            }),
        );
        let imp = |i: usize| text(&b.f.symbols[i], FACT_IMPORT).unwrap().to_string();
        assert_eq!(imp(now), "/js/host.js:now");
        assert_eq!(imp(clock), "/js/host.js:Clock");
        assert_eq!(imp(tick), "/js/host.js:Clock.tick");
        assert_eq!(imp(mk2), "/js/host.js:Clock");
        assert_eq!(imp(log), "global:console.log");
        assert_eq!(imp(alert), "global:alert");
        assert_eq!(imp(raw), "raw:./x.js:r");
        assert_eq!(imp(stat), "/js/host.js:Clock.parse");
        let r = |i: usize| b.f.refs.iter().find(|r| r.from == b.f.symbols[i].name.id());
        assert_eq!(r(now).unwrap().candidates[0].key, "crates/c/js/host.js#now");
        assert_eq!(r(now).unwrap().kind.0, "calls");
        assert_eq!(r(clock).unwrap().kind.0, "references");
        assert_eq!(
            r(tick).unwrap().candidates[0].key,
            "crates/c/js/host.js#Clock"
        );
        assert!(r(log).is_none() && r(alert).is_none());
        assert_eq!(r(dep).unwrap().kind.0, "imports");
        assert_eq!(r(dep).unwrap().candidates[0].key, "@acme/lib");
        let rr = r(raw).unwrap();
        assert_eq!(rr.confidence, Confidence::Inferred);
        assert_eq!(rr.candidates.len(), 2);
        assert_eq!(rr.candidates[0].key, "crates/c/pkg/x.js#r");
    }

    #[test]
    fn type_helpers() {
        assert_eq!(this_type("fn tick(this: &Clock)").as_deref(), Some("Clock"));
        assert_eq!(
            this_type("pub fn t(this: &mut a::Foo<T>, x: i32)").as_deref(),
            Some("Foo")
        );
        assert_eq!(this_type("fn t(this: &'a Foo)").as_deref(), Some("Foo"));
        assert_eq!(this_type("fn t()"), None);
        assert_eq!(return_type("fn m() -> Clock").as_deref(), Some("Clock"));
        assert_eq!(return_type("fn m() -> a::B<T>").as_deref(), Some("B"));
        assert_eq!(return_type("fn m()"), None);
    }
}
