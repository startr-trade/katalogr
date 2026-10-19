//! FEAT-005 tests. The manifest tests run directly against the extension; the others index a
//! copy of a `fixtures/rust/<case>` directory through the engine with only the Rust extractor
//! registered, and compare the canonical graph with the case's `expected.yaml`.

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Consistency, Engine, NewEvent, QueryOptions, Scope};
use katalogr_ext_rust::{RustExtractor, TargetKind, WorkspaceContext};
use katalogr_model::{ExtractInput, Extractor, Fact, Name, RelPath};
use katalogr_store::{ErrorRow, Kindex, ProjectConfig, ProjectName, Reader, RepoConfig};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// The repository name every fixture is indexed under.
const REPO: &str = "fx";

/// The canonical graph of one indexed fixture, parsed.
struct Graph {
    symbols: BTreeMap<String, Value>,
    /// `(from, kind, to, confidence)` with the confidence in lower case.
    links: BTreeSet<(String, String, String, String)>,
    artifacts: BTreeMap<String, Value>,
    text: String,
    /// Index-error rows by repo-relative path (only paths that have rows).
    errors: BTreeMap<String, Vec<ErrorRow>>,
}

impl Graph {
    fn sym(&self, name: &str) -> &Value {
        let full = format!("{REPO}:{name}");
        self.symbols
            .get(&full)
            .unwrap_or_else(|| panic!("no symbol {full}; have {:#?}", self.symbols.keys()))
    }

    fn has_link(&self, from: &str, kind: &str, to: &str, confidence: &str) -> bool {
        let full = |n: &str| format!("{REPO}:{n}");
        self.links
            .contains(&(full(from), kind.into(), full(to), confidence.into()))
    }

    fn links_of_kind(&self, kind: &str) -> Vec<&(String, String, String, String)> {
        self.links.iter().filter(|l| l.1 == kind).collect()
    }
}

fn span(v: &Value) -> (u64, u64, u64, u64) {
    let s = &v["span"];
    let n = |k: &str| s[k].as_u64().unwrap();
    (n("start_line"), n("start_col"), n("end_line"), n("end_col"))
}

/// A fact as plain JSON: strings, booleans and lists of strings.
fn fact(v: &Value, key: &str) -> Option<Value> {
    let f = v["facts"].get(key)?;
    let (_, inner) = f.as_object()?.iter().next()?;
    Some(match inner {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|i| i.as_object().unwrap().values().next().unwrap().clone())
                .collect(),
        ),
        other => other.clone(),
    })
}

fn fixtures_dir() -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust")
}

fn copy_dir(from: &Utf8Path, to: &Utf8Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name().into_string().unwrap();
        if e.file_type().unwrap().is_dir() {
            copy_dir(&from.join(&name), &to.join(&name));
        } else if name != "expected.yaml" {
            std::fs::copy(from.join(&name), to.join(&name)).unwrap();
        }
    }
}

/// Index a copy of the fixture case into a temporary kindex project, wait for everything, and
/// parse the canonical graph.
fn index_case(case: &str) -> Graph {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let work = root.join("work");
    copy_dir(&fixtures_dir().join(case), &work);
    let kx = root.join("kx");
    Kindex::init(&kx)
        .unwrap()
        .create_project(
            &ProjectName::new("p").unwrap(),
            ProjectConfig {
                repos: vec![RepoConfig::new(REPO)],
                ..Default::default()
            },
        )
        .unwrap();
    let dir = Kindex::open(&kx)
        .unwrap()
        .project(&ProjectName::new("p").unwrap())
        .unwrap();
    let mut reg = ExtensionRegistry::new();
    reg.register(Box::new(RustExtractor));
    let roots = BTreeMap::from([(REPO.to_string(), work)]);
    let engine = Engine::open(dir, reg, roots, Instant::now() + Duration::from_secs(10)).unwrap();
    engine.inbox().append(&NewEvent::rescan(REPO)).unwrap();
    let opts = QueryOptions {
        consistency: Consistency::WaitAll,
        timeout: Duration::from_secs(30),
        scope: Scope::Project,
    };
    let answer = engine
        .with_consistency(&opts, |tx| {
            let graph = katalogr_engine::canonical_graph(tx);
            let mut errors = BTreeMap::new();
            for ((repo, path), _) in tx.shard_hashes().unwrap() {
                let rows = tx.index_errors(&repo, &path).unwrap();
                if !rows.is_empty() {
                    errors.insert(path.as_str().to_string(), rows);
                }
            }
            (graph, errors)
        })
        .unwrap();
    assert!(!answer.stale, "queue drained");
    let (text, errors) = answer.value;
    let mut g = parse_graph(text);
    g.errors = errors;
    g
}

