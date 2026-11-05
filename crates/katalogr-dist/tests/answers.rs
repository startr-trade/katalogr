//! Stored answers (FEAT-096): the reports a stored answer holds.

mod answers {
    use katalogr_analytics::{
        Community, CommunityReport, ContractKindCount, CoverageCounts, CoverageReport, CoverageRow,
        Cycle, CycleReport, DeadCandidate, DeadClass, DeadCodeReport, DeadConfidence, HubEntry,
        HubReport, KindCounts, Level, NodeHit, SuffixCount, UnclaimedRow,
    };
    use katalogr_model::{Confidence, Span};
    use katalogr_query::result::{Direction, MapEntry, PathStep, SymbolHit};
    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use std::collections::BTreeMap;

    /// Serialise, parse, serialise again: the bytes must not move.
    fn roundtrip<T: Serialize + DeserializeOwned>(what: &str, v: &T) {
        let first = serde_json::to_vec(v).unwrap();
        let back: T = serde_json::from_slice(&first).unwrap_or_else(|e| panic!("{what}: {e}"));
        let second = serde_json::to_vec(&back).unwrap();
        assert_eq!(
            String::from_utf8_lossy(&first),
            String::from_utf8_lossy(&second),
            "{what}"
        );
    }

    /// Floats that expose a lossy parser: short, tiny, and a full 17-digit value.
    const FLOATS: [f64; 4] = [0.1, 1e-7, 0.123_456_789_012_345_68, 2.0 / 3.0];

    fn sym(n: usize) -> SymbolHit {
        SymbolHit {
            name: format!("pkg::item{n}"),
            kind: "function".into(),
            path: format!("src/f{n}.tx"),
            span: Span {
                start_line: 1,
                start_col: 1,
                end_line: 3,
                end_col: 2,
            },
        }
    }

    fn node(level: Level, n: usize, with_path: bool) -> NodeHit {
        NodeHit {
            level,
            name: format!("n{n}"),
            kind: "function".into(),
            path: with_path.then(|| format!("src/n{n}.tx")),
            line: with_path.then_some(n as u32 + 1),
        }
    }

    #[test]
    fn report_roundtrip_byte_identical() {
        for (i, f) in FLOATS.iter().enumerate() {
            let present = i % 2 == 0;
            let hubs = HubReport {
                level: Level::File,
                nodes: 9,
                mean_degree: *f,
                items: vec![HubEntry {
                    node: node(Level::File, i, present),
                    in_degree: *f,
                    out_degree: f * 3.0,
                    degree: f * 4.0,
                    z: -f,
                    rank: f / 7.0,
                    members: 2,
                }],
            };
            roundtrip("hubs", &hubs);

            let communities = CommunityReport {
                level: Level::Symbol,
                seed: 7,
                resolution: *f,
                modularity: f / 3.0,
                communities_total: 1,
                unclustered: 0,
                note: present.then(|| "a note".to_string()),
                items: vec![Community {
                    id: 0,
                    size: 2,
                    label: node(Level::Symbol, i, present),
                    dir: present.then(|| "src".to_string()),
                    cohesion: *f,
                    internal_weight: f * 2.0,
                    boundary_weight: f / 5.0,
                    top: vec![node(Level::Symbol, 1, present)],
                    bridges: Vec::new(),
                    files: 1,
                }],
            };
            roundtrip("communities", &communities);

            let cycles = CycleReport {
                level: Level::Package,
                include_inferred: present,
                cycles_total: 1,
                cyclic_nodes: 2,
                skipped_contracts: 0,
                items: vec![Cycle {
                    id: 0,
                    size: 2,
                    members: vec![node(Level::Package, 1, false)],
                    members_total: 2,
                    witness: vec![PathStep {
                        from: "a".into(),
                        kind: "calls".into(),
                        direction: Direction::Forward,
                        to: "b".into(),
                        confidence: Confidence::Inferred,
                    }],
                    confidence: Confidence::Extracted,
                }],
            };
            roundtrip("cycles", &cycles);

            let mut roots = BTreeMap::new();
            roots.insert("main".to_string(), 1);
            let dead = DeadCodeReport {
                roots: 1,
                roots_by_reason: roots,
                candidates_total: 1,
                high: 1,
                medium: 0,
                low: 0,
                note: present.then(|| "note".to_string()),
                items: vec![DeadCandidate {
                    symbol: sym(i),
                    class: DeadClass::WeaklyReached,
                    confidence: DeadConfidence::Medium,
                    incoming: 1,
                    weak_incoming: 1,
                }],
            };
            roundtrip("dead-code", &dead);

            let map = vec![MapEntry {
                symbol: sym(i),
                in_degree: *f,
                rank: f / 11.0,
            }];
            roundtrip("map", &map);

            let counts = CoverageCounts {
                files_indexed: 3,
                refs_total: 4,
                parse_error_rate: present.then_some(*f),
                resolution_rate: Some(f / 2.0),
                inferred_rate: None,
                unresolved_rate: present.then_some(1.0 - f),
                ..CoverageCounts::default()
            };
            let row = CoverageRow {
                repo: present.then(|| "r".to_string()),
                extension: None,
                language: Some("l".into()),
                counts: counts.clone(),
            };
            let coverage = CoverageReport {
                totals: counts,
                by_repo: vec![row.clone()],
                by_extension: Vec::new(),
                by_language: Vec::new(),
                groups: vec![row],
                by_link_kind: vec![KindCounts {
                    kind: "calls".into(),
                    total: 2,
                    resolved: 1,
                    inferred: 1,
                    unresolved: 0,
                }],
                dangling_contracts_by_kind: vec![ContractKindCount {
                    kind: "k".into(),
                    dangling: 1,
                }],
                unclaimed: vec![UnclaimedRow {
                    repo: "r".into(),
                    files: 1,
                    suffixes: vec![SuffixCount {
                        suffix: ".x".into(),
                        files: 1,
                    }],
                }],
                roots_missing: Vec::new(),
                metrics: Vec::new(),
            };
            roundtrip("coverage", &coverage);
        }
    }
}
