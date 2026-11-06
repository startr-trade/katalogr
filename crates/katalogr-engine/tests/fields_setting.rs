//! The project setting `extract.code_fields` (FEAT-101 R-23).

mod support;

use camino::Utf8PathBuf;
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Engine, code_fields_enabled};
use katalogr_model::{
    ArtifactName, ArtifactNode, Confidence, ExtractError, ExtractInput, Extractor, Fact, Fragment,
    Link, LinkKind, Name, Provenance, RelPath, Segment, Span, SymbolName, SymbolNode,
};
use katalogr_store::{Kindex, ProjectConfig, ProjectName, Reader, RepoConfig};
use std::collections::BTreeMap;
use support::{drained_graph, full_index, open_engine_with};

const SPAN: Span = Span {
    start_line: 1,
    start_col: 1,
    end_line: 1,
    end_col: 1,
};

/// One type `Rec` with one field `x` per file; `code` declares code kinds, `shape` does not.
struct Fielder {
    id: &'static str,
    ext: &'static str,
    code: bool,
}

impl Extractor for Fielder {
    fn id(&self) -> &'static str {
        self.id
    }
    fn handles(&self, path: &RelPath) -> bool {
        path.as_str().ends_with(self.ext)
    }
    fn code_kinds(&self) -> &'static [&'static str] {
        if self.code { &["struct"] } else { &[] }
    }
    fn revision_salt(&self) -> &'static str {
        "fielder-1"
    }
    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        let artifact = ArtifactName {
            repo: input.repo.to_string(),
            path: input.path.clone(),
        };
        let prov = Provenance {
            repo: input.repo.to_string(),
            path: input.path.clone(),
            revision: input.revision.to_string(),
        };
        let seg = |k: &str, n: &str| Segment {
            kind: k.into(),
            name: n.into(),
        };
        let owner = SymbolName {
            artifact: artifact.clone(),
            segments: vec![seg("struct", "Rec")],
            ordinal: 1,
        };
        let field = SymbolName {
            artifact: artifact.clone(),
            segments: vec![seg("struct", "Rec"), seg("field", "x")],
            ordinal: 1,
        };
        let node = |name: SymbolName, kind: &str, facts: BTreeMap<String, Fact>| SymbolNode {
            name,
            kind: kind.into(),
            span: SPAN,
            facts,
        };
        let facts = BTreeMap::from([("field_type".to_string(), Fact::Str("string".into()))]);
        Ok(Fragment {
            provenance: prov.clone(),
            extension: self.id.to_string(),
            artifact: ArtifactNode {
                name: artifact,
                facts: BTreeMap::new(),
            },
            links: vec![Link {
                from: owner.id(),
                to: field.id(),
                kind: LinkKind("contains".into()),
                confidence: Confidence::Extracted,
                span: Some(SPAN),
                provenance: prov,
            }],
            symbols: vec![
                node(owner, "struct", BTreeMap::new()),
                node(field, "field", facts),
            ],
            refs: Vec::new(),
            exports: Vec::new(),
            bindings: Vec::new(),
        })
    }
}

struct Env {
    _tmp: tempfile::TempDir,
    kx: Utf8PathBuf,
    work: Utf8PathBuf,
}

fn env(setting: Option<bool>) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let work = base.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("a.cd"), "x\n").unwrap();
    std::fs::write(work.join("b.sc"), "x\n").unwrap();
    let kx = base.join("kx");
    let mut config = ProjectConfig {
        repos: vec![RepoConfig::new("r")],
        ..Default::default()
    };
    if let Some(v) = setting {
        config
            .settings
            .insert("extract.code_fields".into(), serde_yaml_ng::Value::Bool(v));
    }
    Kindex::init(&kx)
        .unwrap()
        .create_project(&ProjectName::new("p").unwrap(), config)
        .unwrap();
    Env {
        _tmp: tmp,
        kx,
        work,
    }
}

