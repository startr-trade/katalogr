//! FEAT-006 tests. Each case under `fixtures/ts/<case>` is copied to a temporary directory,
//! indexed through the engine with only the TS extractor registered, and compared with the
//! case's `expected.yaml` (FEAT-023 format: mode, sorted symbols, sorted links).

use camino::Utf8PathBuf;
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Consistency, Engine, NewEvent, QueryOptions, Scope};
use katalogr_ext_ts::TsExtractor;
use katalogr_store::{ErrorRow, Kindex, ProjectConfig, ProjectName, Reader, RepoConfig};
use serde::Deserialize;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectedLink {
    from: String,
    kind: String,
    to: String,
    confidence: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    mode: String,
    symbols: Vec<String>,
    links: Vec<ExpectedLink>,
}

struct Graph {
    symbols: BTreeSet<String>,
    links: BTreeSet<ExpectedLink>,
    artifacts: BTreeMap<String, Value>,
    nodes: BTreeMap<String, Value>,
    /// Index-error rows by repo-relative path (only paths that have rows).
    errors: BTreeMap<String, Vec<ErrorRow>>,
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let name = e.file_name();
        let target = to.join(&name);
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &target);
        } else if name != "expected.yaml" {
            std::fs::copy(e.path(), target).unwrap();
        }
    }
}

fn case_dir(case: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/ts")
        .join(case)
}

fn index(case: &str) -> Graph {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let work = root.join("work");
    copy_dir(&case_dir(case), work.as_std_path());
    let kx = root.join("kx");
    Kindex::init(&kx)
        .unwrap()
        .create_project(
            &ProjectName::new("p").unwrap(),
            ProjectConfig {
                repos: vec![RepoConfig::new("fx")],
                ..Default::default()
            },
        )
        .unwrap();
    let dir = Kindex::open(&kx)
        .unwrap()
        .project(&ProjectName::new("p").unwrap())
        .unwrap();
    let mut registry = ExtensionRegistry::new();
    registry.register(Box::new(TsExtractor));
    let roots = BTreeMap::from([("fx".to_string(), work)]);
    let engine = Engine::open(
        dir,
        registry,
        roots,
        Instant::now() + Duration::from_secs(10),
    )
    .unwrap();
    engine.inbox().append(&NewEvent::rescan("fx")).unwrap();
    let options = QueryOptions {
        consistency: Consistency::WaitAll,
        timeout: Duration::from_secs(60),
        scope: Scope::Project,
    };
    let answer = engine
        .with_consistency(&options, |tx| {
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

    let mut g = Graph {
        symbols: BTreeSet::new(),
        links: BTreeSet::new(),
        artifacts: BTreeMap::new(),
        nodes: BTreeMap::new(),
        errors: answer.value.1,
    };
    for line in answer.value.0.lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
        if v["link"] == Value::Bool(true) {
            g.links.insert(ExpectedLink {
                from: s("from"),
                kind: s("kind"),
                to: s("to"),
                confidence: s("confidence"),
            });
        } else if v["node"] == "symbol" {
            g.symbols.insert(s("name"));
            g.nodes.insert(s("name"), v);
        } else {
            g.artifacts.insert(s("name"), v);
        }
    }
    g
}

/// Compare a case with its `expected.yaml`.
fn check(case: &str) -> Graph {
    let g = index(case);
    let file = case_dir(case).join("expected.yaml");
    let exp: Expected = serde_yaml_ng::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let mut sorted = exp.symbols.clone();
    sorted.sort();
    assert_eq!(exp.symbols, sorted, "{case}: symbols must be sorted");
    let mut sorted = exp.links.clone();
    sorted.sort();
    assert_eq!(exp.links, sorted, "{case}: links must be sorted");

    let want_symbols: BTreeSet<_> = exp.symbols.into_iter().collect();
    let want_links: BTreeSet<_> = exp.links.into_iter().collect();
    let missing_symbols: Vec<_> = want_symbols.difference(&g.symbols).collect();
    let missing_links: Vec<_> = want_links.difference(&g.links).collect();
    assert!(
        missing_symbols.is_empty(),
        "{case}: missing symbols {missing_symbols:#?}"
    );
    assert!(
        missing_links.is_empty(),
        "{case}: missing links {missing_links:#?}"
    );
    if exp.mode == "exact" {
        let extra_symbols: Vec<_> = g.symbols.difference(&want_symbols).collect();
        let extra_links: Vec<_> = g.links.difference(&want_links).collect();
        assert!(
            extra_symbols.is_empty(),
            "{case}: unexpected symbols {extra_symbols:#?}"
        );
        assert!(
            extra_links.is_empty(),
            "{case}: unexpected links {extra_links:#?}"
        );
    } else {
        assert_eq!(exp.mode, "subset");
    }
    g
}

fn fact<'a>(node: &'a Value, key: &str) -> &'a Value {
    &node["facts"][key]
}