fn parse_graph(text: String) -> Graph {
    let mut g = Graph {
        errors: BTreeMap::new(),
        symbols: BTreeMap::new(),
        links: BTreeSet::new(),
        artifacts: BTreeMap::new(),
        text: text.clone(),
    };
    for line in text.lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        if v["node"] == "symbol" {
            g.symbols.insert(s("name"), v);
        } else if v["node"] == "artifact" {
            g.artifacts.insert(s("name"), v);
        } else if v["link"] == true {
            g.links.insert((
                s("from"),
                s("kind"),
                s("to"),
                s("confidence").to_lowercase(),
            ));
        }
    }
    g
}

/// Compare the graph with the case's `expected.yaml` (FEAT-023 format): sorted lists, and in
/// `exact` mode both directions.
fn assert_expected(case: &str, g: &Graph) {
    let path = fixtures_dir().join(case).join("expected.yaml");
    let yaml: Value = serde_yaml_ng::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let list = |k: &str| yaml[k].as_array().cloned().unwrap_or_default();
    let symbols: Vec<String> = list("symbols")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let mut sorted = symbols.clone();
    sorted.sort();
    assert_eq!(symbols, sorted, "{case}: symbols must be sorted");
    let links: Vec<(String, String, String, String)> = list("links")
        .iter()
        .map(|l| {
            let f = |k: &str| l[k].as_str().unwrap_or_default().to_string();
            (f("from"), f("kind"), f("to"), f("confidence"))
        })
        .collect();
    let mut sorted = links.clone();
    sorted.sort();
    assert_eq!(links, sorted, "{case}: links must be sorted");

    let want_syms: BTreeSet<String> = symbols.into_iter().collect();
    let have_syms: BTreeSet<String> = g.symbols.keys().cloned().collect();
    let want_links: BTreeSet<_> = links.into_iter().collect();
    let exact = yaml["mode"] == "exact";
    let mut diffs = Vec::new();
    for s in want_syms.difference(&have_syms) {
        diffs.push(format!("missing symbol {s}"));
    }
    for l in want_links.difference(&g.links) {
        diffs.push(format!("missing link {l:?}"));
    }
    if exact {
        for s in have_syms.difference(&want_syms) {
            diffs.push(format!("unexpected symbol {s}"));
        }
        for l in g.links.difference(&want_links) {
            diffs.push(format!("unexpected link {l:?}"));
        }
    }
    assert!(diffs.is_empty(), "{case}: {}", diffs.join("\n"));
}

fn file(path: &str, text: &str) -> (RelPath, Vec<u8>) {
    (RelPath::new(path).unwrap(), text.as_bytes().to_vec())
}

const ROOT: &str = r#"
[workspace]
members = ["crates/*"]

[workspace.dependencies]
serde = "1"
core-lib = { path = "crates/core-lib" }
renamed = { package = "real-name", version = "2" }
"#;

const CORE: &str = r#"
[package]
name = "core-lib"
version = "0.1.0"
edition = "2024"

[dependencies]
serde.workspace = true
"#;

const APP: &str = r#"
[package]
name = "app"
version = "0.2.0"
edition = "2024"

[lib]
name = "app_lib"

[[bin]]
name = "app-cli"
path = "src/cli.rs"

[dependencies]
core-lib = { workspace = true }
local = { path = "../core-lib" }
pkg = { package = "other-pkg", version = "3" }
renamed = { workspace = true }
tokio = "1"

[dev-dependencies]
serde = { workspace = true }
"#;

fn files() -> Vec<(RelPath, Vec<u8>)> {
    vec![
        file("crates/core-lib/Cargo.toml", CORE),
        file("Cargo.toml", ROOT),
        file("crates/app/Cargo.toml", APP),
        file("crates/app/src/lib.rs", "// ignored: not a manifest"),
    ]
}

