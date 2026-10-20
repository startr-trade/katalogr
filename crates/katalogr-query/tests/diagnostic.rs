//! FEAT-065: diagnostic traces explain an answer without changing it.

use camino::Utf8PathBuf;
use katalogr_model::{
    ArtifactName, Confidence, Field, Fragment, Id, LinkKind, Name, Provenance, RelPath, Segment,
    Span, SymbolName, tokenize,
};
use katalogr_query::{
    DIAG_PACK, DIAG_REFS, DIAG_TERMS, DIAG_TOP, DIAG_WEIGHTS, Diagnostic, PackEntry, PackTrace,
    Trace, map, map_traced, refs, refs_traced, resolve, resolve_traced, result::Resolution, search,
    search_traced,
};
use katalogr_store::{
    Cache, Kindex, LEXICON_FIELDS, LinkEntry, LinkKey, LinkOrigin, LinkRow, ProjectConfig,
    ProjectName, RANK_CONTEXT, Reader, RepoConfig, Restitch, StoreError, SymbolRow, WriteTx,
    pagerank,
};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

struct NoShards;

impl Restitch for NoShards {
    fn replace_fragment(&self, _: &mut WriteTx, _: Fragment) -> Result<(), StoreError> {
        Ok(())
    }
    fn remove_artifact(&self, _: &mut WriteTx, _: &ArtifactName) -> Result<(), StoreError> {
        Ok(())
    }
}

const SPAN: Span = Span {
    start_line: 1,
    start_col: 1,
    end_line: 1,
    end_col: 1,
};

/// A cache with no shards, and the one artifact that holds every test symbol.
struct Fx {
    _tmp: tempfile::TempDir,
    cache: Cache,
    artifact: ArtifactName,
    provenance: Provenance,
}

impl Fx {
    fn new() -> Fx {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("diag").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let path = RelPath::new("f.tx").unwrap();
        Fx {
            _tmp: tmp,
            cache,
            artifact: ArtifactName {
                repo: "r".into(),
                path: path.clone(),
            },
            provenance: Provenance {
                repo: "r".into(),
                path,
                revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
            },
        }
    }

    fn name(&self, kind: &str, name: &str, ordinal: u32) -> SymbolName {
        SymbolName {
            artifact: self.artifact.clone(),
            segments: vec![Segment {
                kind: kind.into(),
                name: name.into(),
            }],
            ordinal,
        }
    }

    /// A symbol whose only searchable text is its name, indexed as the engine's lexicon does.
    fn symbol(&self, tx: &mut WriteTx, kind: &str, name: &str, ordinal: u32) -> Id {
        let n = self.name(kind, name, ordinal);
        let id = n.id();
        let mut lens = [0u32; LEXICON_FIELDS];
        let mut terms: BTreeMap<(String, u8), u32> = BTreeMap::new();
        for t in tokenize(name) {
            lens[Field::Name.index()] += 1;
            *terms.entry((t, Field::Name.index() as u8)).or_default() += 1;
        }
        let terms: Vec<(String, u8, u32)> =
            terms.into_iter().map(|((t, f), n)| (t, f, n)).collect();
        tx.put_doc_postings(id, lens, &terms).unwrap();
        tx.put_symbol(
            id,
            &SymbolRow {
                name: n,
                artifact: self.artifact.id(),
                kind: kind.into(),
                span: SPAN,
                facts: Default::default(),
            },
        )
        .unwrap();
        id
    }

    fn link(&self, tx: &mut WriteTx, from: Id, kind: &str, to: Id, fanout: Option<u32>) {
        tx.put_link(
            &LinkKey {
                from,
                kind: LinkKind(kind.into()),
                to,
            },
            &LinkRow {
                entries: vec![LinkEntry {
                    origin: LinkOrigin::Intra,
                    confidence: if fanout.is_some() {
                        Confidence::Inferred
                    } else {
                        Confidence::Extracted
                    },
                    span: Some(SPAN),
                    provenance: self.provenance.clone(),
                    fanout,
                    produced_by: None,
                }],
            },
        )
        .unwrap();
    }
}