mod ts {
    use super::*;

    #[test]
    fn items_with_spans() {
        let g = check("items");
        let at = |n: &str| &g.nodes[&format!("fx:src/items.ts#{n}")];
        let span = |n: &str| {
            let s = &at(n)["span"];
            [
                s["start_line"].as_u64().unwrap(),
                s["start_col"].as_u64().unwrap(),
                s["end_line"].as_u64().unwrap(),
                s["end_col"].as_u64().unwrap(),
            ]
        };
        assert_eq!(span("interface:Shape"), [1, 1, 3, 1]);
        assert_eq!(span("type:Id"), [5, 1, 5, 33]);
        assert_eq!(span("enum:Color"), [7, 1, 10, 1]);
        assert_eq!(span("fn:plain"), [12, 1, 14, 1]);
        assert_eq!(span("class:Widget/method:render"), [38, 3, 41, 3]);
        assert_eq!(at("class:Widget")["kind"], "class");
        assert_eq!(at("class:Widget/method:render")["kind"], "method");

        let f = |n: &str, k: &str| fact(at(n), k).clone();
        assert_eq!(f("fn:plain", "exported")["Bool"], true);
        assert_eq!(f("fn:hidden", "exported")["Bool"], false);
        assert_eq!(f("fn:later", "async")["Bool"], true);
        assert_eq!(
            f("fn:plain", "signature")["Str"],
            "function plain(a: number): number"
        );
        assert_eq!(f("const:arrow", "async")["Bool"], true);
        assert_eq!(f("const:arrow", "signature")["Str"], "async (a: number)");
        assert_eq!(f("fn:default", "default")["Bool"], true);
        assert_eq!(f("class:Widget/method:make", "static")["Bool"], true);
        assert_eq!(f("class:Widget/method:size", "accessor")["Str"], "get");
        assert_eq!(f("class:Widget", "decorators")["List"][0]["Str"], "@sealed");
        assert_eq!(
            f("class:Widget/method:render", "decorators")["List"][0]["Str"],
            "@bound"
        );
    }

    #[test]
    fn import_export_resolution() {
        check("relative-imports");
        check("reexports");
        check("require");
    }

    #[test]
    fn esm_js_specifier_resolves_ts() {
        check("esm-js-specifiers");
    }

    #[test]
    fn commonjs_exports_recorded() {
        check("commonjs");
    }

    #[test]
    fn extends_any_name() {
        check("extends-base");
    }

    #[test]
    fn constructor_calls_recorded() {
        check("new-calls");
    }

    #[test]
    fn tsconfig_paths() {
        check("path-alias");
    }

    #[test]
    fn package_imports_linked() {
        check("package-imports");
    }

    #[test]
    fn call_sites_resolved_or_inferred() {
        let g = check("calls");
        let inferred: BTreeSet<_> = g
            .links
            .iter()
            .filter(|l| l.kind == "calls" && l.confidence == "Inferred")
            .map(|l| (l.from.as_str(), l.to.as_str()))
            .collect();
        assert!(
            inferred.len() >= 3,
            "member calls are inferred: {inferred:?}"
        );
        assert!(
            g.links
                .iter()
                .any(|l| l.kind == "calls" && l.confidence == "Extracted")
        );
    }

    #[test]
    fn file_kinds_covered() {
        let g = check("file-kinds");
        for ext in ["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs", "d.ts"] {
            let name = format!("fx:k.{ext}");
            assert!(g.artifacts.contains_key(&name), "{name} indexed");
        }
        assert!(!g.artifacts.contains_key("fx:notes.txt"));
        assert!(g.artifacts.values().all(|a| a["extension"] == "ts"));
    }

    #[test]
    fn parse_error_tolerant() {
        let g = check("broken-syntax");
        let rows = g.errors.get("broken.ts").expect("index_errors rows");
        assert!(!rows.is_empty(), "errors recorded for the file");
        assert!(
            rows.iter()
                .all(|r| r.span.is_some() && !r.message.is_empty())
        );
        assert_eq!(rows[0].span.unwrap().start_line, 3, "error located");
        // An unrecoverable error (oxc returns an empty program) still records the error.
        assert!(!g.errors.get("fatal.ts").expect("fatal rows").is_empty());
        assert!(g.symbols.iter().all(|s| !s.starts_with("fx:fatal.ts#")));
        assert!(!g.errors.contains_key("ok.ts"));
        assert!(
            g.symbols.contains("fx:ok.ts#fn:fine"),
            "other files keep indexing"
        );
        assert!(
            g.symbols.contains("fx:broken.ts#fn:before"),
            "recovered part kept"
        );
        assert!(
            g.artifacts
                .values()
                .all(|a| a["facts"].get("parse_errors").is_none())
        );
    }

