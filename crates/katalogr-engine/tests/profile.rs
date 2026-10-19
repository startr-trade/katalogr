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
}