/// Symbols named from a small vocabulary, for the search tests.
const NAMES: [&str; 12] = [
    "fn_ember_ax",
    "fn_ember_ay",
    "fn_ember_cache_one",
    "fn_embers_cache",
    "fn_cache_only",
    "fn_cachet_store",
    "fn_ember_cache_two",
    "fn_ember_cache_two_cache",
    "fn_other_thing",
    "fn_third_thing",
    "fn_xyzzy_plugh",
    "fn_ember",
];

fn search_fx() -> Fx {
    let fx = Fx::new();
    let mut tx = fx.cache.write().unwrap();
    for n in NAMES {
        fx.symbol(&mut tx, "fn", n, 1);
    }
    tx.commit().unwrap();
    fx
}

/// A reference graph: `target` is called by `caller_a` (extracted) and `caller_b` (inferred,
/// fanout 3); `alias` re-exports it; `dup` exists twice.
fn graph_fx() -> (Fx, Id) {
    let fx = Fx::new();
    let mut tx = fx.cache.write().unwrap();
    let target = fx.symbol(&mut tx, "fn", "target", 1);
    let alias = fx.symbol(&mut tx, "use", "target", 2);
    let a = fx.symbol(&mut tx, "fn", "caller_a", 1);
    let b = fx.symbol(&mut tx, "fn", "caller_b", 1);
    let top = fx.symbol(&mut tx, "fn", "top", 1);
    fx.symbol(&mut tx, "fn", "dup", 1);
    fx.symbol(&mut tx, "fn", "dup", 2);
    fx.link(&mut tx, alias, "reexports", target, None);
    fx.link(&mut tx, a, "calls", target, None);
    fx.link(&mut tx, b, "calls", target, Some(3));
    fx.link(&mut tx, top, "references", alias, None);
    tx.commit().unwrap();
    (fx, target)
}

fn traced<T>(f: impl FnOnce(&mut Diagnostic) -> T) -> (T, Trace) {
    let mut d = Diagnostic::on_new();
    let v = f(&mut d);
    (v, d.finish().expect("trace"))
}

fn json(t: &Trace) -> String {
    serde_json::to_string(t).unwrap()
}

mod diagnostic {
    use super::*;

    #[test]
    fn answer_unchanged() {
        let fx = search_fx();
        let r = fx.cache.read().unwrap();
        for q in ["ember cache", "ember", "cache  x", "xyzzy", "qq", "emb"] {
            let plain = search(&r, q, 20).unwrap();
            let off = search_traced(&r, q, 20, &mut Diagnostic::off()).unwrap();
            let (on, _) = traced(|d| search_traced(&r, q, 20, d).unwrap());
            assert_eq!(plain, off, "{q}");
            assert_eq!(plain, on, "{q}");
        }
        let (gx, target) = graph_fx();
        let r = gx.cache.read().unwrap();
        let _ = target;
        assert_eq!(map(&r).unwrap(), traced(|d| map_traced(&r, d).unwrap()).0);
        assert_eq!(
            map(&r).unwrap(),
            map_traced(&r, &mut Diagnostic::off()).unwrap()
        );
        for text in ["target", "dup", "nothing", "Target"] {
            let plain = resolve(&r, text).unwrap();
            let (on, _) = traced(|d| resolve_traced(&r, text, d).unwrap());
            assert_eq!(plain, on, "{text}");
            if let Resolution::One(t) = plain {
                let (on, _) = traced(|d| refs_traced(&r, &t, d).unwrap());
                assert_eq!(refs(&r, &t).unwrap(), on);
                assert_eq!(
                    refs(&r, &t).unwrap(),
                    refs_traced(&r, &t, &mut Diagnostic::off()).unwrap()
                );
            }
        }
        assert_eq!(Diagnostic::off().finish(), None);
        assert!(Diagnostic::off().on().is_none());
    }

