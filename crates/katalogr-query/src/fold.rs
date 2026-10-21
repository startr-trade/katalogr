//! Folding of very-high fan-out in `refs` and `callers` (FEAT-077, ADR-0028): hits that reach
//! their node only through an export key with `FOLD_FROM` or more targets become one summary
//! per key. A fold is a presentation, not a cap: nothing is left out of the answer.

use crate::result::{Fold, SymbolHit};
use katalogr_model::ExportKey;
use katalogr_store::{LinkKey, Reader, StoreError};
use std::collections::BTreeMap;

/// A key with at least this many targets is folded in `refs` and `callers`.
pub const FOLD_FROM: u32 = 50;
/// Folded hits named in a fold's `first`.
pub const FOLD_SHOWN: usize = 5;

/// An answer with its folds. `hits` keeps today's order without the folded hits.
#[derive(Debug, Clone, PartialEq)]
pub struct Folded<T> {
    pub hits: Vec<T>,
    pub folds: Vec<Fold>,
}

/// The export key a hit folds under, with the key's target count: the first fan source of the
/// hit's link whose key has `fold_from` or more targets, provided every entry of the link has a
/// fan-out of `fold_from` or more (a stored entry keeps the hit).
fn fold_key(
    tx: &impl Reader,
    link: &LinkKey,
    fold_from: u32,
) -> Result<Option<(ExportKey, u32)>, StoreError> {
    let Some(row) = tx.link(link)? else {
        return Ok(None);
    };
    if !row
        .entries
        .iter()
        .all(|e| e.fanout.is_some_and(|n| n >= fold_from))
    {
        return Ok(None);
    }
    Ok(tx
        .fan_sources(link)?
        .into_iter()
        .find(|(_, n)| *n >= fold_from))
}

fn key_text(k: &ExportKey) -> String {
    format!("{}:{}", k.namespace, k.key)
}