mod rust {
    use super::*;

    #[test]
    fn workspace_crates_and_deps() {
        let bytes = RustExtractor.build_context(&files()).unwrap();
        // Deterministic: file order does not matter.
        let mut rev = files();
        rev.reverse();
        assert_eq!(bytes, RustExtractor.build_context(&rev).unwrap());

        let ctx = WorkspaceContext::from_bytes(&bytes).unwrap();
        let dirs: Vec<_> = ctx.crates.iter().map(|c| c.dir.as_str()).collect();
        assert_eq!(
            dirs,
            ["crates/app", "crates/core-lib"],
            "virtual root has no crate"
        );

        let app = ctx.crate_at("crates/app").unwrap();
        assert_eq!(
            (app.name.as_str(), app.lib_name.as_str()),
            ("app", "app_lib")
        );
        let targets: Vec<_> = app
            .targets
            .iter()
            .map(|t| (t.kind, t.name.as_str(), t.root.as_str()))
            .collect();
        assert_eq!(
            targets,
            [
                (TargetKind::Lib, "app_lib", "src/lib.rs"),
                (TargetKind::Bin, "app", "src/main.rs"),
                (TargetKind::Bin, "app-cli", "src/cli.rs"),
            ]
        );
        let deps: Vec<_> = app
            .deps
            .iter()
            .map(|d| (d.key.as_str(), d.package.as_str(), d.path.as_deref()))
            .collect();
        assert_eq!(
            deps,
            [
                // workspace = true: the root's path, relative to the root
                ("core_lib", "core-lib", Some("crates/core-lib")),
                // a plain path dependency, relative to the crate
                ("local", "local", Some("crates/core-lib")),
                // package = rename: the key is the name used in code
                ("pkg", "other-pkg", None),
                // workspace = true on a rename
                ("renamed", "real-name", None),
                // a dev dependency from the workspace, and a registry dependency
                ("serde", "serde", None),
                ("tokio", "tokio", None),
            ]
        );
        let core = ctx.crate_at("crates/core-lib").unwrap();
        assert_eq!(core.lib_name, "core_lib");
        assert_eq!(core.deps.len(), 1);
        assert_eq!(core.deps[0].key, "serde");
        // The context records only whether `src/lib.rs` and `src/main.rs` exist.
        assert!(app.has_lib && !app.has_main);
        assert!(!core.has_lib && !core.has_main);
        let mut more = files();
        more.push(file("crates/app/src/main.rs", "fn main() {}"));
        let ctx2 =
            WorkspaceContext::from_bytes(&RustExtractor.build_context(&more).unwrap()).unwrap();
        assert!(ctx2.crate_at("crates/app").unwrap().has_main);
        let lib = more
            .iter()
            .position(|(p, _)| p.as_str() == "crates/app/src/lib.rs")
            .unwrap();
        let before = RustExtractor.build_context(&more).unwrap();
        more[lib] = file("crates/app/src/lib.rs", "pub fn changed() {}");
        assert_eq!(
            before,
            RustExtractor.build_context(&more).unwrap(),
            "file content is not part of the context"
        );

        // The link half: indexed through the engine, `app` depends on `core-lib` by its path;
        // the registry dependency has no node in the repository, so no link.
        let g = index_case("workspace-two-crates");
        assert_expected("workspace-two-crates", &g);
        assert!(g.has_link(
            "crates/app/Cargo.toml#crate:app",
            "depends-on",
            "crates/core-lib/Cargo.toml#crate:core-lib",
            "extracted"
        ));
        assert_eq!(g.links_of_kind("depends-on").len(), 1);
        // The binary reaches the library through the dependency's path.
        assert!(g.has_link(
            "crates/app/src/main.rs#fn:main",
            "calls",
            "crates/core-lib/src/lib.rs#fn:greeting",
            "extracted"
        ));
        assert_eq!(
            fact(g.sym("crates/app/Cargo.toml#crate:app"), "x"),
            None,
            "facts of a crate symbol are on the artifact"
        );
        let manifest = &g.artifacts[&format!("{REPO}:crates/app/Cargo.toml")];
        assert_eq!(manifest["facts"]["name"]["Str"], "app");
        assert_eq!(manifest["facts"]["version"]["Str"], "0.2.0");
    }