    #[test]
    fn routing_traced() {
        let fx = search_fx();
        let r = fx.cache.read().unwrap();
        let (_, t) = traced(|d| search_traced(&r, "x ember q cache ember", 20, d).unwrap());
        let routing = t.routing.unwrap();
        assert_eq!(routing.command, "search");
        assert_eq!(routing.input, "x ember q cache ember");
        assert_eq!(routing.kind, "tokens");
        assert_eq!(routing.tokens, ["ember", "cache"]);
        assert_eq!(routing.dropped, ["x", "q"]);
        let (_, t) = traced(|d| search_traced(&r, "x", 20, d).unwrap());
        assert_eq!(t.routing.unwrap().kind, "none");

        let (gx, _) = graph_fx();
        let r = gx.cache.read().unwrap();
        let route = |text: &str| {
            let (res, t) = traced(|d| resolve_traced(&r, text, d).unwrap());
            (res, t.routing.unwrap())
        };
        // Plain name of a symbol and the alias that re-exports it: two candidates, one thing.
        let (res, x) = route("target");
        assert!(matches!(res, Resolution::One(_)));
        assert_eq!(x.command, "refs");
        assert_eq!(x.kind, "name");
        assert_eq!(x.tier.as_deref(), Some("plain"));
        assert_eq!((x.candidates, x.after_collapse), (2, 1));
        let (_, x) = route("Target");
        assert_eq!(x.tier.as_deref(), Some("plain_ci"));
        let (res, x) = route("dup");
        assert!(matches!(res, Resolution::Ambiguous(_)));
        assert_eq!((x.candidates, x.after_collapse), (2, 2));
        let canonical = match route("target").0 {
            Resolution::One(t) => t.name,
            _ => unreachable!(),
        };
        let (_, x) = route(&canonical);
        assert_eq!(x.tier.as_deref(), Some("canonical"));
        let (res, x) = route("nothing");
        assert_eq!(res, Resolution::None);
        assert_eq!(x.kind, "none");
        assert_eq!(x.tier, None);
        assert_eq!((x.candidates, x.after_collapse), (0, 0));
    }

    #[test]
    fn rank_parts_sum_to_score() {
        let fx = search_fx();
        let r = fx.cache.read().unwrap();
        let (hits, t) = traced(|d| search_traced(&r, "ember cache", 20, d).unwrap());
        assert_eq!(t.ranking.len(), DIAG_TOP.min(hits.len()));
        assert!(!t.ranking.is_empty());
        for (i, rt) in t.ranking.iter().enumerate() {
            assert_eq!(rt.position, i as u64 + 1);
            assert_eq!(rt.symbol, hits[i].symbol.name);
            assert_eq!(rt.score, hits[i].score);
            let sum: f64 = rt.parts.iter().map(|p| p.contribution).sum();
            assert!(
                (sum - rt.score).abs() <= 1e-6 * rt.parts.len() as f64,
                "{}: {sum} vs {}",
                rt.symbol,
                rt.score
            );
            for p in &rt.parts {
                assert!(["exact", "prefix"].contains(&p.via.as_str()));
                assert_eq!(p.factor, if p.via == "exact" { 1.0 } else { 0.5 });
            }
        }
        // `fn_ember_ax` and `fn_ember_ay` score the same for "ember": the name order decides.
        let (hits, t) = traced(|d| search_traced(&r, "ember", 20, d).unwrap());
        let pos = |n: &str| {
            hits.iter()
                .position(|h| h.symbol.name.ends_with(n))
                .unwrap_or_else(|| panic!("{n}"))
        };
        let (ax, ay) = (pos("fn:fn_ember_ax"), pos("fn:fn_ember_ay"));
        assert_eq!(ay, ax + 1);
        assert_eq!(hits[ax].score, hits[ay].score);
        assert!(t.ranking[ax].tied_with_next);
        assert!(!t.ranking[ay].tied_with_next || hits[ay].score == hits[ay + 1].score);
        assert!(!t.ranking.last().unwrap().tied_with_next || hits.len() > t.ranking.len());
    }