impl Env {
    fn open(&self) -> Engine {
        let mut reg = ExtensionRegistry::new();
        reg.register(Box::new(Fielder {
            id: "codey",
            ext: ".cd",
            code: true,
        }));
        reg.register(Box::new(Fielder {
            id: "shapey",
            ext: ".sc",
            code: false,
        }));
        open_engine_with(&self.kx, &self.work, reg)
    }

    /// Rewrite the project's setting on disk.
    fn set(&self, value: bool) {
        let e = self.open();
        let path = e.dir().project_root().join("config.yaml");
        let mut config = e.dir().config().clone();
        drop(e);
        config.settings.insert(
            "extract.code_fields".into(),
            serde_yaml_ng::Value::Bool(value),
        );
        std::fs::write(path, serde_yaml_ng::to_string(&config).unwrap()).unwrap();
    }
}

/// Canonical names of the field symbols per file path, from a drained cache.
fn fields(e: &Engine) -> BTreeMap<String, usize> {
    drained_graph(e);
    let tx = e.cache().read().unwrap();
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for (_, row) in tx.symbols().unwrap() {
        if row.facts.contains_key("field_type") {
            *out.entry(row.name.artifact.path.as_str().to_string())
                .or_default() += 1;
        }
    }
    out
}

fn contains_links_into_fields(e: &Engine) -> usize {
    let tx = e.cache().read().unwrap();
    let contains = LinkKind("contains".into());
    let mut n = 0;
    for (id, row) in tx.symbols().unwrap() {
        if row.facts.contains_key("field_type") {
            n += tx
                .links_in(id)
                .unwrap()
                .iter()
                .filter(|k| k.kind == contains)
                .count();
        }
    }
    n
}

fn revision(e: &Engine, path: &str) -> String {
    let id = ArtifactName::new("r", path).unwrap().id();
    e.cache()
        .read()
        .unwrap()
        .artifact(id)
        .unwrap()
        .unwrap()
        .provenance
        .revision
}

mod fields_setting {
    use super::*;

    #[test]
    fn off_records_no_fields() {
        // Absent: the default, recorded.
        assert!(code_fields_enabled(&Default::default()));
        let on = env(None);
        let e = on.open();
        full_index(&e);
        let got = fields(&e);
        assert_eq!(got.get("a.cd"), Some(&1), "{got:?}");
        assert_eq!(contains_links_into_fields(&e), 2);

        // Off: the code extension's field and the link into it are gone; the owner stays, and
        // an extension with no code kinds keeps its field.
        let off = env(Some(false));
        let e = off.open();
        full_index(&e);
        let got = fields(&e);
        assert_eq!(got.get("a.cd"), None, "{got:?}");
        assert_eq!(got.get("b.sc"), Some(&1), "{got:?}");
        assert_eq!(contains_links_into_fields(&e), 1);
        let tx = e.cache().read().unwrap();
        let owners = tx
            .symbols()
            .unwrap()
            .into_iter()
            .filter(|(_, r)| r.kind == "struct" && r.name.artifact.path.as_str() == "a.cd")
            .count();
        assert_eq!(owners, 1);
    }

    #[test]
    fn change_reextracts() {
        let en = env(Some(true));
        let e = en.open();
        full_index(&e);
        assert_eq!(fields(&e).get("a.cd"), Some(&1));
        let (code_on, schema_on) = (revision(&e, "a.cd"), revision(&e, "b.sc"));
        drop(e);

        // Turning it off re-extracts the code file only.
        en.set(false);
        let e = en.open();
        assert_eq!(fields(&e).get("a.cd"), None);
        assert_ne!(revision(&e, "a.cd"), code_on);
        assert_eq!(revision(&e, "b.sc"), schema_on);
        assert_eq!(fields(&e).get("b.sc"), Some(&1));
        drop(e);

        // And on again restores it, with the first revision.
        en.set(true);
        let e = en.open();
        assert_eq!(fields(&e).get("a.cd"), Some(&1));
        assert_eq!(revision(&e, "a.cd"), code_on);
    }
}