    #[test]
    fn manifest_fragment() {
        let ctx = RustExtractor.build_context(&files()).unwrap();
        let path = RelPath::new("crates/app/Cargo.toml").unwrap();
        let f = RustExtractor
            .extract(&ExtractInput {
                repo: "r",
                path: &path,
                revision: "rev",
                content: APP.as_bytes(),
                context: &ctx,
            })
            .unwrap();
        assert_eq!(f.symbols.len(), 1);
        assert_eq!(
            f.symbols[0].name.canonical(),
            "r:crates/app/Cargo.toml#crate:app"
        );
        assert_eq!(f.exports[0].key.key, "crates/app");
        let cands: Vec<_> = f
            .refs
            .iter()
            .map(|r| {
                (
                    r.candidates[0].namespace.as_str(),
                    r.candidates[0].key.as_str(),
                )
            })
            .collect();
        assert_eq!(
            cands,
            [
                ("rs-crate-dir", "crates/core-lib"),
                ("rs-crate-dir", "crates/core-lib"),
                ("rs-package", "other-pkg"),
                ("rs-package", "real-name"),
                ("rs-package", "serde"),
                ("rs-package", "tokio"),
            ]
        );
    }

    #[test]
    fn items_with_spans() {
        let g = index_case("items");
        assert_expected("items", &g);
        let sp = |n: &str| span(g.sym(n));
        // 1-based, end inclusive; the span covers attributes but not leading comments.
        assert_eq!(sp("src/lib.rs#struct:Graph"), (14, 1, 16, 1));
        assert_eq!(sp("src/lib.rs#enum:Kind/variant:Pair"), (21, 5, 21, 34));
        assert_eq!(sp("src/lib.rs#impl:Graph/fn:add_node"), (37, 5, 39, 5));
        assert_eq!(sp("src/lib.rs#mod:shapes/struct:Point"), (4, 5, 7, 5));
        assert_eq!(sp("src/lib.rs#const:LIMIT"), (48, 1, 48, 28));
        assert_eq!(sp("src/lib.rs#fn:platform~2"), (76, 1, 77, 16));
        // Every symbol kind of the design, nested items under their owner.
        for (name, kind) in [
            ("struct:Graph", "struct"),
            ("enum:Kind", "enum"),
            ("enum:Kind/variant:Leaf", "variant"),
            ("union:Bits", "union"),
            ("trait:Visit", "trait"),
            ("trait:Visit/fn:visit", "fn"),
            ("impl:Visit%20for%20Graph", "impl"),
            ("impl:Visit%20for%20Graph/fn:visit", "fn"),
            ("const:LIMIT", "const"),
            ("static:GREETING", "static"),
            ("type:Id", "type"),
            ("macro:twice", "macro"),
            ("mod:shapes/fn:origin", "fn"),
        ] {
            assert_eq!(g.sym(&format!("src/lib.rs#{name}"))["kind"], kind, "{name}");
        }
        // Facts: visibility, async, unsafe and a one-line signature.
        let f = |n: &str, k: &str| fact(g.sym(&format!("src/lib.rs#{n}")), k);
        assert_eq!(f("fn:fetch", "vis"), Some("pub(crate)".into()));
        assert_eq!(f("fn:fetch", "async"), Some(true.into()));
        assert_eq!(f("fn:raw", "unsafe"), Some(true.into()));
        assert_eq!(f("fn:helper", "vis"), Some("private".into()));
        assert_eq!(f("fn:helper", "async"), None);
        assert_eq!(
            f("fn:helper", "signature"),
            Some("fn helper<T>(value: T) -> T where T: Copy,".into())
        );
        assert_eq!(
            f("impl:Graph/fn:add_node", "signature"),
            Some("pub fn add_node(&mut self, n: u32)".into())
        );
        // The file records its target; `contains` links the nested symbols.
        assert_eq!(
            g.artifacts[&format!("{REPO}:src/lib.rs")]["facts"]["target"]["Str"],
            "lib:items"
        );
        assert!(g.has_link(
            "src/lib.rs#trait:Visit",
            "contains",
            "src/lib.rs#trait:Visit/fn:name",
            "extracted"
        ));
    }

