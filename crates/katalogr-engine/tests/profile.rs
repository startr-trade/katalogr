//! Declarative profiles (FEAT-008 R-1 to R-4).

mod support;

use camino::Utf8PathBuf;
use katalogr_engine::profiles::{self, ProfileSet};
use katalogr_model::{ArtifactName, ContractName, Name};
use katalogr_store::Reader;
use support::{drained_graph, full_index, init_project, open_engine};

/// A project `p` with repo `r` rooted at `work`, using the `.tx` test extractor.
struct Env {
    _tmp: tempfile::TempDir,
    engine: katalogr_engine::Engine,
}

fn env(files: &[(&str, &str)]) -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let base = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let work = base.join("work");
    std::fs::create_dir_all(&work).unwrap();
    for (name, text) in files {
        std::fs::write(work.join(name), text).unwrap();
    }
    let kx = base.join("kx");
    init_project(&kx);
    let engine = open_engine(&kx, &work);
    Env { _tmp: tmp, engine }
}

impl Env {
    fn work(&self) -> camino::Utf8PathBuf {
        camino::Utf8PathBuf::from_path_buf(self._tmp.path().join("work")).unwrap()
    }

    fn write_profile(&self, file: &str, text: &str) {
        let dir = self.engine.dir().root().join(profiles::PROFILES_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), text).unwrap();
    }

    fn contract(&self, kind: &str, key: &str) -> Option<katalogr_store::ContractRow> {
        let id = ContractName {
            kind: kind.into(),
            key: key.into(),
        }
        .id();
        self.engine.cache().read().unwrap().contract(id).unwrap()
    }

    fn revision_of(&self, path: &str) -> String {
        let id = ArtifactName::new("r", path).unwrap().id();
        self.engine
            .cache()
            .read()
            .unwrap()
            .artifact(id)
            .unwrap()
            .unwrap()
            .provenance
            .revision
    }
}

const GENERIC: &str = "profile: generic\nversion: 1\nrules: []\n";

const BASE: &str = "\
profile: base
version: 1
rules:
  - id: mark-main
    when: { extension: test, kind: fn, name: '^main$' }
    emit:
      fact: { entry_point: true }
";

const EXAMPLE: &str = "\
profile: example
version: 1
rules:
  - id: tag-handlers
    when: { extension: test, kind: fn, name: '^handle_(.+)$' }
    emit:
      fact: { implementation: 'channel:$1' }
  - id: bind-channel
    when:
      extension: test
      kind: fn
      fact: implementation
      matches: '^channel:(.+)$'
    emit:
      bind: { contract: { kind: channel, key: '$1' }, role: consumes }
  - id: template-by-name
    when: { extension: test, kind: fn, name: '^handle_(.+)$' }
    emit:
      ref: { kind: renders, candidates: [ { ns: file, key: '$repo:tpl_$1.tx' } ] }
";

const APP: &str = "sym fn handle_orders\nsym fn main\n";
const TPL: &str = "sym fn body\n";
const API: &str = "sym fn serve\nbind serve provides channel:orders\n";

mod profile {
    use super::*;

    #[test]
    fn loaded_from_kindex_and_builtin() {
        let mut e = env(&[("app.tx", APP), ("tpl_orders.tx", TPL)]);
        e.write_profile("example.yaml", EXAMPLE);
        e.write_profile("notes.txt", "not a profile");

        // The loader reads built-ins first, then the project's `profiles/*.yaml` in name order.
        let r = ProfileSet::load(
            e.engine.dir().root(),
            &[("generic.yaml", GENERIC), ("base.yaml", BASE)],
        );
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert_eq!(r.set.names(), vec!["generic", "base", "example"]);
        assert_eq!(
            r.good.keys().collect::<Vec<_>>(),
            vec!["profiles/example.yaml"]
        );

        // The engine applies both sources: the built-in rule and the project's.
        e.engine
            .set_builtin_profiles(vec![("base.yaml".into(), BASE.into())]);
        let graph = full_index(&e.engine);
        assert!(
            graph.contains("entry_point"),
            "built-in rule applied:\n{graph}"
        );
        assert!(
            e.contract("channel", "orders").is_some(),
            "project rule applied"
        );
    }

