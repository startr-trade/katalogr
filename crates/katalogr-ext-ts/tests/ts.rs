//! FEAT-006 tests. Each case under `fixtures/ts/<case>` is copied to a temporary directory,
//! indexed through the engine with only the TS extractor registered, and compared with the
//! case's `expected.yaml` (FEAT-023 format: mode, sorted symbols, sorted links).

use camino::Utf8PathBuf;
use katalogr_engine::registry::ExtensionRegistry;
use katalogr_engine::{Consistency, Engine, NewEvent, QueryOptions, Scope};
use katalogr_ext_ts::TsExtractor;
use katalogr_store::{Kindex, ProjectConfig, ProjectName, RepoConfig};
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
        .with_consistency(&options, |tx| katalogr_engine::canonical_graph(tx))
        .unwrap();
    assert!(!answer.stale, "queue drained");

    let mut g = Graph {
        symbols: BTreeSet::new(),
        links: BTreeSet::new(),
        artifacts: BTreeMap::new(),
        nodes: BTreeMap::new(),
    };
    for line in answer.value.lines() {
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
        assert!(!g.artifacts.contains_key("fx:notes.md"));
        assert!(g.artifacts.values().all(|a| a["extension"] == "ts"));
    }

    #[test]
    fn parse_error_tolerant() {
        let g = check("broken-syntax");
        let broken = &g.artifacts["fx:broken.ts"];
        let errors = fact(broken, "parse_errors")["List"].as_array().unwrap();
        assert!(!errors.is_empty(), "errors recorded with the artifact");
        let line = errors[0]["Map"]["line"]["Int"].as_i64().unwrap();
        assert_eq!(line, 3, "error located");
        assert!(errors[0]["Map"]["message"]["Str"].as_str().is_some());
        // An unrecoverable error (oxc returns an empty program) still records the error.
        let fatal = fact(&g.artifacts["fx:fatal.ts"], "parse_errors")["List"]
            .as_array()
            .unwrap()
            .len();
        assert!(fatal >= 1);
        assert!(g.symbols.iter().all(|s| !s.starts_with("fx:fatal.ts#")));
        assert!(fact(&g.artifacts["fx:ok.ts"], "parse_errors").is_null());
        assert!(
            g.symbols.contains("fx:ok.ts#fn:fine"),
            "other files keep indexing"
        );
        assert!(
            g.symbols.contains("fx:broken.ts#fn:before"),
            "recovered part kept"
        );
    }
}