    #[test]
    fn impl_links_type_and_trait() {
        let g = index_case("impls");
        assert_expected("impls", &g);
        assert!(g.has_link(
            "src/lib.rs#impl:Shape%20for%20Circle",
            "implements",
            "src/traits.rs#trait:Shape",
            "extracted"
        ));
        assert!(g.has_link(
            "src/lib.rs#impl:Shape%20for%20Circle",
            "implements-for",
            "src/lib.rs#struct:Circle",
            "extracted"
        ));
        // Generic self type: the path without its arguments.
        assert!(g.has_link(
            "src/lib.rs#impl:Shape%20for%20Wrapper<T>",
            "implements-for",
            "src/lib.rs#struct:Wrapper",
            "extracted"
        ));
        // An inherent impl has no trait link.
        assert!(g.has_link(
            "src/lib.rs#impl:Circle",
            "implements-for",
            "src/lib.rs#struct:Circle",
            "extracted"
        ));
        // A full path to the trait, and an external trait that resolves to nothing.
        assert!(g.has_link(
            "src/lib.rs#impl:crate%3A%3Atraits%3A%3ANamed%20for%20Circle",
            "implements",
            "src/traits.rs#trait:Named",
            "extracted"
        ));
        assert_eq!(g.links_of_kind("implements").len(), 3);
        assert_eq!(g.links_of_kind("implements-for").len(), 5);
    }

    #[test]
    fn use_resolution() {
        let g = index_case("use-and-reexport");
        assert_expected("use-and-reexport", &g);
        // A `pub use` is a symbol of kind `use` that reexports the original.
        assert_eq!(g.sym("src/lib.rs#use:Circle")["kind"], "use");
        assert!(g.has_link(
            "src/lib.rs#use:Circle",
            "reexports",
            "src/shapes.rs#struct:Circle",
            "extracted"
        ));
        // `as` renames the exported name; the glob reexports the module.
        assert!(g.has_link(
            "src/lib.rs#use:assist",
            "reexports",
            "src/util/inner.rs#fn:helper",
            "extracted"
        ));
        assert!(g.has_link(
            "src/lib.rs#use:util%3A%3Ainner%3A%3A*",
            "reexports",
            "src/util/mod.rs#mod:inner",
            "extracted"
        ));
        // A registry dependency resolves to its package, which has no node here.
        assert!(g.links.iter().all(|l| !l.0.ends_with("#use:Serialize")));
        // crate:: with a self import, an alias through super::, and a glob import.
        let draw = "src/shapes.rs#fn:draw";
        for to in [
            "src/util/inner.rs#fn:helper",
            "src/util/inner.rs#fn:other",
            "src/util/mod.rs#impl:Marker/fn:mark",
        ] {
            assert!(g.has_link(draw, "calls", to, "extracted"), "{to}");
        }
        assert!(g.has_link(
            "src/extra.rs#fn:run",
            "calls",
            "src/util/inner.rs#fn:other",
            "extracted"
        ));
        // Modules are linked to their files.
        assert!(g.has_link(
            "src/util/mod.rs#mod:inner",
            "declares",
            "src/util/inner.rs",
            "extracted"
        ));
        assert!(g.has_link(
            "src/lib.rs#mod:util",
            "declares",
            "src/util/mod.rs",
            "extracted"
        ));
    }

    #[test]
    fn call_sites_resolved_or_inferred() {
        let g = index_case("calls");
        assert_expected("calls", &g);
        let run = "src/lib.rs#impl:Counter/fn:run";
        // Path calls are extracted: an import, `Self::`, `Type::`, a module path, `crate::`.
        for to in [
            "src/math.rs#fn:square",
            "src/math.rs#fn:cube",
            "src/lib.rs#impl:Counter/fn:new",
            "src/lib.rs#fn:local",
        ] {
            assert!(g.has_link(run, "calls", to, "extracted"), "{to}");
        }
        assert!(g.has_link(
            "src/lib.rs#fn:local",
            "calls",
            "src/math.rs#fn:square",
            "extracted"
        ));
        // Method calls and macro calls are matched by name only: inferred.
        assert!(g.has_link(run, "calls", "src/lib.rs#impl:Counter/fn:bump", "inferred"));
        assert!(g.has_link(run, "calls", "src/lib.rs#macro:trace", "inferred"));
        assert!(!g.has_link(run, "calls", "src/lib.rs#impl:Counter/fn:bump", "extracted"));
    }