    #[test]
    fn rules_applied_to_fragments() {
        let e = env(&[("app.tx", APP), ("tpl_orders.tx", TPL), ("api.tx", API)]);
        e.write_profile("example.yaml", EXAMPLE);
        let graph = full_index(&e.engine);

        // The first rule adds a fact, the second matches it in file order and adds a binding.
        let c = e.contract("channel", "orders").expect("contract node");
        assert_eq!((c.providers, c.consumers), (1, 1));
        // The third rule adds a reference, which resolves through the file export.
        assert!(
            graph.contains(
                "\"from\":\"r:app.tx#fn:handle_orders\",\"kind\":\"renders\",\"link\":true"
            ) && graph.contains("\"to\":\"r:tpl_orders.tx\""),
            "{graph}"
        );
        // Facts the rules add sit on the matched symbol only.
        let snap = e.engine.cache().read().unwrap();
        let handler = snap
            .symbols()
            .unwrap()
            .into_iter()
            .find(|(_, s)| s.name.canonical() == "r:app.tx#fn:handle_orders")
            .unwrap()
            .1;
        assert_eq!(
            handler.facts.get("implementation"),
            Some(&katalogr_model::Fact::Str("channel:orders".into()))
        );
        let main = snap
            .symbols()
            .unwrap()
            .into_iter()
            .find(|(_, s)| s.name.canonical() == "r:app.tx#fn:main")
            .unwrap()
            .1;
        assert!(main.facts.is_empty());
        // Everything a rule emitted is declared, not extracted.
        let links = snap
            .links_in(
                ContractName {
                    kind: "channel".into(),
                    key: "orders".into(),
                }
                .id(),
            )
            .unwrap();
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn change_triggers_rescan() {
        let e = env(&[
            ("app.tx", APP),
            ("tpl_orders.tx", TPL),
            ("other.tx", "sym fn o\n"),
        ]);
        full_index(&e.engine);
        assert!(e.contract("channel", "orders").is_none());
        let before = e.revision_of("app.tx");

        // A new profile reaches files already indexed, with no event for them.
        e.write_profile("example.yaml", EXAMPLE);
        drained_graph(&e.engine);
        assert!(e.contract("channel", "orders").is_some());
        let after = e.revision_of("app.tx");
        assert_ne!(before, after, "the shard depends on the profile");

        // A rule that names another extension cannot affect these files: they stay as they are.
        e.write_profile(
            "elsewhere.yaml",
            "profile: elsewhere\nversion: 1\nrules:\n  - id: r\n    when: { extension: nothing }\n    emit:\n      fact: { x: 1 }\n",
        );
        drained_graph(&e.engine);
        assert_eq!(e.revision_of("app.tx"), after);

        // Editing the profile re-applies it; removing the rule removes what it produced.
        e.write_profile(
            "example.yaml",
            &EXAMPLE.replace("role: consumes", "role: provides"),
        );
        drained_graph(&e.engine);
        let c = e.contract("channel", "orders").unwrap();
        assert_eq!((c.providers, c.consumers), (1, 0));
        e.write_profile("example.yaml", "profile: example\nversion: 1\nrules: []\n");
        drained_graph(&e.engine);
        assert!(e.contract("channel", "orders").is_none());
        assert_eq!(
            e.revision_of("app.tx"),
            before,
            "no profile, the original revision"
        );
    }

    /// Parse `text` and return the refusal as `(line, message)`.
    fn refused(text: &str) -> (usize, String) {
        let err = profiles::parse("profiles/bad.yaml", text).expect_err("must be refused");
        assert_eq!(err.file, "profiles/bad.yaml");
        (
            err.line.unwrap_or_else(|| panic!("no line: {err}")),
            err.message,
        )
    }

    #[test]
    fn invalid_refused_with_location() {
        // A regex that does not compile: the line of its value.
        let (line, why) = refused(
            "profile: p\nversion: 1\nrules:\n  - id: a\n    when:\n      name: '(open'\n    emit:\n      fact: { x: 1 }\n",
        );
        assert_eq!(line, 6, "{why}");
        assert!(why.contains("regex") && why.contains("(open"), "{why}");

        // An unknown key.
        let (line, why) = refused(
            "profile: p\nversion: 1\nrules:\n  - id: a\n    when: { kinds: fn }\n    emit:\n      fact: { x: 1 }\n",
        );
        assert_eq!(line, 5, "{why}");
        assert!(why.contains("kinds"), "{why}");

        // A capture out of range, reported at the rule.
        let (line, why) = refused(
            "profile: p\nversion: 1\nrules:\n  - id: a\n    when: { name: '^x$' }\n    emit:\n      fact: { x: '$1' }\n",
        );
        assert_eq!(line, 4, "{why}");
        assert!(why.contains("$1") && why.contains("out of range"), "{why}");

        // An empty emit.
        let (line, why) = refused(
            "profile: p\nversion: 1\nrules:\n  - id: a\n    when: { kind: fn }\n    emit: {}\n",
        );
        assert_eq!(line, 4, "{why}");
        assert!(why.contains("emit is empty"), "{why}");

        // A YAML syntax error and a wrong version.
        let (line, _) = refused("profile: p\nversion: 1\nrules: [\n  - id: a\n");
        assert!(line >= 3);
        let (line, why) = refused("profile: p\nversion: 3\nrules: []\n");
        assert_eq!(line, 2, "{why}");
        assert!(why.contains("version"), "{why}");
        let (line, why) = refused(
            "profile: p\nversion: 1\nrules:\n  - id: a\n    emit: { fact: { x: 1 } }\n  - id: a\n    emit: { fact: { x: 2 } }\n",
        );
        assert_eq!(line, 6, "{why}");

        // A bad file is refused with its location, and the last valid version stays in use.
        let e = env(&[("app.tx", APP), ("tpl_orders.tx", TPL)]);
        e.write_profile("example.yaml", EXAMPLE);
        full_index(&e.engine);
        assert!(e.contract("channel", "orders").is_some());
        assert!(profiles::stored_errors(&e.engine.cache().read().unwrap()).is_empty());

        e.write_profile(
            "example.yaml",
            &EXAMPLE.replace(
                "'^handle_(.+)$' }\n    emit:\n      fact",
                "'^handle_(.+' }\n    emit:\n      fact",
            ),
        );
        drained_graph(&e.engine);
        let errors = profiles::stored_errors(&e.engine.cache().read().unwrap());
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].file, "profiles/example.yaml");
        assert_eq!(errors[0].line, Some(5));
        assert!(
            e.contract("channel", "orders").is_some(),
            "the previous valid profile is still applied"
        );