    #[test]
    fn retrieval_counts() {
        let fx = search_fx();
        let r = fx.cache.read().unwrap();
        let (_, t) = traced(|d| search_traced(&r, "ember cache", 20, d).unwrap());
        assert_eq!(t.terms.len(), 2);
        let docs = r.lexicon_stats().unwrap().docs as f64;
        for term in &t.terms {
            assert_eq!(
                term.exact_postings,
                r.postings(&term.token).unwrap().len() as u64
            );
            let mut matching: BTreeSet<Id> = BTreeSet::new();
            for p in r.postings(&term.token).unwrap() {
                matching.insert(p.symbol);
            }
            for (_, ps) in r.postings_prefix(&term.token, 50).unwrap() {
                matching.extend(ps.iter().map(|p| p.symbol));
            }
            assert_eq!(term.matching_docs, matching.len() as u64, "{}", term.token);
            let n = matching.len() as f64;
            let idf = (1.0 + (docs - n + 0.5) / (n + 0.5)).ln();
            assert_eq!(term.idf, (idf * 1e6).round() / 1e6, "{}", term.token);
        }
        assert_eq!(t.terms[0].token, "ember");
        assert_eq!(t.terms[1].token, "cache");
    }

    #[test]
    fn prefix_expansions_traced() {
        let fx = Fx::new();
        let mut tx = fx.cache.write().unwrap();
        // 60 distinct terms that "qux" prefixes, and a two-letter token.
        for a in 0..6u8 {
            for b in 0..10u8 {
                let name = format!("qux{}{}", (b'a' + a) as char, (b'k' + b) as char);
                fx.symbol(&mut tx, "fn", &name, 1);
            }
        }
        fx.symbol(&mut tx, "fn", "ab", 1);
        fx.symbol(&mut tx, "fn", "abc", 1);
        tx.commit().unwrap();
        let r = fx.cache.read().unwrap();
        let (_, t) = traced(|d| search_traced(&r, "qux", 5, d).unwrap());
        let p = &t.terms[0].prefix;
        assert!(p.attempted);
        assert_eq!(p.expanded, 50);
        assert!(p.cap_hit);
        assert_eq!(p.factor, 0.5);
        assert_eq!(p.terms.len(), DIAG_TERMS);
        assert_eq!(t.omitted["prefix_terms"], 50 - DIAG_TERMS as u64);
        assert!(p.terms.iter().all(|e| e.term.starts_with("qux")));
        assert!(p.terms.iter().all(|e| e.postings >= 1));

        let (_, t) = traced(|d| search_traced(&r, "ab", 5, d).unwrap());
        let p = &t.terms[0].prefix;
        assert!(!p.attempted);
        assert_eq!((p.expanded, p.cap_hit, p.terms.len()), (0, false, 0));
        assert_eq!(t.terms[0].exact_postings, 1);

        // Expansion under the cap.
        let (_, t) = traced(|d| search_traced(&r, "abc", 5, d).unwrap());
        let p = &t.terms[0].prefix;
        assert!(p.attempted && !p.cap_hit);
        assert_eq!(p.expanded, 0);
    }

