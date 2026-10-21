//! A small in-memory graph builder for the unit tests: collects artifacts, symbols, links and
//! contracts, then writes them to a temporary cache.

use camino::Utf8PathBuf;
use katalogr_model::{
    ArtifactName, Confidence, ContractName, ExportKey, Fact, Fragment, Id, LinkKind, Name, OutRef,
    Provenance, RelPath, Segment, Span, SymbolName,
};
use katalogr_store::{
    ArtifactRow, Cache, ContractRow, FanKey, Kindex, LinkEntry, LinkKey, LinkOrigin, LinkRow,
    ProjectConfig, ProjectName, ReadTx, RepoConfig, Restitch, StoreError, SymbolRow, WriteTx,
};
use std::collections::BTreeMap;
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

pub const SPAN: Span = Span {
    start_line: 1,
    start_col: 1,
    end_line: 1,
    end_col: 1,
};

#[derive(Default)]
pub struct Fx {
    artifacts: BTreeMap<Id, ArtifactRow>,
    symbols: BTreeMap<Id, SymbolRow>,
    links: BTreeMap<LinkKey, LinkRow>,
    contracts: BTreeMap<Id, ContractRow>,
    exports: BTreeMap<ExportKey, Vec<Id>>,
    fans: BTreeMap<FanKey, LinkRow>,
}

pub struct Built {
    _tmp: tempfile::TempDir,
    pub cache: Cache,
}

impl Built {
    pub fn read(&self) -> ReadTx<'_> {
        self.cache.read().unwrap()
    }
}

fn provenance(path: &RelPath) -> Provenance {
    Provenance {
        repo: "r".into(),
        path: path.clone(),
        revision: "ce013625030ba8dba906f756967f9e9ca394464a".into(),
    }
}

impl Fx {
    pub fn new() -> Fx {
        Fx::default()
    }

    /// An artifact `r:<path>`, with the `package` fact when given.
    pub fn file(&mut self, path: &str, package: Option<&str>) -> Id {
        let rel = RelPath::new(path).unwrap();
        let name = ArtifactName {
            repo: "r".into(),
            path: rel.clone(),
        };
        let id = name.id();
        let mut facts = BTreeMap::new();
        if let Some(p) = package {
            facts.insert("package".to_string(), Fact::Str(p.to_string()));
        }
        self.artifacts.entry(id).or_insert(ArtifactRow {
            provenance: provenance(&rel),
            name,
            extension: "test".into(),
            facts,
            symbols: Vec::new(),
            exports: Vec::new(),
            refs: Vec::new(),
        });
        id
    }

    /// Set a fact on an artifact.
    pub fn file_fact(&mut self, file: Id, key: &str, value: Fact) {
        self.artifacts
            .get_mut(&file)
            .unwrap()
            .facts
            .insert(key.into(), value);
    }

    /// A symbol of `kind` named `name` in the artifact `file` (created in no package if new).
    pub fn sym_at(&mut self, file: &str, kind: &str, name: &str, line: u32) -> Id {
        let aid = self.file(file, None);
        let artifact = self.artifacts[&aid].name.clone();
        let sname = SymbolName {
            artifact,
            segments: vec![Segment {
                kind: kind.into(),
                name: name.into(),
            }],
            ordinal: 1,
        };
        let id = sname.id();
        self.symbols.insert(
            id,
            SymbolRow {
                name: sname,
                artifact: aid,
                kind: kind.into(),
                span: Span {
                    start_line: line,
                    end_line: line,
                    ..SPAN
                },
                facts: BTreeMap::new(),
            },
        );
        let row = self.artifacts.get_mut(&aid).unwrap();
        row.symbols.push(id);
        row.symbols.sort();
        id
    }

    /// A `fn` symbol.
    pub fn sym(&mut self, file: &str, name: &str) -> Id {
        self.sym_at(file, "fn", name, 1)
    }

    pub fn sym_fact(&mut self, sym: Id, key: &str, value: Fact) {
        self.symbols
            .get_mut(&sym)
            .unwrap()
            .facts
            .insert(key.into(), value);
    }

    /// Append a rationale marker (`NOTE`, `WHY`, `RATIONALE`) at `line` to a symbol.
    pub fn note(&mut self, sym: Id, marker: &str, line: u32, text: &str) {
        let entry = Fact::Map(BTreeMap::from([
            ("marker".to_string(), Fact::Str(marker.into())),
            ("line".to_string(), Fact::Int(i64::from(line))),
            ("text".to_string(), Fact::Str(text.into())),
        ]));
        let facts = &mut self.symbols.get_mut(&sym).unwrap().facts;
        match facts
            .entry("rationale".to_string())
            .or_insert(Fact::List(Vec::new()))
        {
            Fact::List(l) => l.push(entry),
            _ => unreachable!(),
        }
    }

    /// Export `target` under `namespace:key`.
    pub fn export(&mut self, namespace: &str, key: &str, target: Id) {
        self.exports
            .entry(ExportKey {
                namespace: namespace.into(),
                key: key.into(),
            })
            .or_default()
            .push(target);
    }

    /// A string fact (`root`, for one).
    pub fn sym_str(&mut self, sym: Id, key: &str, value: &str) {
        self.sym_fact(sym, key, Fact::Str(value.into()));
    }

    pub fn link_with(
        &mut self,
        from: Id,
        kind: &str,
        to: Id,
        confidence: Confidence,
        fanout: Option<u32>,
    ) {
        let rel = RelPath::new("x.tx").unwrap();
        let entry = LinkEntry {
            origin: LinkOrigin::Intra,
            confidence,
            span: Some(SPAN),
            provenance: provenance(&rel),
            fanout,
            produced_by: None,
        };
        self.links
            .entry(LinkKey {
                from,
                kind: LinkKind(kind.into()),
                to,
            })
            .or_insert(LinkRow {
                entries: Vec::new(),
            })
            .entries
            .push(entry);
    }