        // Fixing the file clears the refusal.
        e.write_profile("example.yaml", EXAMPLE);
        drained_graph(&e.engine);
        assert!(profiles::stored_errors(&e.engine.cache().read().unwrap()).is_empty());
    }

    const ROUTE: &str = "\
profile: route
version: 2
select:
  - { extension: test-alt, path: 'notes.txt' }
  - { extension: test, path: 'notes.txt' }
";

    #[test]
    fn select_routes_file_to_extension() {
        let e = env(&[("app.tx", APP), ("notes.txt", "alpha one\nbeta two\n")]);
        let graph = full_index(&e.engine);
        assert!(!graph.contains("notes.txt"), "no route, no shard:\n{graph}");

        // An entry that names an unregistered extension routes nothing.
        e.write_profile(
            "ghost.yaml",
            "profile: ghost\nversion: 2\nselect:\n  - { extension: nothing-registered, path: 'notes.txt' }\n",
        );
        let graph = drained_graph(&e.engine);
        assert!(!graph.contains("notes.txt"), "{graph}");
        std::fs::remove_file(e.engine.dir().root().join("profiles/ghost.yaml")).unwrap();

        // The first entry that matches wins. A profile change reaches files already seen.
        e.write_profile("route.yaml", ROUTE);
        let graph = drained_graph(&e.engine);
        assert!(
            graph.contains("r:notes.txt#alt:alpha") && graph.contains("r:notes.txt#alt:beta"),
            "{graph}"
        );
        assert!(graph.contains("r:app.tx#fn:handle_orders"), "{graph}");

        // The router sees the entries.
        let r = ProfileSet::load(e.engine.dir().root(), &[]);
        assert_eq!(
            r.set
                .route(&katalogr_model::RelPath::new("notes.txt").unwrap()),
            Some("test-alt")
        );
        assert_eq!(
            r.set
                .route(&katalogr_model::RelPath::new("other.txt").unwrap()),
            None
        );
        assert_eq!(r.set.selects().count(), 2);

        // A file event for a routed path is processed with the routed extension.
        std::fs::write(e.work().join("notes.txt"), "gamma three\n").unwrap();
        e.engine
            .inbox()
            .append(&support::file_event("notes.txt"))
            .unwrap();
        let graph = drained_graph(&e.engine);
        assert!(
            graph.contains("r:notes.txt#alt:gamma") && !graph.contains("r:notes.txt#alt:alpha"),
            "{graph}"
        );
    }

    #[test]
    fn deselect_removes_shard() {
        let e = env(&[("app.tx", APP), ("notes.txt", "alpha one\n")]);
        e.write_profile("route.yaml", ROUTE);
        let graph = full_index(&e.engine);
        assert!(graph.contains("r:notes.txt#alt:alpha"), "{graph}");
        let shard = e
            .engine
            .dir()
            .shard_path("r", &katalogr_model::RelPath::new("notes.txt").unwrap());
        assert!(shard.exists());

        // Removing the entry leaves no extractor for the path: its shard and symbols go.
        e.write_profile("route.yaml", "profile: route\nversion: 2\nrules: []\n");
        let graph = drained_graph(&e.engine);
        assert!(!graph.contains("notes.txt"), "{graph}");
        assert!(!shard.exists(), "the stale shard file is removed");
        assert!(graph.contains("r:app.tx#fn:handle_orders"), "{graph}");
    }

    const SCOPES: &str = "sym doc d\nsub d key schema\nsub d/schema key type\nfact d/schema/type value Order\nsub d key fields\nsub d/fields key id\nfact d/fields/id value orderId\nsub d/fields key sku\nfact d/fields/sku value sku\nsub d/fields key odd\nfact d/fields/odd shape round\n";

    const SCOPE_RULES: &str = "\
profile: scopes
version: 2
rules:
  - id: doc-provides
    when: { extension: test, kind: doc }
    emit:
      bind: { contract: { kind: codec, key: '${scope:schema/type}' }, role: provides }
  - id: id-maps
    when: { extension: test, kind: key, name: '^id$' }
    emit:
      ref: { kind: maps-to, candidates: [ { ns: member, key: '${scope:schema/type}/${fact:value}' } ] }
      fact: { mapped: '${fact:value}', price: '$$5' }
  - id: sku-no-fact
    when: { extension: test, kind: key, name: '^sku$' }
    emit:
      ref: { kind: maps-to, candidates: [ { ns: member, key: '${fact:nothing}' } ] }
  - id: sku-no-scope
    when: { extension: test, kind: key, name: '^sku$' }
    emit:
      bind: { contract: { kind: codec, key: '${scope:nope/there}' }, role: consumes }
  - id: odd-not-string
    when: { extension: test, kind: key, name: '^odd$' }
    emit:
      fact: { shape_copy: '${fact:shape}', missing: '${scope:fields}' }