    fn root(g: &Graph, name: &str) -> Option<String> {
        g.nodes[&format!("fx:{name}")]["facts"]["root"]["Str"]
            .as_str()
            .map(str::to_string)
    }

    #[test]
    fn entry_roots_marked() {
        let g = check("insight-roots");
        let r = |n: &str| root(&g, n);
        // Exported symbols are the API; private ones are not roots.
        assert_eq!(r("src/index.ts#fn:start").as_deref(), Some("api"));
        assert_eq!(r("src/util.ts#fn:used").as_deref(), Some("api"));
        assert_eq!(r("src/util.ts#const:arrow").as_deref(), Some("api"));
        assert_eq!(r("src/util.ts#fn:hidden"), None);
        assert_eq!(r("src/shapes.ts#interface:Shape"), None);
        // Test files: every symbol, whether exported or not; and the file itself.
        assert_eq!(r("src/util.test.ts#fn:check").as_deref(), Some("test"));
        assert_eq!(r("test/helpers.ts#fn:help").as_deref(), Some("test"));
        let art = |p: &str| {
            g.artifacts[&format!("fx:{p}")]["facts"]["root"]["Str"]
                .as_str()
                .map(str::to_string)
        };
        assert_eq!(art("src/util.test.ts").as_deref(), Some("test"));
        // The package entry file is an API root; other files are not roots.
        assert_eq!(art("src/index.ts").as_deref(), Some("api"));
        assert_eq!(art("src/util.ts"), None);
        // A method of a class that implements or extends something is reached through dispatch.
        assert_eq!(r("src/shapes.ts#class:Circle").as_deref(), Some("api"));
        assert_eq!(
            r("src/shapes.ts#class:Circle/method:area").as_deref(),
            Some("impl")
        );
        assert_eq!(
            r("src/shapes.ts#class:Child/method:run").as_deref(),
            Some("impl")
        );
        assert_eq!(r("src/shapes.ts#class:Base/method:run"), None);
        assert_eq!(r("src/shapes.ts#class:Child"), None);
    }

    #[test]
    fn rationale_markers_recorded() {
        let g = check("rationale");
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
        let node = |n: &str| &g.nodes[&format!("fx:src/notes.ts#{n}")]["facts"]["rationale"];
        // A comment before a symbol belongs to it, and so does one inside its body.
        assert_eq!(
            entries(node("fn:leading")),
            [
                t(
                    "WHY",
                    1,
                    "a leading comment explains why the function exists."
                ),
                t(
                    "NOTE",
                    3,
                    "a body comment belongs to the function that holds it."
                ),
            ]
        );
        // A block comment is one comment.
        assert_eq!(
            entries(node("fn:documented")),
            [t("NOTE", 8, "a block comment continues over two lines.")]
        );
        // Prose is no marker.
        assert!(node("fn:plain").is_null());
        // A comment with nothing after it belongs to the file.
        let file = &g.artifacts["fx:src/notes.ts"]["facts"]["rationale"];
        assert_eq!(
            entries(file),
            [t(
                "RATIONALE",
                16,
                "nothing follows, so this one belongs to the file."
            )]
        );
    }

    #[test]
    fn package_fact_recorded() {
        let g = check("package-imports");
        let package = |p: &str| {
            g.artifacts[&format!("fx:{p}")]["facts"]["package"]["Str"]
                .as_str()
                .map(str::to_string)
        };
        // The innermost project that holds the file names its package.
        assert_eq!(
            package("packages/util/src/main.ts").as_deref(),
            Some("@fx/util")
        );
        assert_eq!(package("app.ts").as_deref(), Some("fx-root"));
    }

    #[test]
    fn language_by_suffix() {
        use katalogr_model::{Extractor, RelPath};
        let lang = |p: &str| TsExtractor.language(&RelPath::new(p).unwrap());
        assert_eq!(lang("a.ts"), "typescript");
        assert_eq!(lang("a.tsx"), "typescript");
        assert_eq!(lang("a.js"), "javascript");
        assert_eq!(lang("a.mjs"), "javascript");
        assert_eq!(lang("package.json"), "json");
        assert_eq!(lang("sub/tsconfig.json"), "json");
    }
}
