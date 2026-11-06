//! Timing benchmark, run on demand:
//! `cargo test -p katalogr-engine --release --test bench_profile -- --ignored --nocapture`.

use camino::Utf8PathBuf;
use katalogr_engine::profiles::ProfileSet;
use katalogr_model::{
    ArtifactName, ArtifactNode, Fact, Fragment, Name, Provenance, RelPath, Segment, Span,
    SymbolName, SymbolNode,
};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const RULES: &str = "\
profile: scope-bench
version: 2
rules:
  - id: r0
    when: { extension: bench, kind: key, name: '^k\\d*[01]$', fact: pointer, matches: '^/s\\d+/k(\\d+)$' }
    emit:
      ref: { kind: maps-to, candidates: [ { ns: member, key: '${scope:meta/type}/${fact:value}' } ] }
      fact: { wire_field: '$1' }
  - id: r1
    when: { extension: bench, kind: key, name: '^k\\d*[23]$', fact: pointer, matches: '^/s\\d+/k(\\d+)$' }
    emit:
      ref: { kind: maps-to, candidates: [ { ns: member, key: '${scope:meta/type}/${fact:value}' } ] }
  - id: r2
    when: { extension: bench, kind: key, name: '^k\\d*[45]$', fact: pointer, matches: '^/s\\d+/k(\\d+)$' }
    emit:
      ref: { kind: maps-to, candidates: [ { ns: member, key: '${scope:meta/type}/${fact:value}' } ] }
  - id: r3
    when: { extension: bench, kind: key, name: '^k\\d*[67]$', fact: pointer, matches: '^/s\\d+/k(\\d+)$' }
    emit:
      ref: { kind: maps-to, candidates: [ { ns: member, key: '${scope:meta/type}/${fact:value}' } ] }
  - id: r4
    when: { extension: bench, kind: key, name: '^k\\d*[89]$', fact: pointer, matches: '^/s\\d+/k(\\d+)$' }
    emit:
      ref: { kind: maps-to, candidates: [ { ns: member, key: '${scope:meta/type}/${fact:value}' } ] }
";

/// 100 maps of 200 symbols each: the map, its `meta` and `meta/type` (which holds the value the
/// rules look up) and 197 plain keys: 20,000 symbols.
fn fragment() -> Fragment {
    let artifact = ArtifactName::new("r", "codecs/big.yaml").unwrap();
    let span = Span {
        start_line: 1,
        start_col: 1,
        end_line: 1,
        end_col: 1,
    };
    let mut symbols = Vec::new();
    let mut add = |segs: &[(&str, String)], facts: &[(&str, String)]| {
        symbols.push(SymbolNode {
            name: SymbolName {
                artifact: artifact.clone(),
                segments: segs
                    .iter()
                    .map(|(k, n)| Segment {
                        kind: (*k).to_string(),
                        name: n.clone(),
                    })
                    .collect(),
                ordinal: 1,
            },
            kind: segs.last().unwrap().0.to_string(),
            span,
            facts: facts
                .iter()
                .map(|(k, v)| ((*k).to_string(), Fact::Str(v.clone())))
                .collect::<BTreeMap<_, _>>(),
        });
    };
    for a in 0..100 {
        let s = format!("s{a}");
        add(&[("key", s.clone())], &[("pointer", format!("/{s}"))]);
        add(
            &[("key", s.clone()), ("key", "meta".into())],
            &[("pointer", format!("/{s}/meta"))],
        );
        add(
            &[
                ("key", s.clone()),
                ("key", "meta".into()),
                ("key", "type".into()),
            ],
            &[
                ("pointer", format!("/{s}/meta/type")),
                ("value", format!("T{a}")),
            ],
        );
        for b in 0..197 {
            add(
                &[("key", s.clone()), ("key", format!("k{b}"))],
                &[
                    ("pointer", format!("/{s}/k{b}")),
                    ("value", format!("v{b}")),
                ],
            );
        }
    }
    assert_eq!(symbols.len(), 20_000);
    Fragment {
        provenance: Provenance {
            repo: "r".into(),
            path: RelPath::new("codecs/big.yaml").unwrap(),
            revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
        },
        extension: "bench".into(),
        artifact: ArtifactNode {
            name: artifact,
            facts: BTreeMap::new(),
        },
        symbols,
        links: vec![],
        refs: vec![],
        exports: vec![],
        bindings: vec![],
    }
}

mod bench {
    use super::*;

    /// R-13 (FEAT-009). Five rules with `${scope:..}` and `${fact:..}` templates over a
    /// fragment of 20,000 symbols, including the index built for the scope lookups. Budget:
    /// 30 ms.
    #[test]
    #[ignore]
    fn profile_scope_20k_symbols() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let report = ProfileSet::load(&dir, &[("bench.yaml", RULES)]);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let set = report.set;
        let base = fragment();
        let mut best = Duration::MAX;
        let mut emitted = 0;
        for _ in 0..7 {
            let mut f = base.clone();
            let start = Instant::now();
            set.apply(&mut f);
            best = best.min(start.elapsed());
            emitted = f.refs.len();
        }
        // 197 keys per map, of which those named k0..k196 ending in 0..9: every key matches
        // exactly one rule.
        assert_eq!(emitted, 100 * 197);
        let s = base.symbols[3].name.canonical();
        assert!(s.contains("k0"));
        println!("bench::profile_scope_20k_symbols: {emitted} refs in {best:?}");
        println!(
            "katalogr-bench: profile_scope_20k_symbols {:.3} ms",
            best.as_secs_f64() * 1000.0
        );
        assert!(best < Duration::from_millis(105), "took {best:?}");
    }
}
