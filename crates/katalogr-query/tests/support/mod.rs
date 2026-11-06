//! A synthetic graph builder shared by the `doc_impact` tests and benchmark.
#![allow(dead_code)]

use camino::Utf8PathBuf;
use katalogr_model::{
    ArtifactName, Confidence, Id, LinkKind, Name, Provenance, RelPath, Segment, Span, SymbolName,
};
use katalogr_store::{
    ArtifactRow, Cache, Kindex, LinkEntry, LinkKey, LinkOrigin, LinkRow, ProjectConfig,
    ProjectName, RepoConfig, SymbolRow, WriteTx,
};
use std::time::{Duration, Instant};

pub fn open(tmp: &tempfile::TempDir, repos: &[&str]) -> Cache {
    let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
    let dir = Kindex::init(&root)
        .unwrap()
        .create_project(
            &ProjectName::new("docs").unwrap(),
            ProjectConfig {
                repos: repos.iter().map(|r| RepoConfig::new(r)).collect(),
                ..Default::default()
            },
        )
        .unwrap();
    Cache::open(&dir, Instant::now() + Duration::from_secs(60)).unwrap()
}

pub fn provenance(repo: &str, path: &RelPath) -> Provenance {
    Provenance {
        repo: repo.into(),
        path: path.clone(),
        revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
    }
}

pub fn span(a: u32, b: u32) -> Span {
    Span {
        start_line: a,
        start_col: 1,
        end_line: b,
        end_col: 1,
    }
}

/// Add an artifact with the given `(name, first line, last line)` symbols; returns the artifact
/// id and the symbol ids in order.
pub fn artifact(
    tx: &mut WriteTx,
    repo: &str,
    path: &str,
    syms: &[(&str, u32, u32)],
) -> (Id, Vec<Id>) {
    let rel = RelPath::new(path).unwrap();
    let name = ArtifactName {
        repo: repo.into(),
        path: rel.clone(),
    };
    let aid = name.id();
    let mut ids = Vec::new();
    for (n, a, b) in syms {
        let sn = SymbolName {
            artifact: name.clone(),
            segments: vec![Segment {
                kind: "fn".into(),
                name: (*n).into(),
            }],
            ordinal: 1,
        };
        let id = sn.id();
        tx.put_symbol(
            id,
            &SymbolRow {
                name: sn,
                artifact: aid,
                kind: "fn".into(),
                span: span(*a, *b),
                facts: Default::default(),
            },
        )
        .unwrap();
        ids.push(id);
    }
    let mut sorted = ids.clone();
    sorted.sort();
    tx.put_artifact(
        aid,
        &ArtifactRow {
            name,
            extension: "tx".into(),
            provenance: provenance(repo, &rel),
            facts: Default::default(),
            symbols: sorted,
            exports: Vec::new(),
            refs: Vec::new(),
        },
    )
    .unwrap();
    (aid, ids)
}

pub fn link_row(repo: &str, path: &str) -> LinkRow {
    LinkRow {
        entries: vec![LinkEntry {
            origin: LinkOrigin::Intra,
            confidence: Confidence::Extracted,
            span: Some(span(1, 1)),
            provenance: provenance(repo, &RelPath::new(path).unwrap()),
            fanout: None,
            produced_by: None,
        }],
    }
}

pub fn link(tx: &mut WriteTx, repo: &str, path: &str, from: Id, kind: &str, to: Id) {
    tx.put_link(
        &LinkKey {
            from,
            kind: LinkKind(kind.into()),
            to,
        },
        &link_row(repo, path),
    )
    .unwrap();
}

/// A link with the given confidence.
pub fn link_conf(
    tx: &mut WriteTx,
    repo: &str,
    path: &str,
    from: Id,
    kind: &str,
    to: Id,
    confidence: Confidence,
) {
    let mut row = link_row(repo, path);
    row.entries[0].confidence = confidence;
    tx.put_link(
        &LinkKey {
            from,
            kind: LinkKind(kind.into()),
            to,
        },
        &row,
    )
    .unwrap();
}

/// Add a contract node with the given role counts; returns its id.
pub fn contract(
    tx: &mut WriteTx,
    kind: &str,
    key: &str,
    providers: u32,
    consumers: u32,
    declarers: u32,
) -> Id {
    let name = katalogr_model::ContractName {
        kind: kind.into(),
        key: key.into(),
    };
    let id = Id::of(&name.canonical());
    tx.put_contract(
        id,
        &katalogr_store::ContractRow {
            name,
            providers,
            consumers,
            declarers,
        },
    )
    .unwrap();
    id
}

/// Give a symbol the neutral `test` fact with the given role.
pub fn mark_test(tx: &mut WriteTx, id: Id, role: &str) {
    use katalogr_store::Reader;
    let mut row = tx.symbol(id).unwrap().unwrap();
    row.facts.insert(
        katalogr_model::insight::FACT_TEST.to_string(),
        katalogr_model::Fact::Str(role.to_string()),
    );
    tx.put_symbol(id, &row).unwrap();
}

/// One symbol for [`artifact_with`]: the path segments as `(kind, name)`, the symbol kind and
/// its facts.
pub struct Sym {
    pub segs: Vec<(&'static str, &'static str)>,
    pub kind: &'static str,
    pub facts: Vec<(&'static str, katalogr_model::Fact)>,
}

/// A symbol with no facts.
pub fn sym(segs: &[(&'static str, &'static str)], kind: &'static str) -> Sym {
    Sym {
        segs: segs.to_vec(),
        kind,
        facts: Vec::new(),
    }
}

/// A `field` symbol with the neutral structure facts.
pub fn field(
    segs: &[(&'static str, &'static str)],
    ty: &'static str,
    required: bool,
    many: bool,
) -> Sym {
    use katalogr_model::Fact;
    Sym {
        segs: segs.to_vec(),
        kind: "field",
        facts: vec![
            ("field_type", Fact::Str(ty.into())),
            ("required", Fact::Bool(required)),
            ("many", Fact::Bool(many)),
        ],
    }
}

/// Add an artifact with symbols that have their own segments, kinds and facts; returns the
/// symbol ids in order.
pub fn artifact_with(tx: &mut WriteTx, repo: &str, path: &str, syms: Vec<Sym>) -> Vec<Id> {
    let rel = RelPath::new(path).unwrap();
    let name = ArtifactName {
        repo: repo.into(),
        path: rel.clone(),
    };
    let aid = name.id();
    let mut ids = Vec::new();
    for (i, s) in syms.into_iter().enumerate() {
        let sn = SymbolName {
            artifact: name.clone(),
            segments: s
                .segs
                .iter()
                .map(|(k, n)| Segment {
                    kind: (*k).into(),
                    name: (*n).into(),
                })
                .collect(),
            ordinal: 1,
        };
        let id = sn.id();
        let line = i as u32 + 1;
        tx.put_symbol(
            id,
            &SymbolRow {
                name: sn,
                artifact: aid,
                kind: s.kind.into(),
                span: span(line, line),
                facts: s
                    .facts
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v))
                    .collect(),
            },
        )
        .unwrap();
        ids.push(id);
    }
    let mut sorted = ids.clone();
    sorted.sort();
    tx.put_artifact(
        aid,
        &ArtifactRow {
            name,
            extension: "tx".into(),
            provenance: provenance(repo, &rel),
            facts: Default::default(),
            symbols: sorted,
            exports: Vec::new(),
            refs: Vec::new(),
        },
    )
    .unwrap();
    ids
}
