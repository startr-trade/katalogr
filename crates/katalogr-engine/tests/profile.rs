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
        let (line, why) = refused("profile: p\nversion: 2\nrules: []\n");
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
}