    #[test]
    fn pack_trace_matches_fit_budget() {
        use katalogr_query::budget::{fit_budget, pack_trace};
        use proptest::prelude::*;
        let config = ProptestConfig {
            failure_persistence: None,
            ..ProptestConfig::default()
        };
        proptest!(config, |(
            lines in proptest::collection::vec("[a-z ]{0,40}", 0..60),
            budget in 0u64..120,
        )| {
            let fit = fit_budget(&lines, budget);
            let p = pack_trace(&lines, budget);
            prop_assert_eq!(p.kept, fit.kept as u64);
            prop_assert_eq!(p.omitted, fit.omitted as u64);
            prop_assert_eq!(p.considered, lines.len() as u64);
            prop_assert!(p.used_tokens <= budget);
            let kept_tokens: u64 = p.entries.iter().filter(|e| e.kept).map(|e| e.tokens).sum();
            if fit.kept <= DIAG_PACK {
                prop_assert_eq!(kept_tokens, p.used_tokens);
            }
            prop_assert_eq!(
                p.entries.iter().filter(|e| !e.kept).count(),
                usize::from(fit.omitted > 0)
            );
            prop_assert_eq!(p.first_omitted_tokens.is_some(), fit.omitted > 0);
            prop_assert!(p.entries.len() <= DIAG_PACK + 1);
        });
        let lines = ["aaaa", "bbbbbbbb", "cc"];
        let p = pack_trace(&lines, 3);
        assert_eq!((p.kept, p.omitted, p.used_tokens), (1, 2, 2));
        assert_eq!(p.first_omitted_tokens, Some(2));
        assert_eq!(p.estimator, "ceil(chars/4)");
        let _ = PackTrace {
            entries: vec![PackEntry {
                index: 0,
                tokens: 1,
                kept: true,
                tier: None,
                reason: None,
            }],
            ..p
        };
    }

    #[test]
    fn link_weights_traced() {
        let (fx, _) = graph_fx();
        let r = fx.cache.read().unwrap();
        let Resolution::One(target) = resolve(&r, "target").unwrap() else {
            panic!("target");
        };
        let (hits, t) = traced(|d| refs_traced(&r, &target, d).unwrap());
        assert_eq!(t.weights.len(), hits.len());
        let by_from = |s: &str| t.weights.iter().find(|w| w.from.contains(s)).unwrap();
        let a = by_from("caller_a");
        assert_eq!(
            (a.kind.as_str(), a.confidence.as_str()),
            ("calls", "Extracted")
        );
        assert_eq!((a.fanout, a.weight), (None, 1.0));
        let b = by_from("caller_b");
        assert_eq!(b.confidence, "Inferred");
        assert_eq!((b.fanout, b.weight), (Some(3), 0.333333));
        assert_eq!(b.to, target.name);
        // The link that arrives through the re-export names the alias as its end.
        let top = by_from("fn:top");
        assert!(top.to.contains("use:target"), "{}", top.to);

        // Map: live until a rank is stored at the current watermark, live again after a change.
        let (_, t) = traced(|d| map_traced(&r, d).unwrap());
        let s = t.rank_source.unwrap();
        assert_eq!(s.source, "live");
        assert_eq!(s.rank_watermark, None);
        assert_eq!((s.damping, s.iterations), (0.85, 30));
        assert!(!t.weights.is_empty());
        let heavy: Vec<_> = t.weights.iter().filter(|w| w.to == target.name).collect();
        assert_eq!(heavy.len(), 3);
        assert!(heavy.windows(2).all(|p| p[0].weight >= p[1].weight));
        drop(r);

        let mut tx = fx.cache.write().unwrap();
        let symbols = tx.symbols().unwrap();
        let scores = pagerank(&tx, &symbols).unwrap();
        let stored: Vec<(Id, f64)> = symbols.iter().map(|(i, _)| *i).zip(scores).collect();
        let wm = tx.watermark(RANK_CONTEXT).unwrap();
        tx.store_rank(&stored, wm).unwrap();
        tx.commit().unwrap();
        let r = fx.cache.read().unwrap();
        let (cached, t) = traced(|d| map_traced(&r, d).unwrap());
        let s = t.rank_source.unwrap();
        assert_eq!((s.source.as_str(), s.rank_watermark), ("cached", Some(wm)));
        assert_eq!(cached, map(&r).unwrap());
        drop(r);

        let mut tx = fx.cache.write().unwrap();
        fx.symbol(&mut tx, "fn", "late", 1);
        tx.commit().unwrap();
        let r = fx.cache.read().unwrap();
        let (_, t) = traced(|d| map_traced(&r, d).unwrap());
        assert_eq!(t.rank_source.unwrap().source, "live");
        // Per-symbol cap.
        assert!(t.weights.len() <= DIAG_TOP * DIAG_WEIGHTS);
        let _ = DIAG_REFS;
    }