    /// Record the reference `index` of the artifact holding `from`, so counts that read the
    /// artifact's references see it.
    fn note_ref(&mut self, from: Id, kind: &str, index: u32, candidates: Vec<ExportKey>) {
        let aid = self.symbols[&from].artifact;
        let refs = &mut self.artifacts.get_mut(&aid).unwrap().refs;
        let blank = OutRef {
            from,
            kind: LinkKind(kind.into()),
            span: Some(SPAN),
            candidates: Vec::new(),
            confidence: Confidence::Extracted,
            origin: None,
            binds: None,
        };
        while refs.len() <= index as usize {
            refs.push(blank.clone());
        }
        refs[index as usize] = OutRef {
            candidates,
            ..blank
        };
    }

    fn ref_entry(&self, index: u32, fanout: Option<u32>) -> LinkEntry {
        let rel = RelPath::new("x.tx").unwrap();
        LinkEntry {
            origin: LinkOrigin::Ref(index),
            confidence: if fanout.is_some_and(|n| n >= 2) {
                Confidence::Inferred
            } else {
                Confidence::Extracted
            },
            span: Some(SPAN),
            provenance: provenance(&rel),
            fanout,
            produced_by: None,
        }
    }

    /// A link with origin `Ref(index)`, confidence Inferred when `fanout` is Some(n >= 2).
    pub fn link_ref(&mut self, from: Id, kind: &str, to: Id, index: u32, fanout: Option<u32>) {
        let entry = self.ref_entry(index, fanout);
        self.links
            .entry(LinkKey {
                from,
                kind: LinkKind(kind.into()),
                to,
            })
            .or_insert(LinkRow {
                entries: Vec::new(),
            })
            .entries
            .push(entry);
    }

    /// The fan row `(from, kind, ns:key)` with one entry of origin `Ref(index)`, and the export
    /// `ns:key` to each of `targets` (two or more).
    pub fn fan(&mut self, from: Id, kind: &str, key: (&str, &str), index: u32, targets: &[Id]) {
        let k = ExportKey {
            namespace: key.0.into(),
            key: key.1.into(),
        };
        let mut entry = self.ref_entry(index, Some(targets.len() as u32));
        entry.fanout = None;
        self.note_ref(from, kind, index, vec![k.clone()]);
        self.fans.insert(
            FanKey {
                from,
                kind: LinkKind(kind.into()),
                key: k.clone(),
            },
            LinkRow {
                entries: vec![entry],
            },
        );
        let ex = self.exports.entry(k).or_default();
        for t in targets {
            if !ex.contains(t) {
                ex.push(*t);
            }
        }
    }

    /// What [`Fx::fan`] stands for in the layout before fan rows: one `Ref(index)` link per
    /// target with `fanout` equal to the target count, the same export and reference.
    pub fn fan_flat(
        &mut self,
        from: Id,
        kind: &str,
        key: (&str, &str),
        index: u32,
        targets: &[Id],
    ) {
        let k = ExportKey {
            namespace: key.0.into(),
            key: key.1.into(),
        };
        self.note_ref(from, kind, index, vec![k.clone()]);
        for t in targets {
            self.link_ref(from, kind, *t, index, Some(targets.len() as u32));
        }
        let ex = self.exports.entry(k).or_default();
        for t in targets {
            if !ex.contains(t) {
                ex.push(*t);
            }
        }
    }

    /// An extracted link.
    pub fn link(&mut self, from: Id, kind: &str, to: Id) {
        self.link_with(from, kind, to, Confidence::Extracted, None);
    }

    /// `from` provides (`provides == true`) or consumes the contract `kind:key`.
    pub fn bind(&mut self, from: Id, provides: bool, kind: &str, key: &str) -> Id {
        let name = ContractName {
            kind: kind.into(),
            key: key.into(),
        };
        let cid = name.id();
        let row = self.contracts.entry(cid).or_insert(ContractRow {
            name,
            providers: 0,
            consumers: 0,
            declarers: 0,
        });
        if provides {
            row.providers += 1;
        } else {
            row.consumers += 1;
        }
        self.link(from, if provides { "provides" } else { "consumes" }, cid);
        cid
    }

    pub fn build(self) -> Built {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().join("kx")).unwrap();
        let dir = Kindex::init(&root)
            .unwrap()
            .create_project(
                &ProjectName::new("t").unwrap(),
                ProjectConfig {
                    repos: vec![RepoConfig::new("r")],
                    ..Default::default()
                },
            )
            .unwrap();
        let cache =
            Cache::open_or_rebuild(&dir, &NoShards, Instant::now() + Duration::from_secs(60))
                .unwrap();
        let mut tx = cache.write().unwrap();
        for (id, row) in &self.artifacts {
            tx.put_artifact(*id, row).unwrap();
        }
        for (id, row) in &self.symbols {
            tx.put_symbol(*id, row).unwrap();
        }
        for (key, row) in &self.links {
            let mut row = row.clone();
            row.entries.sort_by_key(|e| e.origin);
            tx.put_link(key, &row).unwrap();
        }
        for (key, row) in &self.fans {
            tx.put_fan(key, row).unwrap();
        }
        for (id, row) in &self.contracts {
            tx.put_contract(*id, row).unwrap();
        }
        for (key, targets) in &self.exports {
            let mut targets = targets.clone();
            targets.sort();
            tx.put_exports(key, &targets).unwrap();
        }
        tx.commit().unwrap();
        Built { _tmp: tmp, cache }
    }
}