    #[test]
    fn std_method_names_not_inferred() {
        let g = index_case("calls");
        let run = "src/lib.rs#impl:Counter/fn:run";
        // `[1].iter().map(..)` is a standard-library call, not a call of the workspace `map`.
        assert!(
            g.symbols
                .contains_key(&format!("{REPO}:src/lib.rs#impl:Counter/fn:map"))
        );
        assert!(
            !g.links_of_kind("calls")
                .iter()
                .any(|l| l.2.ends_with("impl:Counter/fn:map")),
            "no link to the workspace `map`"
        );
        // Names off the list are still matched by name.
        assert!(g.has_link(run, "calls", "src/lib.rs#impl:Counter/fn:bump", "inferred"));
    }

    #[test]
    fn attributes_recorded() {
        let g = index_case("attributes");
        assert_expected("attributes", &g);
        let f = |n: &str| fact(g.sym(&format!("src/lib.rs#{n}")), "attrs");
        assert_eq!(
            f("struct:Config"),
            Some(serde_json::json!(["derive(Debug, Clone)"]))
        );
        assert_eq!(
            f("enum:Mode"),
            Some(serde_json::json!(["derive(Debug)", "repr(u8)"]))
        );
        assert_eq!(
            f("enum:Mode/variant:Slow"),
            Some(serde_json::json!(["deprecated(note = \"slow\")"]))
        );
        assert_eq!(
            f("fn:compute"),
            Some(serde_json::json!(["inline", "must_use"]))
        );
        // `cfg` is recorded as a fact, never evaluated; custom attributes are kept as text.
        assert_eq!(
            f("fn:health"),
            Some(serde_json::json!([
                "cfg(feature = \"extra\")",
                "route(GET, \"/health\")"
            ]))
        );
        assert_eq!(f("mod:tests/fn:works"), Some(serde_json::json!(["test"])));
        // Inner attributes belong to the file.
        let file = &g.artifacts[&format!("{REPO}:src/lib.rs")];
        assert_eq!(file["facts"]["attrs"]["List"][0]["Str"], "allow(dead_code)");
        // No attributes: no fact.
        assert_eq!(fact(g.sym("src/lib.rs#fn:compute"), "async"), None);
    }

    #[test]
    fn parse_error_tolerant() {
        let g = index_case("broken-syntax");
        assert_expected("broken-syntax", &g);
        // Everything around the broken function is still indexed.
        for n in ["fn:before", "fn:unfinished", "struct:After", "fn:last"] {
            assert!(
                g.symbols.contains_key(&format!("{REPO}:src/lib.rs#{n}")),
                "{n}"
            );
        }
        assert_eq!(span(g.sym("src/lib.rs#fn:last")), (13, 1, 13, 16));
        assert!(g.text.contains("src/lib.rs"));
        // The syntax errors are recorded for the file, with spans.
        let rows = g.errors.get("src/lib.rs").expect("index_errors rows");
        assert!(!rows.is_empty());
        assert!(
            rows.iter()
                .all(|r| r.span.is_some() && !r.message.is_empty())
        );
        assert!(!g.errors.contains_key("Cargo.toml"));
    }

    fn root(g: &Graph, name: &str) -> Option<String> {
        g.sym(name)["facts"]["root"]["Str"]
            .as_str()
            .map(str::to_string)
    }