    /// A project of 500 symbols named from a 60-word vocabulary.
    fn big_fx() -> (Fx, Vec<String>) {
        let heads = ["ba", "ce", "di", "fo", "gu", "hy", "ja", "ke", "li", "mo"];
        let tails = ["ra", "ste", "tol", "ven", "wix", "yar"];
        let words: Vec<String> = heads
            .iter()
            .flat_map(|h| tails.iter().map(move |t| format!("{h}{t}")))
            .collect();
        let fx = Fx::new();
        let mut tx = fx.cache.write().unwrap();
        let mut ids = Vec::new();
        for i in 0..500usize {
            let name = format!(
                "fn_{}_{}_{}",
                words[(i * 7) % words.len()],
                words[(i * 13 + 5) % words.len()],
                // Digits are tokens too, so the ordinal keeps names distinct.
                i
            );
            ids.push(fx.symbol(&mut tx, "fn", &name, 1));
        }
        for i in 0..500usize {
            fx.link(&mut tx, ids[i], "calls", ids[(i * 31 + 7) % 500], None);
        }
        tx.commit().unwrap();
        (fx, words)
    }

    #[test]
    fn trace_is_bounded() {
        let (fx, words) = big_fx();
        let r = fx.cache.read().unwrap();
        // Cap helper.
        let mut t = Trace::default();
        let mut v: Vec<u32> = (0..30).collect();
        t.cap(&mut v, 20, "things");
        assert_eq!((v.len(), t.omitted["things"]), (20, 10));
        t.cap(&mut v, 20, "things");
        assert_eq!(t.omitted["things"], 10);

        let query = words[..50].join(" ");
        let (hits, t) = traced(|d| search_traced(&r, &query, 20, d).unwrap());
        assert_eq!(t.terms.len(), 50);
        assert!(t.terms.iter().all(|x| x.prefix.terms.len() <= DIAG_TERMS));
        assert!(t.ranking.len() <= DIAG_TOP);
        assert_eq!(
            t.omitted.get("ranking").copied().unwrap_or(0),
            hits.len().saturating_sub(DIAG_TOP) as u64
        );
        let size = json(&t).len();
        assert!(size < 32 * 1024, "search trace is {size} bytes");

        let (_, m) = traced(|d| map_traced(&r, d).unwrap());
        assert!(m.weights.len() <= DIAG_TOP * DIAG_WEIGHTS);
        assert!(json(&m).len() < 32 * 1024);
        let Resolution::One(first) = resolve(&r, &hits[0].symbol.name).unwrap() else {
            panic!("canonical name resolves");
        };
        let (_, rt) = traced(|d| refs_traced(&r, &first, d).unwrap());
        assert!(rt.weights.len() <= DIAG_REFS);
    }

    #[test]
    fn trace_is_deterministic() {
        let (fx, words) = big_fx();
        let query = format!("{} {}", words[3], words[11]);
        let run = || {
            let r = fx.cache.read().unwrap();
            let (_, s) = traced(|d| search_traced(&r, &query, 20, d).unwrap());
            let (_, m) = traced(|d| map_traced(&r, d).unwrap());
            (json(&s), json(&m))
        };
        let first = run();
        assert_eq!(first, run());
        assert!(first.0.contains("\"ranking\""));
    }
}