";

    #[test]
    fn fact_and_scope_templates() {
        let e = env(&[("m.tx", SCOPES)]);
        e.write_profile("scopes.yaml", SCOPE_RULES);
        full_index(&e.engine);

        // The nearest enclosing path wins: from `d` the scope is its own descendant.
        let c = e
            .contract("codec", "Order")
            .expect("bound through ${scope:..}");
        assert_eq!((c.providers, c.consumers), (1, 0));
        // `sku` found no value for either template: its rules did nothing, not even partly.
        assert!(e.contract("codec", "nope").is_none());

        let snap = e.engine.cache().read().unwrap();
        let refs: Vec<_> = snap.unresolved_refs(None).unwrap();
        assert_eq!(refs.len(), 1, "{refs:?}");
        let (_, r) = &refs[0];
        assert_eq!(r.kind.0, "maps-to");
        assert_eq!(r.candidates[0].namespace, "member");
        assert_eq!(r.candidates[0].key, "Order/orderId");

        let sym = |name: &str| {
            snap.symbols()
                .unwrap()
                .into_iter()
                .find(|(_, s)| s.name.canonical() == format!("r:m.tx#{name}"))
                .unwrap()
                .1
        };
        let id = sym("doc:d/key:fields/key:id");
        assert_eq!(
            id.facts.get("mapped"),
            Some(&katalogr_model::Fact::Str("orderId".into()))
        );
        assert_eq!(
            id.facts.get("price"),
            Some(&katalogr_model::Fact::Str("$5".into()))
        );
        // `${fact:shape}` is a string fact; `${scope:fields}` has no `value`: no partial emit.
        let odd = sym("doc:d/key:fields/key:odd");
        assert!(!odd.facts.contains_key("shape_copy") && !odd.facts.contains_key("missing"));
    }

    #[test]
    fn v2_syntax_refused_in_v1() {
        let (line, why) = refused(
            "profile: p\nversion: 1\nselect:\n  - { extension: test, path: '*.x' }\nrules: []\n",
        );
        assert_eq!(line, 3, "{why}");
        assert!(
            why.contains("select") && why.contains("version: 2"),
            "{why}"
        );

        let rule = |t: &str| {
            format!(
                "profile: p\nversion: 1\nrules:\n  - id: a\n    emit:\n      fact: {{ x: '{t}' }}\n"
            )
        };
        let (line, why) = refused(&rule("${fact:y}"));
        assert_eq!(line, 6, "{why}");
        assert!(
            why.contains("${fact:}") && why.contains("version: 2"),
            "{why}"
        );
        let (line, why) = refused(&rule("${scope:a/b}"));
        assert_eq!(line, 6, "{why}");
        assert!(
            why.contains("${scope:}") && why.contains("version: 2"),
            "{why}"
        );

        // The same text is fine in version 2; an unknown variable and an unclosed one are not.
        assert!(
            profiles::parse(
                "p.yaml",
                &rule("${fact:y}").replace("version: 1", "version: 2")
            )
            .is_ok()
        );
        let (_, why) = refused(&rule("${kind:y}").replace("version: 1", "version: 2"));
        assert!(why.contains("unknown template variable"), "{why}");
        let (_, why) = refused(&rule("${fact:y").replace("version: 1", "version: 2"));
        assert!(why.contains("not closed"), "{why}");
        // Version 1 files without the new syntax are unchanged.
        assert!(profiles::parse("p.yaml", BASE).is_ok());
        assert!(profiles::parse("p.yaml", EXAMPLE).is_ok());
    }

    // ---- FEAT-014: per-element rules, normalisers, holders, overrides

    use katalogr_model::{Binding, Confidence, ExtractInput, Extractor, Fragment, RelPath};
    use std::collections::BTreeMap;

    /// Load `profile` as the built-in `p.yaml` with the given normalisers.
    fn load_set(
        profile: &str,
        norms: &[&'static dyn katalogr_model::KeyNormaliser],
    ) -> profiles::LoadReport {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        profiles::ProfileSet::load_full(&root, &[("p.yaml", profile)], &BTreeMap::new(), norms)
    }

    fn fragment(path: &str, text: &str) -> Fragment {
        let path = RelPath::new(path).unwrap();
        support::TestExtractor
            .extract(&ExtractInput {
                repo: "r",
                path: &path,
                revision: "ce013625030ba8dba906f756967f9e9ca394464a",
                content: text.as_bytes(),
                context: &[],
            })
            .unwrap()
    }

    fn sym_id(f: &Fragment, name: &str) -> katalogr_model::Id {
        f.symbols
            .iter()
            .find(|s| s.name.segments.last().unwrap().name == name)
            .unwrap_or_else(|| panic!("no symbol {name}"))
            .name
            .id()
    }

    fn contract_keys(f: &Fragment) -> Vec<String> {
        let mut v: Vec<String> = f.bindings.iter().map(|b| b.contract.key.clone()).collect();
        v.sort();
        v
    }

    const EACH_ROUTE: &str = "\
profile: p
version: 2
rules:
  - id: route
    when: { extension: test, on: all, each: str_calls, matches: '^r\\.(get|post)\\(\"([^\"]*)\"' }
    emit: { bind: { contract: { kind: route, key: '$1 $2' }, role: provides } }
";

    #[test]
    fn each_rule_binds_per_element() {
        let e = env(&[(
            "a.tx",
            "sym fn reg\nstrcall reg r.get(\"/a\")\nstrcall reg r.post(\"/b\")\nstrcall reg log(\"x\")\n",
        )]);
        e.write_profile("route.yaml", EACH_ROUTE);
        full_index(&e.engine);
        assert!(e.contract("route", "get /a").is_some());
        assert!(e.contract("route", "post /b").is_some());
        assert!(e.contract("route", "log /x").is_none());
        // Each binding carries its element's span.
        let reg = fragment("a.tx", "sym fn reg\n").symbols[0].name.id();
        let snap = e.engine.cache().read().unwrap();
        let span_of = |key: &str| {
            let c = ContractName {
                kind: "route".into(),
                key: key.into(),
            }
            .id();
            snap.link(&katalogr_store::LinkKey {
                from: reg,
                kind: katalogr_model::LinkKind("provides".into()),
                to: c,
            })
            .unwrap()
            .unwrap()
            .entries[0]
                .span
                .unwrap()
        };
        assert_eq!(span_of("get /a").start_line, 2);
        assert_eq!(span_of("post /b").start_line, 3);
    }

    #[test]
    fn artifact_target_rules() {
        let set = |on: &str| {
            let r = load_set(
                &format!(
                    "profile: p\nversion: 2\nrules:\n  - id: route\n    when: {{ extension: test, on: {on}, each: str_calls, matches: '^r\\.get\\(\"([^\"]*)\"' }}\n    emit: {{ bind: {{ contract: {{ kind: route, key: '$1' }}, role: provides }} }}\n"
                ),
                &[],
            );
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            r.set
        };
        let text = "sym fn reg\nstrcall reg r.get(\"/s\")\nstrcall @ r.get(\"/top\")\n";
        // Symbols only: the artifact's call is not seen.
        let mut f = fragment("a.tx", text);
        set("symbols").apply(&mut f);
        assert_eq!(contract_keys(&f), vec!["/s"]);
        // Artifact only: bound by the artifact itself.
        let mut f = fragment("a.tx", text);
        set("artifact").apply(&mut f);
        assert_eq!(contract_keys(&f), vec!["/top"]);
        assert_eq!(f.bindings[0].symbol, f.artifact.name.id());
        // All: both.
        let mut f = fragment("a.tx", text);
        set("all").apply(&mut f);
        assert_eq!(contract_keys(&f), vec!["/s", "/top"]);
    }

    const UPPER_RULE: &str = "\
profile: p
version: 2
rules:
  - id: shout
    when: { extension: test, on: all, each: str_calls, matches: '^k\\(([^)]*)\\)' }
    emit: { bind: { contract: { kind: route, key: '$1' }, role: provides, normalise: upper } }
";

    #[test]
    fn normaliser_applied() {
        let r = load_set(UPPER_RULE, &[&support::UPPER]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let mut f = fragment(
            "a.tx",
            "sym fn reg\nstrcall reg k(get /a)\nstrcall reg k()\n",
        );
        r.set.apply(&mut f);
        // The key is upper-cased; the empty key produced no binding.
        assert_eq!(contract_keys(&f), vec!["GET /A"]);
        // A normaliser version change alters the hash of the extension it applies to.
        let v2 = load_set(UPPER_RULE, &[&support::UPPER_V2]);
        assert_ne!(r.set.hash_for("test"), v2.set.hash_for("test"));
        assert_eq!(
            r.set.hash_for("test"),
            load_set(UPPER_RULE, &[&support::UPPER])
                .set
                .hash_for("test")
        );
    }

    #[test]
    fn unknown_normaliser_refused_with_location() {
        let r = load_set(UPPER_RULE, &[]);
        assert_eq!(r.errors.len(), 1, "{:?}", r.errors);
        let err = &r.errors[0];
        assert_eq!(err.line, Some(6), "{err}");
        assert!(
            err.message.contains("unknown normaliser \"upper\""),
            "{err}"
        );
        assert!(r.set.is_empty());
    }

    #[test]
    fn project_profile_replaces_builtin() {
        let dir = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).unwrap();
        std::fs::create_dir_all(root.join("profiles")).unwrap();
        std::fs::write(
            root.join("profiles/mine.yaml"),
            "profile: base\nversion: 1\nrules:\n  - id: other\n    when: { kind: fn }\n    emit:\n      fact: { x: 1 }\n",
        )
        .unwrap();
        let r = profiles::ProfileSet::load_full(
            &root,
            &[("generic.yaml", GENERIC), ("base.yaml", BASE)],
            &BTreeMap::new(),
            &[],
        );
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        // The built-in `base` is gone, the project's file stands in its place.
        assert_eq!(r.set.names(), vec!["generic", "base"]);
        let rules: Vec<_> = r.set.rules().map(|(p, r)| (p, r.id.as_str())).collect();
        assert_eq!(rules, vec![("base", "other")]);
        assert_eq!(
            r.set.overridden(),
            &[(
                "profiles/mine.yaml".to_string(),
                "builtin:base.yaml".to_string()
            )]
        );
    }

    const HOLDER: &str = "\
profile: p
version: 2
rules:
  - id: route
    when: { extension: test, on: all, each: str_calls, matches: '^r\\.get\\(\"([^\"]*)\"(?:, ([a-z]*))?\\)' }
    emit: { bind: { contract: { kind: route, key: '$1' }, role: provides, holder: '$2' } }
";

    fn held(text: &str) -> Fragment {
        let r = load_set(HOLDER, &[]);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        let mut f = fragment("a.tx", text);
        r.set.apply(&mut f);
        f
    }

    #[test]
    fn holder_binds_local_symbol() {
        let f = held(
            "sym fn reg\nsym fn show\nstrcall reg r.get(\"/a\", show)\nargref reg show local:fn:show\n",
        );
        assert_eq!(f.bindings.len(), 1, "{:?}", f.bindings);
        let b: &Binding = &f.bindings[0];
        assert_eq!(b.symbol, sym_id(&f, "show"));
        assert_eq!(b.confidence, Confidence::Declared);
        assert!(f.refs.is_empty());
    }

    #[test]
    fn holder_emits_bound_reference() {
        let f = held("sym fn reg\nstrcall reg r.get(\"/a\", show)\nargref reg show item:show\n");
        assert!(f.bindings.is_empty(), "{:?}", f.bindings);
        assert_eq!(f.refs.len(), 1);
        let r = &f.refs[0];
        assert_eq!(r.from, sym_id(&f, "reg"));
        assert_eq!(r.kind.0, "provides");
        assert_eq!(r.candidates[0].namespace, "item");
        assert_eq!(r.candidates[0].key, "show");
        assert_eq!(r.confidence, Confidence::Declared);
        let b = r.binds.as_ref().expect("holds a binding");
        assert_eq!(b.contract.canonical(), "contract:route:%2Fa");
        assert_eq!(b.role, katalogr_model::ContractRole::Provides);
        // An inferred argument reference lowers the confidence.
        let f = held("sym fn reg\nstrcall reg r.get(\"/a\", show)\nargref reg show item:show!\n");
        assert_eq!(f.refs[0].confidence, Confidence::Inferred);
    }

    #[test]
    fn holder_fallback_inferred() {
        let check = |f: &Fragment| {
            assert_eq!(f.bindings.len(), 1, "{:?}", f.bindings);
            assert_eq!(f.bindings[0].symbol, sym_id(f, "reg"));
            assert_eq!(f.bindings[0].confidence, Confidence::Inferred);
            assert!(f.refs.is_empty());
        };
        // An empty capture (a closure renders `_`, which the regex does not capture).
        check(&held("sym fn reg\nstrcall reg r.get(\"/a\")\n"));
        // A name with no `refs` entry.
        check(&held("sym fn reg\nstrcall reg r.get(\"/a\", show)\n"));
        // A local name that is not a symbol of the fragment.
        check(&held(
            "sym fn reg\nstrcall reg r.get(\"/a\", show)\nargref reg show local:fn:gone\n",
        ));
    }

    #[test]
    fn holder_without_each_refused() {
        let text = "profile: p\nversion: 2\nrules:\n  - id: a\n    when: { kind: fn }\n    emit:\n      bind: { contract: { kind: k, key: x }, role: provides, holder: '$1' }\n";
        let err = profiles::parse("p.yaml", text).expect_err("refused");
        assert!(err.message.contains("'holder' needs 'each'"), "{err}");
        assert!(err.line.is_some(), "{err}");
    }

    #[test]
    fn version_1_still_reads() {
        assert!(profiles::parse("p.yaml", BASE).is_ok());
        assert!(profiles::parse("p.yaml", EXAMPLE).is_ok());
        assert!(profiles::parse("p.yaml", GENERIC).is_ok());
        // Version 1 stays accepted next to 2.
        assert_eq!(profiles::format::FORMAT_VERSIONS, &[1, 2]);
    }

    #[test]
    fn v2_keys_refused_in_v1() {
        let rule = |when: &str, bind: &str| {
            format!(
                "profile: p\nversion: 1\nrules:\n  - id: a\n    when: {{ {when} }}\n    emit:\n      bind: {{ contract: {{ kind: k, key: x }}, role: provides{bind} }}\n"
            )
        };
        for (text, key, line) in [
            (rule("each: str_calls", ""), "each", 5),
            (rule("on: artifact", ""), "on", 5),
            (rule("kind: fn", ", normalise: upper"), "normalise", 7),
            (rule("kind: fn", ", confidence: inferred"), "confidence", 7),
        ] {
            let (l, why) = refused(&text);
            assert_eq!(l, line, "{key}: {why}");
            assert!(
                why.contains(&format!("`{key}` needs version: 2")),
                "{key}: {why}"
            );
            // The same text is read in version 2 (a holder needs `each` there).
            if key != "holder" {
                let v2 = text.replace("version: 1", "version: 2");
                assert!(profiles::parse("p.yaml", &v2).is_ok(), "{key}");
            }
        }
    }
}
