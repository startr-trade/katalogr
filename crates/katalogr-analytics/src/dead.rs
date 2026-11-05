//! Dead-code candidates, classed by confidence (FEAT-030 section 6.6). A candidate is a symbol
//! of a code kind that no entry point reaches. Reachability is computed twice: over strong links
//! only, and over all links; the difference is what is reached only through inferred or
//! fan-out links.

use crate::graph::LinkSink;
use crate::options::DeadCodeOptions;
use crate::result::{DeadCandidate, DeadClass, DeadCodeReport, DeadConfidence};
use katalogr_model::insight::{FACT_ROOT, ROOT_API};
use katalogr_model::{Fact, Id, Name};
use katalogr_query::result::SymbolHit;
use katalogr_store::{ArtifactRow, Reader, StoreError, SymbolRow};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Why a provider of a contract is an entry point.
const ROOT_CONTRACT: &str = "contract";

/// Edges in compressed form: `start[v]..start[v + 1]` indexes `to` and `strong`.
struct Adjacency {
    start: Vec<u32>,
    to: Vec<u32>,
    strong: Vec<bool>,
}

impl Adjacency {
    fn new(n: usize, mut edges: Vec<(u32, u32, bool)>) -> Adjacency {
        edges.sort_unstable_by_key(|e| (e.0, e.1));
        let mut start = vec![0u32; n + 1];
        for e in &edges {
            start[e.0 as usize + 1] += 1;
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        Adjacency {
            start,
            to: edges.iter().map(|e| e.1).collect(),
            strong: edges.iter().map(|e| e.2).collect(),
        }
    }

    /// Nodes reachable from `roots`, following only strong edges when `strong_only`.
    fn reach(&self, roots: &[u32], strong_only: bool) -> Vec<bool> {
        let mut seen = vec![false; self.start.len() - 1];
        let mut stack: Vec<u32> = Vec::new();
        for &r in roots {
            if !seen[r as usize] {
                seen[r as usize] = true;
                stack.push(r);
            }
        }
        while let Some(v) = stack.pop() {
            for i in self.start[v as usize] as usize..self.start[v as usize + 1] as usize {
                let w = self.to[i];
                if (!strong_only || self.strong[i]) && !seen[w as usize] {
                    seen[w as usize] = true;
                    stack.push(w);
                }
            }
        }
        seen
    }
}

fn root_reason(facts: &BTreeMap<String, Fact>) -> Option<&str> {
    match facts.get(FACT_ROOT) {
        Some(Fact::Str(r)) => Some(r.as_str()),
        _ => None,
    }
}

/// Candidates of the code kinds in `opts.code_kinds`, with the counts by confidence. When an
/// extension that declares code kinds has symbols but the index records no entry point for it,
/// the report holds no candidates and says to re-extract.
pub fn dead_code(
    tx: &(impl Reader + Sync),
    opts: &DeadCodeOptions,
) -> Result<DeadCodeReport, StoreError> {
    // The symbols (with their facts) and artifacts are read side by side; the links are walked
    // without being decoded in full.
    let (symbols, artifacts) = std::thread::scope(|scope| {
        let artifacts = scope.spawn(|| tx.artifacts());
        let symbols = tx.symbols();
        Ok::<_, StoreError>((
            symbols?,
            artifacts.join().expect("a reader thread panicked")?,
        ))
    })?;
    dead_code_from(symbols, artifacts, opts, &mut |f| tx.for_each_link(f))
}

/// [`dead_code`] over the given rows, with the links delivered by `walk` in `links_all` order.
pub(crate) fn dead_code_from(
    symbols: Vec<(Id, SymbolRow)>,
    artifacts: Vec<(Id, ArtifactRow)>,
    opts: &DeadCodeOptions,
    walk: &mut dyn FnMut(&mut LinkSink<'_>) -> Result<(), StoreError>,
) -> Result<DeadCodeReport, StoreError> {
    let sym_count = symbols.len();
    let mut index: HashMap<Id, u32> = HashMap::with_capacity(sym_count + artifacts.len());
    for (i, (id, _)) in symbols.iter().enumerate() {
        index.insert(*id, i as u32);
    }
    for (i, (id, _)) in artifacts.iter().enumerate() {
        index.insert(*id, (sym_count + i) as u32);
    }
    let n = sym_count + artifacts.len();
    let extension_of_artifact: HashMap<Id, &str> = artifacts
        .iter()
        .map(|(id, a)| (*id, a.extension.as_str()))
        .collect();
    let extension_of = |v: usize| -> &str {
        if v < sym_count {
            extension_of_artifact
                .get(&symbols[v].1.artifact)
                .copied()
                .unwrap_or("")
        } else {
            artifacts[v - sym_count].1.extension.as_str()
        }
    };

    // Edges, incoming counts and providers.
    let mut edges: Vec<(u32, u32, bool)> = Vec::new();
    let mut incoming = vec![0u64; n];
    let mut weak_incoming = vec![0u64; n];
    let mut providers: BTreeSet<u32> = BTreeSet::new();
    walk(
        &mut |from: Id, kind: &str, to: Id, _weight: f64, strong: bool| {
            let Some(&a) = index.get(&from) else {
                return;
            };
            if kind == "provides" {
                providers.insert(a);
                return;
            }
            let Some(&b) = index.get(&to) else {
                return;
            };
            if kind == "contains" {
                // A reached member makes its container reachable.
                edges.push((b, a, true));
                return;
            }
            if opts.exclude_kinds.iter().any(|x| x == kind) || a == b {
                return;
            }
            edges.push((a, b, strong));
            incoming[b as usize] += 1;
            if !strong {
                weak_incoming[b as usize] += 1;
            }
        },
    )?;
    for (i, (_, row)) in symbols.iter().enumerate() {
        if let Some(&art) = index.get(&row.artifact) {
            edges.push((i as u32, art, true));
        }
    }
    let adjacency = Adjacency::new(n, edges);

    // Roots, and the extensions that record any.
    let fact_of = |v: usize| -> Option<&str> {
        if v < sym_count {
            root_reason(&symbols[v].1.facts)
        } else {
            root_reason(&artifacts[v - sym_count].1.facts)
        }
    };
    let mut roots: Vec<u32> = Vec::new();
    let mut roots_by_reason: BTreeMap<String, u64> = BTreeMap::new();
    let mut rooted_extensions: BTreeSet<&str> = BTreeSet::new();
    for v in 0..n {
        let reason = fact_of(v);
        if reason.is_some() || providers.contains(&(v as u32)) {
            rooted_extensions.insert(extension_of(v));
        }
        let counted = match reason {
            Some(r) if opts.internal && r == ROOT_API => None,
            Some(r) => Some(r),
            None if providers.contains(&(v as u32)) => Some(ROOT_CONTRACT),
            None => None,
        };
        if let Some(r) = counted {
            roots.push(v as u32);
            *roots_by_reason.entry(r.to_string()).or_default() += 1;
        }
    }

    // The guard: code kinds declared and symbols present, but no entry point recorded.
    let mut unrooted: BTreeSet<&str> = BTreeSet::new();
    for (v, (_, row)) in symbols.iter().enumerate() {
        let ext = extension_of(v);
        let Some(kinds) = opts.code_kinds.get(ext) else {
            continue;
        };
        if kinds.contains(&row.kind) && !rooted_extensions.contains(ext) {
            unrooted.insert(ext);
        }
    }
    let mut report = DeadCodeReport {
        roots: roots.len() as u64,
        roots_by_reason,
        candidates_total: 0,
        high: 0,
        medium: 0,
        low: 0,
        note: None,
        items: Vec::new(),
    };
    if !unrooted.is_empty() {
        report.note = Some(format!(
            "no entry points recorded for {}; run `katalogr index --full` to re-extract",
            unrooted.into_iter().collect::<Vec<_>>().join(",")
        ));
        return Ok(report);
    }

    let strong = adjacency.reach(&roots, true);
    let any = adjacency.reach(&roots, false);
    let is_root: BTreeSet<u32> = roots.iter().copied().collect();
    let mut found: Vec<(DeadConfidence, DeadClass, usize)> = Vec::new();
    for (v, (_, row)) in symbols.iter().enumerate() {
        let Some(kinds) = opts.code_kinds.get(extension_of(v)) else {
            continue;
        };
        if !kinds.contains(&row.kind) || is_root.contains(&(v as u32)) || strong[v] {
            continue;
        }
        if let Some(prefix) = &opts.path_prefix
            && !row.name.artifact.path.as_str().starts_with(prefix.as_str())
        {
            continue;
        }
        let (confidence, class) = if any[v] {
            (DeadConfidence::Low, DeadClass::WeaklyReached)
        } else if incoming[v] == 0 {
            (DeadConfidence::High, DeadClass::Unreferenced)
        } else {
            (DeadConfidence::Medium, DeadClass::Unreachable)
        };
        match confidence {
            DeadConfidence::High => report.high += 1,
            DeadConfidence::Medium => report.medium += 1,
            DeadConfidence::Low => report.low += 1,
        }
        if confidence >= opts.min_confidence {
            found.push((confidence, class, v));
        }
    }
    report.candidates_total = report.high + report.medium + report.low;
    found.sort_by(|a, b| {
        let (ra, rb) = (&symbols[a.2].1, &symbols[b.2].1);
        b.0.cmp(&a.0)
            .then_with(|| ra.name.artifact.path.cmp(&rb.name.artifact.path))
            .then_with(|| ra.span.start_line.cmp(&rb.span.start_line))
            .then_with(|| ra.name.canonical().cmp(&rb.name.canonical()))
    });
    report.items = found
        .into_iter()
        .take(opts.limit)
        .map(|(confidence, class, v)| {
            let row = &symbols[v].1;
            DeadCandidate {
                symbol: SymbolHit {
                    name: row.name.canonical(),
                    kind: row.kind.clone(),
                    path: row.name.artifact.path.as_str().to_string(),
                    span: row.span,
                },
                class,
                confidence,
                incoming: incoming[v],
                weak_incoming: weak_incoming[v],
            }
        })
        .collect();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::Fx;
    use katalogr_model::Confidence;

    fn opts(min: DeadConfidence) -> DeadCodeOptions {
        DeadCodeOptions {
            code_kinds: BTreeMap::from([("test".to_string(), BTreeSet::from(["fn".to_string()]))]),
            exclude_kinds: crate::options::STRUCTURAL_KINDS
                .iter()
                .map(|k| k.to_string())
                .collect(),
            internal: false,
            min_confidence: min,
            path_prefix: None,
            limit: 100,
        }
    }

    fn short(name: &str) -> &str {
        name.rsplit(':').next().unwrap()
    }

    #[test]
    fn classes_from_reachability() {
        let mut fx = Fx::new();
        let main = fx.sym("a.tx", "main");
        fx.sym_str(main, "root", "main");
        let used = fx.sym("a.tx", "used");
        let deep = fx.sym("a.tx", "deep");
        fx.sym("a.tx", "lonely");
        let island = fx.sym("a.tx", "island");
        let island_user = fx.sym("a.tx", "island_user");
        let guessed = fx.sym("a.tx", "guessed");
        let api = fx.sym("a.tx", "api");
        fx.sym_str(api, "root", "api");
        let hidden = fx.sym("a.tx", "hidden");
        fx.link(main, "calls", used);
        fx.link(used, "calls", deep);
        fx.link(island_user, "calls", island);
        fx.link_with(main, "calls", guessed, Confidence::Inferred, Some(2));
        fx.link(api, "calls", hidden);
        let built = fx.build();
        let tx = built.read();

        let r = dead_code(&tx, &opts(DeadConfidence::Low)).unwrap();
        let got: Vec<(&str, DeadClass, DeadConfidence)> = r
            .items
            .iter()
            .map(|c| (short(&c.symbol.name), c.class, c.confidence))
            .collect();
        assert_eq!(
            got,
            [
                ("island_user", DeadClass::Unreferenced, DeadConfidence::High),
                ("lonely", DeadClass::Unreferenced, DeadConfidence::High),
                ("island", DeadClass::Unreachable, DeadConfidence::Medium),
                ("guessed", DeadClass::WeaklyReached, DeadConfidence::Low),
            ]
        );
        assert_eq!((r.high, r.medium, r.low, r.candidates_total), (2, 1, 1, 4));
        assert_eq!(r.roots, 2);
        assert_eq!(r.roots_by_reason["main"], 1);
        assert_eq!(r.roots_by_reason["api"], 1);
        assert_eq!(r.note, None);
        let guessed_item = r.items.last().unwrap();
        assert_eq!((guessed_item.incoming, guessed_item.weak_incoming), (1, 1));

        // The default floor hides the low class; the counts still include it.
        let r = dead_code(&tx, &opts(DeadConfidence::Medium)).unwrap();
        assert_eq!(r.items.len(), 3);
        assert_eq!(r.low, 1);

        // Internal mode drops the api root, so what it alone reached becomes a candidate.
        let internal = DeadCodeOptions {
            internal: true,
            ..opts(DeadConfidence::Medium)
        };
        let r = dead_code(&tx, &internal).unwrap();
        let names: Vec<&str> = r.items.iter().map(|c| short(&c.symbol.name)).collect();
        assert!(
            names.contains(&"api") && names.contains(&"hidden"),
            "{names:?}"
        );
        assert_eq!(r.roots_by_reason.get("api"), None);

        // A path prefix and a limit narrow the list.
        let r = dead_code(
            &tx,
            &DeadCodeOptions {
                path_prefix: Some("zzz".into()),
                ..opts(DeadConfidence::Low)
            },
        )
        .unwrap();
        assert!(r.items.is_empty());
        let r = dead_code(
            &tx,
            &DeadCodeOptions {
                limit: 1,
                ..opts(DeadConfidence::Low)
            },
        )
        .unwrap();
        assert_eq!(r.items.len(), 1);
    }

    #[test]
    fn providers_are_roots_and_no_roots_refuses() {
        let mut fx = Fx::new();
        let handler = fx.sym("a.tx", "handler");
        fx.sym("a.tx", "other");
        fx.bind(handler, true, "channel", "orders");
        let built = fx.build();
        let r = dead_code(&built.read(), &opts(DeadConfidence::Low)).unwrap();
        assert_eq!(r.roots_by_reason["contract"], 1);
        assert_eq!(r.items.len(), 1, "only `other` is a candidate");

        // No entry point recorded at all: refuse, and say why.
        let mut fx = Fx::new();
        fx.sym("a.tx", "one");
        fx.sym("a.tx", "two");
        let built = fx.build();
        let r = dead_code(&built.read(), &opts(DeadConfidence::Low)).unwrap();
        assert!(r.items.is_empty());
        assert_eq!(r.candidates_total, 0);
        assert!(r.note.unwrap().contains("index --full"));
    }
}
