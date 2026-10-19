//! FEAT-001 R-7. The FEAT-023 fixture corpus does not exist yet, so this test indexes a small
//! inline multi-file repository.
mod support;

use camino::Utf8PathBuf;
use katalogr_model::RelPath;
use katalogr_store::{Kindex, ProjectName};
use support::{fresh_index, open_engine};

mod graph {
    use super::*;

    fn shard_bytes(kx: &Utf8PathBuf) -> Vec<(String, Vec<u8>)> {
        let dir = Kindex::open(kx)
            .unwrap()
            .project(&ProjectName::new("p").unwrap())
            .unwrap();
        dir.list_shards()
            .unwrap()
            .into_iter()
            .map(|(repo, path): (String, RelPath)| {
                let bytes = std::fs::read(dir.shard_path(&repo, &path)).unwrap();
                (format!("{repo}:{path}"), bytes)
            })
            .collect()
    }

    #[test]
    fn deterministic_output() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::write(work.join("a.tx"), "sym fn A\nexport k:a A\n").unwrap();
        std::fs::write(
            work.join("b.tx"),
            "sym fn B\nsym struct S\nref B calls k:a\nlink S contains B\n",
        )
        .unwrap();
        std::fs::write(
            work.join("sub/c.tx"),
            "sym fn C\nref C calls k:a\nbind C provides ch:x\n",
        )
        .unwrap();

        let (kx1, g1) = fresh_index(&root, "kx1", &work);
        let (kx2, g2) = fresh_index(&root, "kx2", &work);
        assert_eq!(g1, g2);
        assert!(g1.ends_with('\n'));
        assert!(g1.contains(r#""node":"artifact""#));
        assert!(g1.contains(r#""link":true"#));
        assert!(g1.contains("r:a.tx#fn:A"), "canonical names, not ids");
        let s1 = shard_bytes(&kx1);
        assert_eq!(s1.len(), 3);
        assert_eq!(s1, shard_bytes(&kx2), "shard files are byte-identical");

        // Re-opening and re-reading gives the same text.
        let e = open_engine(&kx1, &work);
        assert_eq!(support::drained_graph(&e), g1);
    }
}

const PROFILE: &str = "\
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
";

fn is_hex_id(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn lines(g: &str) -> Vec<serde_json::Value> {
    g.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn indexed(files: &[(&str, &str)]) -> String {
    let tmp = tempfile::tempdir().unwrap();
    let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    for (n, t) in files {
        std::fs::write(work.join(n), t).unwrap();
    }
    fresh_index(&root, "kx", &work).1
}

mod export {
    use super::*;

    const API: &str = "sym fn serve\nbind serve provides channel:orders\n";
    const APP: &str = "sym fn handle\nbind handle consumes channel:orders\n";

    #[test]
    fn contracts_exported_as_nodes() {
        let g = indexed(&[("api.tx", API), ("app.tx", APP)]);
        let c: Vec<_> = lines(&g)
            .into_iter()
            .filter(|v| v["node"] == "contract")
            .collect();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0]["name"], "contract:channel:orders");
        assert_eq!(c[0]["kind"], "channel");
        assert_eq!(c[0]["key"], "orders");
        assert_eq!(c[0]["providers"], 1);
        assert_eq!(c[0]["consumers"], 1);
        assert_eq!(c[0]["declarers"], 0);
        // After the symbol nodes, before the links.
        let kinds: Vec<_> = lines(&g)
            .iter()
            .map(|v| {
                if v["link"] == true {
                    "link"
                } else {
                    v["node"].as_str().unwrap()
                }
                .to_string()
            })
            .collect();
        let mut sorted = kinds.clone();
        sorted.sort_by_key(|k| match k.as_str() {
            "artifact" => 0,
            "symbol" => 1,
            "contract" => 2,
            _ => 3,
        });
        assert_eq!(kinds, sorted);
    }

    #[test]
    fn binding_links_use_contract_names() {
        let g = indexed(&[("api.tx", API), ("app.tx", APP)]);
        let binds: Vec<_> = lines(&g)
            .into_iter()
            .filter(|v| v["link"] == true && v["to"].as_str().unwrap().starts_with("contract:"))
            .collect();
        assert_eq!(binds.len(), 2);
        for v in lines(&g).iter().filter(|v| v["link"] == true) {
            assert!(!is_hex_id(v["to"].as_str().unwrap()), "{v}");
            assert!(!is_hex_id(v["from"].as_str().unwrap()), "{v}");
        }
        assert!(
            binds
                .iter()
                .any(|v| v["kind"] == "provides" && v["to"] == "contract:channel:orders")
        );
    }

    #[test]
    fn no_contracts_unchanged() {
        let g = indexed(&[
            ("a.tx", "sym fn A\nexport k:a A\n"),
            ("b.tx", "sym fn B\nref B calls k:a\n"),
        ]);
        assert!(!g.contains(r#""node":"contract""#));
        assert!(!g.contains("produced_by"));
    }

    #[test]
    fn contract_order_stable() {
        let fwd =
            "sym fn a\nsym fn b\nbind a provides channel:alpha\nbind b provides channel:beta\n";
        let rev =
            "sym fn a\nsym fn b\nbind b provides channel:beta\nbind a provides channel:alpha\n";
        let g1 = indexed(&[("x.tx", fwd)]);
        let g2 = indexed(&[("x.tx", rev)]);
        let names = |g: &str| -> Vec<String> {
            lines(g)
                .iter()
                .filter(|v| v["node"] == "contract")
                .map(|v| v["name"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(
            names(&g1),
            vec!["contract:channel:alpha", "contract:channel:beta"]
        );
        assert_eq!(names(&g1), names(&g2));
        let strip = |g: &str| -> Vec<String> {
            let mut v: Vec<String> = lines(g)
                .iter()
                .filter(|v| v["link"] == true)
                .map(|v| format!("{} {} {}", v["from"], v["kind"], v["to"]))
                .collect();
            v.sort();
            v
        };
        assert_eq!(strip(&g1), strip(&g2));
    }

    #[test]
    fn produced_by_exported_when_present() {
        let tmp = tempfile::tempdir().unwrap();
        let root = Utf8PathBuf::from_path_buf(tmp.path().to_path_buf()).unwrap();
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(work.join("app.tx"), "sym fn handle_orders\n").unwrap();
        std::fs::write(work.join("api.tx"), API).unwrap();
        let kx = root.join("kx");
        support::init_project(&kx);
        let e = open_engine(&kx, &work);
        let pdir = e.dir().root().join(katalogr_engine::profiles::PROFILES_DIR);
        std::fs::create_dir_all(&pdir).unwrap();
        std::fs::write(pdir.join("example.yaml"), PROFILE).unwrap();
        let g = support::full_index(&e);
        let ls = lines(&g);
        let consumes: Vec<_> = ls
            .iter()
            .filter(|v| v["link"] == true && v["kind"] == "consumes")
            .collect();
        assert_eq!(consumes.len(), 1, "{g}");
        assert_eq!(consumes[0]["produced_by"], "profile:example#bind-channel");
        assert_eq!(consumes[0]["to"], "contract:channel:orders");
        let provides: Vec<_> = ls
            .iter()
            .filter(|v| v["link"] == true && v["kind"] == "provides")
            .collect();
        assert_eq!(provides.len(), 1);
        assert!(provides[0].get("produced_by").is_none());
    }
}