/// Fold `keyed` hits (each with the link it arrived by; None never folds). `group` names a
/// hit's group from the hit and its key; `fill` sets the answer-specific fields of a fold from
/// its group. Groups are ordered by their first member, and `at` counts the hits kept before it.
pub(crate) fn fold<T, G: Ord + Clone>(
    tx: &impl Reader,
    keyed: Vec<(T, Option<LinkKey>)>,
    fold_from: u32,
    group: impl Fn(&T, &ExportKey) -> G,
    first: impl Fn(&T) -> SymbolHit,
    fill: impl Fn(&G, &mut Fold),
) -> Result<Folded<T>, StoreError> {
    struct Acc<G> {
        group: G,
        key: ExportKey,
        targets: u32,
        hits: u64,
        first: Vec<SymbolHit>,
        at: u64,
    }
    let mut order: Vec<G> = Vec::new();
    let mut accs: BTreeMap<G, Acc<G>> = BTreeMap::new();
    let mut hits = Vec::new();
    for (hit, link) in keyed {
        let folded = match &link {
            Some(l) => fold_key(tx, l, fold_from)?,
            None => None,
        };
        let Some((key, targets)) = folded else {
            hits.push(hit);
            continue;
        };
        let g = group(&hit, &key);
        let at = hits.len() as u64;
        let acc = accs.entry(g.clone()).or_insert_with(|| {
            order.push(g.clone());
            Acc {
                group: g.clone(),
                key,
                targets,
                hits: 0,
                first: Vec::new(),
                at,
            }
        });
        acc.hits += 1;
        if acc.first.len() < FOLD_SHOWN {
            acc.first.push(first(&hit));
        }
    }
    let folds = order
        .iter()
        .map(|g| {
            let a = &accs[g];
            let mut f = Fold {
                key: key_text(&a.key),
                kind: String::new(),
                targets: u64::from(a.targets),
                hits: a.hits,
                first: a.first.clone(),
                at: a.at,
                via: Vec::new(),
                depth: None,
            };
            fill(&a.group, &mut f);
            f
        })
        .collect();
    Ok(Folded { hits, folds })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::hit;
    use crate::{Diagnostic, callers_folded, refs_folded_traced};
    use camino::Utf8PathBuf;
    use katalogr_model::{
        ArtifactName, Confidence, Fragment, Id, LinkKind, Name, Provenance, RelPath, Segment, Span,
        SymbolName,
    };
    use katalogr_store::{
        Cache, FanKey, Kindex, LinkEntry, LinkOrigin, LinkRow, ProjectConfig, ProjectName,
        RepoConfig, Restitch, SymbolRow, WriteTx,
    };
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

    struct G {
        _tmp: tempfile::TempDir,
        cache: Cache,
        artifact: ArtifactName,
    }

    impl G {
        fn name(&self, n: &str) -> SymbolName {
            SymbolName {
                artifact: self.artifact.clone(),
                segments: vec![Segment {
                    kind: "fn".into(),
                    name: n.into(),
                }],
                ordinal: 1,
            }
        }

        fn id(&self, n: &str) -> Id {
            self.name(n).id()
        }

        fn hit(&self, n: &str) -> SymbolHit {
            let tx = self.cache.read().unwrap();
            hit(&tx.symbol(self.id(n)).unwrap().unwrap())
        }
    }

    fn entry(origin: LinkOrigin, c: Confidence) -> LinkEntry {
        LinkEntry {
            origin,
            confidence: c,
            span: None,
            provenance: Provenance {
                repo: "r".into(),
                path: RelPath::new("f.tx").unwrap(),
                revision: "0".repeat(40),
            },
            fanout: None,
            produced_by: None,
        }
    }

    fn key(k: &str) -> ExportKey {
        ExportKey {
            namespace: "m".into(),
            key: k.into(),
        }
    }

    /// Keys `a` and `b` have 60 targets (`t0..` and `u0..`), `c` has 5 (`c0..`). `r0..r6` and
    /// `s0` call `a`, `s1` calls `b`, `q0` calls `c`, all by fan rows. `direct` calls `t0` and
    /// `r0` by stored links; `top` calls `r0`.
    fn graph() -> G {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("p").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let artifact = ArtifactName::new("r", "f.tx").unwrap();
        let g = G {
            _tmp: tmp,
            cache,
            artifact,
        };
        let mut tx = g.cache.write().unwrap();
        let mut names: Vec<String> = Vec::new();
        names.extend((0..60).map(|i| format!("t{i}")));
        names.extend((0..60).map(|i| format!("u{i}")));
        names.extend((0..5).map(|i| format!("c{i}")));
        names.extend((0..7).map(|i| format!("r{i}")));
        names.extend(["s0", "s1", "q0", "direct", "top"].map(String::from));
        for n in &names {
            tx.put_symbol(
                g.id(n),
                &SymbolRow {
                    name: g.name(n),
                    artifact: g.artifact.id(),
                    kind: "fn".into(),
                    span: SPAN,
                    facts: Default::default(),
                },
            )
            .unwrap();
        }
        let ids = |p: &str, n: usize| -> Vec<Id> {
            let mut v: Vec<Id> = (0..n).map(|i| g.id(&format!("{p}{i}"))).collect();
            v.sort();
            v
        };
        tx.put_exports(&key("a"), &ids("t", 60)).unwrap();
        tx.put_exports(&key("b"), &ids("u", 60)).unwrap();
        tx.put_exports(&key("c"), &ids("c", 5)).unwrap();
        let mut fan = |from: &str, k: &str| {
            tx.put_fan(
                &FanKey {
                    from: g.id(from),
                    kind: LinkKind("calls".into()),
                    key: key(k),
                },
                &LinkRow {
                    entries: vec![entry(LinkOrigin::Ref(0), Confidence::Inferred)],
                },
            )
            .unwrap();
        };
        for i in 0..7 {
            fan(&format!("r{i}"), "a");
        }
        fan("s0", "a");
        fan("s1", "b");
        fan("q0", "c");
        for (from, to) in [("direct", "t0"), ("top", "r0")] {
            tx.put_link(
                &LinkKey {
                    from: g.id(from),
                    kind: LinkKind("calls".into()),
                    to: g.id(to),
                },
                &LinkRow {
                    entries: vec![entry(LinkOrigin::Intra, Confidence::Extracted)],
                },
            )
            .unwrap();
        }
        tx.commit().unwrap();
        g
    }

    fn names(hits: &[SymbolHit]) -> Vec<&str> {
        hits.iter()
            .map(|h| h.name.rsplit(':').next().unwrap())
            .collect()
    }

    #[test]
    fn fold_groups_and_orders() {
        let g = graph();
        let tx = g.cache.read().unwrap();
        // refs: `direct` stays, the 8 fan referrers of key `a` fold into one fold.
        let f =
            refs_folded_traced(&tx, &g.hit("t0"), Some(FOLD_FROM), &mut Diagnostic::off()).unwrap();
        let kept: Vec<&str> = f
            .hits
            .iter()
            .map(|h| h.from.name.rsplit(':').next().unwrap())
            .collect();
        assert_eq!(kept, ["direct"]);
        assert_eq!(f.folds.len(), 1);
        let fold = &f.folds[0];
        assert_eq!(
            (
                fold.key.as_str(),
                fold.kind.as_str(),
                fold.targets,
                fold.hits,
                fold.at
            ),
            ("m:a", "calls", 60, 8, 1)
        );
        assert_eq!(names(&fold.first), ["r0", "r1", "r2", "r3", "r4"]);
        // No folding: the unfolded answer.
        let all = refs_folded_traced(&tx, &g.hit("t0"), None, &mut Diagnostic::off()).unwrap();
        assert_eq!(all.hits.len(), 9);
        assert!(all.folds.is_empty());
        // A key below the threshold is not folded, nor is a threshold above its targets.
        let small =
            refs_folded_traced(&tx, &g.hit("c0"), Some(FOLD_FROM), &mut Diagnostic::off()).unwrap();
        assert_eq!((small.hits.len(), small.folds.len()), (1, 0));
        let high = refs_folded_traced(&tx, &g.hit("t0"), Some(61), &mut Diagnostic::off()).unwrap();
        assert_eq!((high.hits.len(), high.folds.len()), (9, 0));
        // At threshold 60 the key folds; one more target would keep the hits.
        let at60 = refs_folded_traced(&tx, &g.hit("t0"), Some(60), &mut Diagnostic::off()).unwrap();
        assert_eq!(at60.folds.len(), 1);
        // The other key folds on its own target; a hit through `b` is one fold of one hit.
        let b =
            refs_folded_traced(&tx, &g.hit("u0"), Some(FOLD_FROM), &mut Diagnostic::off()).unwrap();
        assert!(b.hits.is_empty());
        assert_eq!((b.folds[0].hits, b.folds[0].key.as_str()), (1, "m:b"));

        // callers: depth 1 folds the 8 fan callers; `direct` stays; `top` is a depth-2 caller of
        // `r0`, reached through the folded `r0`.
        let c = callers_folded(&tx, &g.hit("t0"), 2, Some(FOLD_FROM)).unwrap();
        assert!(!c.deeper);
        assert_eq!(c.unfolded_len, 10);
        let kept: Vec<(&str, u32)> = c
            .folded
            .hits
            .iter()
            .map(|h| (h.caller.name.rsplit(':').next().unwrap(), h.depth))
            .collect();
        assert_eq!(kept, [("direct", 1), ("top", 2)]);
        assert_eq!(c.folded.folds.len(), 1);
        assert_eq!(c.folded.folds[0].depth, Some(1));
        assert_eq!(c.folded.folds[0].hits, 8);
        let probe = callers_folded(&tx, &g.hit("t0"), 1, None).unwrap();
        assert!(probe.deeper);
        assert_eq!(probe.unfolded_len, 9);
        assert_eq!(probe.folded.hits.len(), 9);
    }
}