    #[test]
    fn entry_roots_marked() {
        let g = index_case("insight-roots");
        assert_expected("insight-roots", &g);
        let r = |n: &str| root(&g, &format!("src/lib.rs#{n}"));
        assert_eq!(r("fn:public_api").as_deref(), Some("api"));
        assert_eq!(r("fn:private_helper"), None);
        assert_eq!(r("fn:crate_visible"), None);
        assert_eq!(r("fn:a_test").as_deref(), Some("test"));
        assert_eq!(r("mod:tests/fn:helper_in_tests").as_deref(), Some("test"));
        // `#[no_mangle]` wins over `pub`; an `extern "C"` ABI is external too.
        assert_eq!(r("fn:exported_by_name").as_deref(), Some("external"));
        assert_eq!(r("fn:abi_only").as_deref(), Some("external"));
        // A trait impl method is reached through dispatch.
        let fmt = "impl:fmt%3A%3ADisplay%20for%20Thing/fn:fmt";
        assert_eq!(r(fmt).as_deref(), Some("impl"));
        assert_eq!(r("struct:Thing").as_deref(), Some("api"));
        // The binary's `main` is the program entry; its helper is not a root.
        assert_eq!(root(&g, "src/main.rs#fn:main").as_deref(), Some("main"));
        assert_eq!(root(&g, "src/main.rs#fn:helper"), None);
        // Integration tests are test code, and an example's `main` is still `main`.
        let a = |text: &str, path: &str| {
            let input_path = RelPath::new(path).unwrap();
            let ctx = RustExtractor
                .build_context(&[file("Cargo.toml", "[package]\nname = \"x\"\n")])
                .unwrap();
            RustExtractor
                .extract(&ExtractInput {
                    repo: REPO,
                    path: &input_path,
                    revision: "r",
                    content: text.as_bytes(),
                    context: &ctx,
                })
                .unwrap()
        };
        let frag = a("fn main() {}\nfn other() {}\n", "examples/demo.rs");
        let roots: Vec<_> = frag
            .symbols
            .iter()
            .map(|s| {
                (
                    s.name.segments[0].name.clone(),
                    s.facts.get("root").cloned(),
                )
            })
            .collect();
        assert_eq!(
            roots,
            [
                ("main".to_string(), Some(Fact::Str("main".into()))),
                ("other".to_string(), Some(Fact::Str("test".into()))),
            ]
        );
        let frag = a("fn it() {}\n", "tests/it.rs");
        assert_eq!(frag.symbols[0].facts["root"], Fact::Str("test".into()));
    }

    #[test]
    fn rationale_markers_recorded() {
        let g = index_case("rationale");
        assert_expected("rationale", &g);
        let entries = |v: &Value| -> Vec<(String, u64, String)> {
            v["List"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| {
                    let m = &e["Map"];
                    (
                        m["marker"]["Str"].as_str().unwrap().to_string(),
                        m["line"]["Int"].as_u64().unwrap(),
                        m["text"]["Str"].as_str().unwrap().to_string(),
                    )
                })
                .collect()
        };
        let t = |m: &str, l: u64, x: &str| (m.to_string(), l, x.to_string());
        // Leading and body comments belong to the function that holds or follows them.
        assert_eq!(
            entries(&g.sym("src/lib.rs#fn:leading")["facts"]["rationale"]),
            [
                t(
                    "WHY",
                    3,
                    "a leading comment explains why the function exists."
                ),
                t(
                    "NOTE",
                    5,
                    "a body comment belongs to the function that holds it."
                ),
            ]
        );
        // A doc comment's lines are one comment.
        assert_eq!(
            entries(&g.sym("src/lib.rs#fn:documented")["facts"]["rationale"]),
            [t("NOTE", 9, "a doc comment continues over two lines.")]
        );
        // Prose is no marker; the file-level comment goes to the file.
        assert!(
            g.sym("src/lib.rs#fn:plain")["facts"]
                .get("rationale")
                .is_none()
        );
        let file = &g.artifacts[&format!("{REPO}:src/lib.rs")];
        assert_eq!(
            entries(&file["facts"]["rationale"]),
            [t(
                "RATIONALE",
                1,
                "this file exists to show a file-level marker."
            )]
        );
    }

    #[test]
    fn package_fact_recorded() {
        let g = index_case("workspace-two-crates");
        assert_expected("workspace-two-crates", &g);
        let package = |path: &str| {
            g.artifacts[&format!("{REPO}:{path}")]["facts"]["package"]["Str"]
                .as_str()
                .map(str::to_string)
        };
        assert_eq!(package("crates/app/src/main.rs").as_deref(), Some("app"));
        assert_eq!(
            package("crates/core-lib/src/lib.rs").as_deref(),
            Some("core-lib")
        );
        // The root manifest is a virtual workspace: no package.
        assert_eq!(package("Cargo.toml"), None);
        let _ = fact;
    }
}
