//! Structure insights through the CLI (FEAT-030): hubs, communities, cycles, dead code and
//! rationale over small `.tx` projects.

mod insights {
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn run(dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).expect("utf8"),
            String::from_utf8(out.stderr).expect("utf8"),
        )
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out, err) = run(dir, args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        out
    }

    /// The parsed `--json` answer and the exit code.
    fn json(repo: &Path, args: &[&str]) -> (i32, Value) {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let (code, out, err) = run(repo, &all);
        let v: Value = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("{all:?}: not JSON ({e}): {out}{err}"));
        (code, v)
    }

    /// A project `demo` with one marked repo `app`; the files are written, then indexed.
    fn project(tmp: &Path, files: &[(&str, &str)]) -> PathBuf {
        let (_, repo) = empty_project(tmp);
        for (name, text) in files {
            std::fs::write(repo.join(name), text).unwrap();
        }
        ok(&repo, &["index"]);
        repo
    }

    /// A project with no files yet; returns the kindex and the repo.
    fn empty_project(tmp: &Path) -> (PathBuf, PathBuf) {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        ok(tmp, &["kindex", "init", kx.to_str().unwrap()]);
        let spec = format!("app={}", repo.display());
        ok(
            tmp,
            &[
                "--kindex",
                kx.to_str().unwrap(),
                "project",
                "init",
                "demo",
                "--repo",
                &spec,
                "--mark",
            ],
        );
        (kx, repo)
    }

    fn names(v: &Value, path: &str, field: &str) -> Vec<String> {
        v["result"][path]
            .as_array()
            .unwrap_or_else(|| panic!("no list {path} in {v}"))
            .iter()
            .map(|i| i[field].as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// The part of a canonical name after the file: `fn:hub`.
    fn short(name: &str) -> &str {
        name.rsplit_once('#').map_or(name, |(_, last)| last)
    }

    /// A hub called from 12 symbols, two of them through a reference that resolved to two
    /// targets (so each counts for half).
    fn hub_files() -> Vec<(&'static str, String)> {
        let mut main =
            String::from("sym fn hub\nsym fn other\nexport k:target hub\nexport k:target other\n");
        for i in 1..=12 {
            main.push_str(&format!("sym fn c{i}\n"));
        }
        for i in 1..=10 {
            main.push_str(&format!("link c{i} calls hub\n"));
        }
        for i in 11..=12 {
            main.push_str(&format!("iref c{i} calls k:target\n"));
        }
        vec![("hubs.tx", main)]
    }

    #[test]
    fn hubs_ranked_by_weighted_degree() {
        let tmp = tempfile::tempdir().unwrap();
        let files = hub_files();
        let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
        let repo = project(tmp.path(), &files);

        let (code, v) = json(&repo, &["hubs", "--min-degree", "1"]);
        assert_eq!(code, 0);
        let items = v["result"]["items"].as_array().unwrap();
        assert_eq!(short(items[0]["node"]["name"].as_str().unwrap()), "fn:hub");
        // 10 whole links and two halves: 11.
        assert_eq!(items[0]["in_degree"], 11.0);
        assert_eq!(items[0]["out_degree"], 0.0);
        assert_eq!(items[0]["degree"], 11.0);
        assert!(items[0]["z"].as_f64().unwrap() > 2.0, "{v}");
        // The other target of the ambiguous reference gets the other halves.
        let other = items
            .iter()
            .find(|i| i["node"]["name"].as_str().unwrap().ends_with("fn:other"))
            .unwrap();
        assert_eq!(other["degree"], 1.0);
        // Ordered by degree: every later hub has no more than the one before.
        let degrees: Vec<f64> = items
            .iter()
            .map(|i| i["degree"].as_f64().unwrap())
            .collect();
        assert!(degrees.windows(2).all(|w| w[0] >= w[1]), "{degrees:?}");

        // Strict drops the weak links; the minimum degree filters.
        let (_, strict) = json(&repo, &["hubs", "--strict", "--min-degree", "5"]);
        let items = strict["result"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["degree"], 10.0);
        // The text form carries the same numbers.
        let text = ok(&repo, &["hubs", "--min-degree", "5"]);
        assert!(text.contains("11.00") && text.contains("#fn:hub"), "{text}");
    }

    #[test]
    fn hubs_at_file_and_package_level() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                ("f1.tx", "fact @ package p1\nsym fn a\nexport x:a a\n"),
                (
                    "f2.tx",
                    "fact @ package p1\nsym fn b\nsym fn b2\nref b calls x:a\nref b2 calls x:a\nexport x:b b\n",
                ),
                (
                    "f3.tx",
                    "fact @ package p2\nsym fn c\nsym fn d\nref c calls x:a\nref d calls x:a\nref d calls x:b\n",
                ),
            ],
        );
        let (_, v) = json(&repo, &["hubs", "--level", "file", "--min-degree", "1"]);
        assert_eq!(v["result"]["level"], "file");
        let items = v["result"]["items"].as_array().unwrap();
        assert_eq!(items[0]["node"]["name"], "app:f1.tx");
        assert_eq!(items[0]["node"]["kind"], "artifact");
        assert_eq!(items[0]["degree"], 4.0);
        assert_eq!(items[0]["members"], 1);

        let (_, v) = json(&repo, &["hubs", "--level", "package", "--min-degree", "1"]);
        assert_eq!(v["result"]["level"], "package");
        let items = v["result"]["items"].as_array().unwrap();
        let got: Vec<(&str, f64)> = items
            .iter()
            .map(|i| {
                (
                    i["node"]["name"].as_str().unwrap(),
                    i["degree"].as_f64().unwrap(),
                )
            })
            .collect();
        // Links inside p1 vanish; p2 uses p1 three times.
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(
            got.contains(&("p1", 3.0)) && got.contains(&("p2", 3.0)),
            "{got:?}"
        );
        assert!(items.iter().all(|i| i["node"]["kind"] == "package"));
        // Symbols per package: p1 holds a, b, b2 and p2 holds c, d.
        assert_eq!(
            items
                .iter()
                .map(|i| i["members"].as_u64().unwrap())
                .sum::<u64>(),
            5
        );
    }

    /// Two clusters of four symbols that call each other, joined by one call.
    fn cluster_files() -> Vec<(&'static str, String)> {
        let cluster = |p: &str| {
            let mut s = String::new();
            for i in 1..=4 {
                s.push_str(&format!("sym fn {p}{i}\n"));
            }
            for (a, b) in [(1, 2), (2, 3), (3, 4), (4, 1), (1, 3), (2, 4)] {
                s.push_str(&format!("link {p}{a} calls {p}{b}\n"));
            }
            s
        };
        let mut a = cluster("a");
        a.push_str("export x:a1 a1\n");
        let mut b = cluster("b");
        b.push_str("ref b1 calls x:a1\n");
        vec![("a.tx", a), ("b.tx", b)]
    }

    #[test]
    fn communities_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let files = cluster_files();
        let files: Vec<(&str, &str)> = files.iter().map(|(n, t)| (*n, t.as_str())).collect();
        let repo = project(tmp.path(), &files);

        let (code, v) = json(&repo, &["communities", "--level", "symbol"]);
        assert_eq!(code, 0);
        assert_eq!(v["result"]["communities_total"], 2);
        assert_eq!(v["result"]["unclustered"], 0);
        assert!(v["result"]["modularity"].as_f64().unwrap() > 0.4, "{v}");
        let items = v["result"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        for c in items {
            assert_eq!(c["size"], 4);
            assert_eq!(c["internal_weight"], 6.0);
            assert_eq!(c["boundary_weight"], 1.0);
            assert_eq!(c["cohesion"], 0.857);
            assert_eq!(c["files"], 1);
            assert_eq!(c["bridges"].as_array().unwrap().len(), 1);
            assert_eq!(c["top"].as_array().unwrap().len(), 4);
            assert!(c["label"]["name"].as_str().unwrap().contains("#fn:"));
        }
        // Each community holds one file's symbols.
        let paths: Vec<&str> = items
            .iter()
            .map(|c| c["label"]["path"].as_str().unwrap())
            .collect();
        assert!(
            paths.contains(&"a.tx") && paths.contains(&"b.tx"),
            "{paths:?}"
        );

        // `--of` reports only the community that holds the node, with every member listed.
        let (_, of) = json(
            &repo,
            &[
                "communities",
                "--level",
                "symbol",
                "--of",
                "app:a.tx#fn:a3",
                "--members",
                "10",
            ],
        );
        let items = of["result"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["label"]["path"], "a.tx");
        // A community above the minimum size is all that clustered: the file level has two nodes.
        let (_, files) = json(&repo, &["communities"]);
        assert_eq!(files["result"]["level"], "file");
        assert_eq!(files["result"]["items"].as_array().unwrap().len(), 0);
        // An unknown node is said so.
        let (_, none) = json(
            &repo,
            &[
                "communities",
                "--level",
                "symbol",
                "--of",
                "app:a.tx#fn:nope",
            ],
        );
        assert!(
            none["result"]["note"]
                .as_str()
                .unwrap()
                .contains("no node named")
        );
        // The text form names the modularity and the top members.
        let text = ok(&repo, &["communities", "--level", "symbol"]);
        assert!(text.starts_with("modularity "), "{text}");
        assert!(
            text.contains("cohesion 0.857") && text.contains("top: "),
            "{text}"
        );
    }

    const INSIGHTS: [&[&str]; 5] = [
        &["hubs", "--min-degree", "1", "--level", "symbol"],
        &["communities", "--level", "symbol"],
        &["cycles", "--level", "symbol"],
        &["dead-code", "--min-confidence", "low"],
        &["rationale"],
    ];

    #[test]
    fn output_deterministic() {
        let files: Vec<(&str, String)> = {
            let mut v = cluster_files();
            v.push((
                "c.tx",
                "sym fn main\nfact main root main\nsym fn lone\nnote lone WHY because\nsym fn x1\nsym fn x2\nlink x1 calls x2\nlink x2 calls x1\n".to_string(),
            ));
            v
        };
        let mut results: Vec<Vec<String>> = Vec::new();
        for order in [[0, 1, 2], [2, 1, 0]] {
            let tmp = tempfile::tempdir().unwrap();
            let (_, repo) = empty_project(tmp.path());
            // The files are indexed one by one, in a different order each time.
            for i in order {
                std::fs::write(repo.join(files[i].0), &files[i].1).unwrap();
                ok(&repo, &["index"]);
            }
            let mut out = Vec::new();
            for args in INSIGHTS {
                let (_, v) = json(&repo, args);
                out.push(v["result"].to_string());
            }
            // The same project indexed again from scratch answers the same.
            ok(&repo, &["index", "--full"]);
            for (i, args) in INSIGHTS.iter().enumerate() {
                let (_, v) = json(&repo, args);
                assert_eq!(out[i], v["result"].to_string(), "{args:?}");
            }
            results.push(out);
        }
        assert_eq!(results[0], results[1]);
        // And the answers are not empty.
        assert!(results[0].iter().all(|r| r.len() > 40), "{:?}", results[0]);
    }

    /// A symbol cycle, a file cycle and a package cycle.
    const CYCLE_FILES: [(&str, &str); 5] = [
        (
            "f1.tx",
            "fact @ package p1\nsym fn a\nsym fn b\nlink a calls b\nlink b calls a\n",
        ),
        (
            "f2.tx",
            "fact @ package p1\nsym fn c\nref c calls x:d\nexport x:c c\n",
        ),
        (
            "f3.tx",
            "fact @ package p1\nsym fn d\nref d calls x:c\nexport x:d d\n",
        ),
        (
            "f4.tx",
            "fact @ package p2\nsym fn e\nref e calls x:f\nexport x:e e\n",
        ),
        (
            "f5.tx",
            "fact @ package p3\nsym fn f\nref f calls x:e\nexport x:f f\n",
        ),
    ];

    #[test]
    fn cycles_at_three_levels() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), &CYCLE_FILES);

        let (code, v) = json(&repo, &["cycles", "--level", "symbol"]);
        assert_eq!(code, 1);
        assert_eq!(v["result"]["cycles_total"], 3);
        assert_eq!(v["result"]["cyclic_nodes"], 6);
        let first = &v["result"]["items"][0];
        assert_eq!(first["size"], 2);
        assert_eq!(first["witness"].as_array().unwrap().len(), 2);
        assert_eq!(first["confidence"], "Extracted");
        assert_eq!(first["witness"][0]["direction"], "Forward");

        let (code, v) = json(&repo, &["cycles"]);
        assert_eq!(code, 1);
        assert_eq!(v["result"]["level"], "file");
        assert_eq!(v["result"]["cycles_total"], 2);
        let members: Vec<String> = v["result"]["items"][0]["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(members, ["app:f2.tx", "app:f3.tx"]);

        // Files of one package do not make a package cycle by themselves.
        let (code, v) = json(&repo, &["cycles", "--level", "package"]);
        assert_eq!(code, 1);
        assert_eq!(v["result"]["cycles_total"], 1);
        let members: Vec<&str> = v["result"]["items"][0]["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        assert_eq!(members, ["p2", "p3"]);

        // The text form: `#id  size n  [confidence]  a -kind-> b -kind-> a`.
        let (code, text, _) = run(&repo, &["cycles", "--level", "symbol"]);
        assert_eq!(code, 1);
        assert!(
            text.contains("#1  size 2  [Extracted]  ") && text.contains(" -calls-> "),
            "{text}"
        );
    }

    #[test]
    fn cycles_through_contracts() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                // f1 consumes the endpoint that f2 provides, and f2 calls back into f1.
                (
                    "f1.tx",
                    "sym fn client\nbind client consumes endpoint:orders\nexport x:client client\n",
                ),
                (
                    "f2.tx",
                    "sym fn serve\nbind serve provides endpoint:orders\nref serve calls x:client\n",
                ),
            ],
        );
        let (code, v) = json(&repo, &["cycles"]);
        assert_eq!(code, 1, "{v}");
        assert_eq!(v["result"]["cycles_total"], 1);
        let kinds: Vec<&str> = v["result"]["items"][0]["witness"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["kind"].as_str().unwrap())
            .collect();
        assert_eq!(kinds.len(), 2);
        assert!(
            kinds.contains(&"via-contract") && kinds.contains(&"calls"),
            "{kinds:?}"
        );
    }

    #[test]
    fn cycles_exit_code() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), &CYCLE_FILES);
        let (code, _, _) = run(&repo, &["cycles"]);
        assert_eq!(code, 1);

        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                ("f1.tx", "sym fn a\nref a calls x:b\n"),
                ("f2.tx", "sym fn b\nexport x:b b\n"),
            ],
        );
        let (code, text, _) = run(&repo, &["cycles"]);
        assert_eq!(code, 0, "{text}");
        assert!(text.starts_with("0 cycles"), "{text}");
        let (code, _) = json(&repo, &["cycles"]);
        assert_eq!(code, 0);
    }

    #[test]
    fn inferred_cycles_opt_in() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                ("f1.tx", "sym fn a\nref a calls x:b\nexport x:a a\n"),
                ("f2.tx", "sym fn b\niref b calls x:a\nexport x:b b\n"),
            ],
        );
        let (code, v) = json(&repo, &["cycles"]);
        assert_eq!((code, &v["result"]["cycles_total"]), (0, &Value::from(0)));
        assert_eq!(v["result"]["include_inferred"], false);

        let (code, v) = json(&repo, &["cycles", "--include-inferred"]);
        assert_eq!(code, 1, "{v}");
        assert_eq!(v["result"]["include_inferred"], true);
        assert_eq!(v["result"]["cycles_total"], 1);
        assert_eq!(v["result"]["items"][0]["confidence"], "Inferred");
        let steps = v["result"]["items"][0]["witness"].as_array().unwrap();
        let weakest: Vec<&str> = steps
            .iter()
            .map(|s| s["confidence"].as_str().unwrap())
            .collect();
        assert!(
            weakest.contains(&"Inferred") && weakest.contains(&"Extracted"),
            "{weakest:?}"
        );
    }

    const DEAD: &str = "\
sym fn main
fact main root main
sym fn used
sym fn lonely
sym fn island
sym fn island_user
sym fn guessed
export x:guessed guessed
link main calls used
link island_user calls island
iref main calls x:guessed
";

    fn candidates(v: &Value) -> Vec<(String, String, String)> {
        v["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                (
                    short(i["symbol"]["name"].as_str().unwrap()).to_string(),
                    i["class"].as_str().unwrap().to_string(),
                    i["confidence"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn dead_code_confidence_classes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), &[("d.tx", DEAD)]);

        let (code, v) = json(&repo, &["dead-code", "--min-confidence", "low"]);
        assert_eq!(code, 0);
        let s = |a: &str, b: &str, c: &str| (a.to_string(), b.to_string(), c.to_string());
        assert_eq!(
            candidates(&v),
            [
                s("fn:lonely", "unreferenced", "high"),
                s("fn:island_user", "unreferenced", "high"),
                s("fn:island", "unreachable", "medium"),
                s("fn:guessed", "weakly-reached", "low"),
            ]
        );
        assert_eq!(
            (
                v["result"]["high"].as_u64(),
                v["result"]["medium"].as_u64(),
                v["result"]["low"].as_u64()
            ),
            (Some(2), Some(1), Some(1))
        );
        // The default floor is medium: the low class is counted but not listed.
        let (_, v) = json(&repo, &["dead-code"]);
        assert_eq!(candidates(&v).len(), 3);
        assert_eq!(v["result"]["low"], 1);
        // High only.
        let (_, v) = json(&repo, &["dead-code", "--min-confidence", "high"]);
        assert_eq!(candidates(&v).len(), 2);
        // Text lines: `confidence  class  kind  name  path:line  in n`.
        let text = ok(&repo, &["dead-code"]);
        assert!(
            text.contains("high  unreferenced  fn  app:d.tx#fn:lonely  d.tx:4  in 0"),
            "{text}"
        );
        // `--check` fails when something is listed.
        let (code, _, _) = run(&repo, &["dead-code", "--check"]);
        assert_eq!(code, 1);
        let (code, _, _) = run(&repo, &["dead-code", "--path", "elsewhere/", "--check"]);
        assert_eq!(code, 0);
    }

    #[test]
    fn dead_code_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, repo) = empty_project(tmp.path());
        std::fs::write(
            repo.join("r.tx"),
            "sym fn start\nfact start root main\nsym fn handler\nbind handler provides channel:orders\n\
             sym fn registered\nsym fn stray\n",
        )
        .unwrap();
        let profiles = kx.join("projects/demo/profiles");
        std::fs::create_dir_all(&profiles).unwrap();
        std::fs::write(
            profiles.join("roots.yaml"),
            "profile: roots\nversion: 1\nrules:\n  - id: registry\n    when: { extension: test, kind: fn, name: '^registered$' }\n    emit:\n      fact: { root: declared }\n",
        )
        .unwrap();
        ok(&repo, &["index"]);

        let (_, v) = json(&repo, &["dead-code", "--min-confidence", "low"]);
        // The marked entry, the contract provider and the profile-declared root are not
        // candidates; the stray function is.
        let got = candidates(&v);
        assert_eq!(got.len(), 1, "{v}");
        assert_eq!(got[0].0, "fn:stray");
        assert_eq!(v["result"]["roots"], 3);
        assert_eq!(v["result"]["roots_by_reason"]["main"], 1);
        assert_eq!(v["result"]["roots_by_reason"]["contract"], 1);
        assert_eq!(v["result"]["roots_by_reason"]["declared"], 1);
    }

    #[test]
    fn internal_drops_api_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[(
                "l.tx",
                "sym fn start\nfact start root main\nsym fn exported\nfact exported root api\nsym fn helper\nlink exported calls helper\n",
            )],
        );
        let (_, v) = json(&repo, &["dead-code", "--min-confidence", "low"]);
        assert!(candidates(&v).is_empty(), "{v}");
        assert_eq!(v["result"]["roots_by_reason"]["api"], 1);

        let (_, v) = json(
            &repo,
            &["dead-code", "--internal", "--min-confidence", "low"],
        );
        let names: Vec<String> = candidates(&v).into_iter().map(|c| c.0).collect();
        assert_eq!(names, ["fn:exported", "fn:helper"]);
        assert!(v["result"]["roots_by_reason"].get("api").is_none());
    }

    #[test]
    fn dead_code_without_roots_refuses() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[("n.tx", "sym fn one\nsym fn two\nlink one calls two\n")],
        );
        let (code, v) = json(&repo, &["dead-code", "--min-confidence", "low", "--check"]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["result"]["items"].as_array().unwrap().len(), 0);
        assert_eq!(v["result"]["candidates_total"], 0);
        let note = v["result"]["note"].as_str().unwrap();
        assert!(
            note.contains("test") && note.contains("index --full"),
            "{note}"
        );
        let text = ok(&repo, &["dead-code"]);
        assert!(text.contains("re-extract"), "{text}");
    }

    fn rationale_files() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "code.tx",
                "sym fn s\nsym fn t\nnote s WHY kept for the legacy exporter\n\
                 ref s annotated-by board-adr:ADR-0001\nref s refs-item board-item:FEAT-001\n\
                 export file:app:code.tx s\nlink t calls s\n",
            ),
            (
                "board.tx",
                "sym adr ADR-0001\nexport board-adr:ADR-0001 ADR-0001\nsym item FEAT-001\nexport board-item:FEAT-001 FEAT-001\n",
            ),
        ]
    }

    #[test]
    fn rationale_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut files = rationale_files();
        files.push(("docs.md", "# Overview\n\nSee [the code](code.tx).\n"));
        let repo = project(tmp.path(), &files);

        let (code, v) = json(&repo, &["rationale", "--path", "code"]);
        assert_eq!(code, 0, "{v}");
        let entries: Vec<(String, String)> = v["result"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["kind"].as_str().unwrap().into(),
                    e["text"].as_str().unwrap().into(),
                )
            })
            .collect();
        assert!(
            entries.contains(&("why".into(), "kept for the legacy exporter".into())),
            "{entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.0 == "adr" && e.1.contains("ADR-0001")),
            "{entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.0 == "item" && e.1.contains("FEAT-001")),
            "{entries:?}"
        );
        assert!(
            entries
                .iter()
                .any(|e| e.0 == "doc" && e.1.contains("docs.md#section:overview")),
            "{entries:?}"
        );
        // The link from the document reaches the file and, through the export, the symbol.
        assert_eq!(v["result"]["entries_total"], 5);

        // By symbol, resolved like `explain`.
        let (code, v) = json(&repo, &["rationale", "s"]);
        assert_eq!(code, 0, "{v}");
        assert_eq!(v["result"]["entries_total"], 4);
        // The text form: `kind  name  path:line  text`.
        let text = ok(&repo, &["rationale", "s"]);
        assert!(
            text.contains("why  app:code.tx#fn:s  code.tx:3  kept for the legacy exporter"),
            "{text}"
        );
        // A symbol that is not there is no match.
        let (code, _, _) = run(&repo, &["rationale", "absent"]);
        assert_eq!(code, 4);
    }

    #[test]
    fn unexplained_hubs_named() {
        let tmp = tempfile::tempdir().unwrap();
        let mut main =
            String::from("sym fn explained\nsym fn bare\nnote explained NOTE the busiest one\n");
        for i in 1..=4 {
            main.push_str(&format!("sym fn u{i}\nlink u{i} calls explained\n"));
        }
        for i in 1..=3 {
            main.push_str(&format!("link u{i} calls bare\n"));
        }
        let repo = project(tmp.path(), &[("h.tx", &main)]);
        let (code, v) = json(&repo, &["rationale", "--hubs", "2"]);
        assert_eq!(code, 0, "{v}");
        let uncovered = names(&v, "uncovered", "name");
        assert_eq!(uncovered, ["app:h.tx#fn:bare"]);
        assert_eq!(v["result"]["entries_total"], 1);
        let text = ok(&repo, &["rationale", "--hubs", "2"]);
        assert!(text.contains("no rationale: app:h.tx#fn:bare"), "{text}");
        assert!(text.contains("note  app:h.tx#fn:explained"), "{text}");
    }

    #[test]
    fn why_shows_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[(
                "w.tx",
                "sym fn s\nnote s WHY because of the exporter\nnote s NOTE second\n",
            )],
        );
        let text = ok(&repo, &["why", "w.tx:1"]);
        assert!(
            text.contains("WHY  line 2  because of the exporter"),
            "{text}"
        );
        assert!(text.contains("NOTE  line 3  second"), "{text}");
        let (_, v) = json(&repo, &["why", "w.tx:1"]);
        let notes = v["result"]["notes"].as_array().unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0]["marker"], "WHY");
        assert_eq!(notes[0]["line"], 2);
        assert_eq!(notes[0]["text"], "because of the exporter");
    }

    #[test]
    fn budget_truncates_items() {
        let tmp = tempfile::tempdir().unwrap();
        let mut main = String::from("sym fn main\nfact main root main\n");
        for i in 1..=8 {
            main.push_str(&format!(
                "sym fn caller{i}\nsym fn callee{i}\nlink caller{i} calls callee{i}\n"
            ));
        }
        let repo = project(tmp.path(), &[("b.tx", &main)]);

        let (_, full) = json(&repo, &["hubs", "--min-degree", "1"]);
        let all = full["result"]["items"].as_array().unwrap().len();
        assert_eq!(all, 16);
        assert!(full["truncated"].is_null());
        let (_, v) = json(&repo, &["--budget", "60", "hubs", "--min-degree", "1"]);
        let kept = v["result"]["items"].as_array().unwrap().len();
        assert!(kept > 0 && kept < all, "{kept}");
        // The summary stays, and the truncation is recorded.
        assert_eq!(v["result"]["nodes"], 17);
        assert_eq!(v["truncated"]["shown"], kept);
        assert_eq!(v["truncated"]["omitted"], all - kept);
        assert_eq!(v["truncated"]["budget"], 60);

        let (_, v) = json(
            &repo,
            &["--budget", "60", "dead-code", "--min-confidence", "low"],
        );
        // Eight callers nobody calls (high) and their eight callees (medium).
        assert_eq!(v["result"]["high"], 8);
        assert_eq!(v["result"]["medium"], 8);
        let kept = v["result"]["items"].as_array().unwrap().len();
        assert!(kept > 0 && kept < 16, "{kept}");
        assert_eq!(v["truncated"]["omitted"], 16 - kept);
        // Text output says how many were left out.
        let text = ok(
            &repo,
            &["--budget", "60", "dead-code", "--min-confidence", "low"],
        );
        assert!(text.contains("more omitted"), "{text}");
        assert!(text.contains("entry points"), "{text}");
    }
}
