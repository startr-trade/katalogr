//! Retrieval evaluation through the CLI (FEAT-061): golden sets, scoring, baselines, the gate,
//! sweeps, cross-repository breakdowns and project mode.

mod eval {
    use serde_json::Value;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn fixtures() -> PathBuf {
        root().join("fixtures/eval")
    }

    fn run_in(dir: &Path, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .expect("run katalogr");
        (
            out.status.code().expect("exit code"),
            String::from_utf8(out.stdout).unwrap(),
            String::from_utf8(out.stderr).unwrap(),
        )
    }

    fn run(args: &[&str]) -> (i32, String, String) {
        run_in(&root(), args)
    }

    fn report(args: &[&str]) -> Value {
        let mut full = vec!["--json", "eval", "run"];
        full.extend_from_slice(args);
        let (code, out, err) = run(&full);
        assert_eq!(code, 0, "{args:?}: {out} {err}");
        let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        v["result"].clone()
    }

    fn metric(r: &Value, name: &str, labels: &[(&str, &str)]) -> Option<f64> {
        r["metrics"].as_array().unwrap().iter().find_map(|m| {
            let l = m.get("labels").and_then(Value::as_object);
            let want = labels.len();
            let have = l.map_or(0, |l| l.len());
            let same = m["name"] == name
                && want == have
                && labels
                    .iter()
                    .all(|(k, v)| l.is_some_and(|l| l.get(*k).and_then(Value::as_str) == Some(*v)));
            same.then(|| m["value"].as_f64().unwrap())
        })
    }

    fn query<'a>(r: &'a Value, set: &str, id: &str) -> &'a Value {
        r["queries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["set"] == set && q["id"] == id)
            .unwrap_or_else(|| panic!("no query {set}/{id}"))
    }

    #[test]
    fn fixture_suite_runs() {
        let r = report(&["--sets", "fixtures/eval"]);
        assert_eq!(r["mode"], "fixture");
        assert_eq!(r["queries"].as_array().unwrap().len(), 35);
        let sets: Vec<&str> = r["sets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["set"].as_str().unwrap())
            .collect();
        assert_eq!(sets, ["core", "cross"]);
        assert_eq!(metric(&r, "retrieval.evidence_missing", &[]), Some(0.0));
        assert!(metric(&r, "retrieval.ndcg@10", &[]).unwrap() > 0.9);
        assert!(metric(&r, "retrieval.hit@1", &[("set", "core"), ("op", "search")]).is_some());
        let (code, out, _) = run(&["eval", "run", "--sets", "fixtures/eval"]);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("core") && out.contains("cross"), "{out}");
    }

    #[test]
    fn route_and_scope_checked() {
        let r = report(&["--sets", "fixtures/eval/core"]);
        let none = query(&r, "core", "s12");
        assert_eq!(none["route_actual"], "no-match");
        assert_eq!(none["route_ok"], true);
        let amb = query(&r, "core", "e04");
        assert_eq!(amb["route_expected"], "ambiguous");
        assert_eq!(amb["route_actual"], "ambiguous");
        assert_eq!(metric(&r, "retrieval.route_accuracy", &[]), Some(1.0));
        // s01 expects src/** only, but the TypeScript client and the docs also answer.
        assert!(query(&r, "core", "s01")["scope_leaks"].as_u64().unwrap() > 0);
        assert!(query(&r, "core", "s05")["scope_leaks"].is_null());
        assert!(metric(&r, "retrieval.scope_leak_rate", &[]).unwrap() > 0.0);
    }

    #[test]
    fn write_baseline_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("b.json");
        let f = file.to_str().unwrap();
        let (code, _, err) = run(&[
            "eval",
            "run",
            "--sets",
            "fixtures/eval",
            "--write-baseline",
            f,
        ]);
        assert_eq!(code, 0, "{err}");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(
            text.ends_with("}\n") && text.contains("\n  \"format\": 1"),
            "{}",
            &text[..80]
        );
        let v: Value = serde_json::from_str(&text).unwrap();
        assert!(v.get("comparison").is_none());
        let (code, out, err) = run(&[
            "eval",
            "run",
            "--sets",
            "fixtures/eval",
            "--baseline",
            f,
            "--gate",
        ]);
        assert_eq!(code, 0, "{out} {err}");
        assert!(out.contains("gate passed"), "{out}");
        // A baseline recorded with other parameters is refused (usage, exit 2).
        let (code, _, err) = run(&[
            "eval",
            "run",
            "--sets",
            "fixtures/eval",
            "--baseline",
            f,
            "--param",
            "search.k1=2.0",
        ]);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("other parameters"), "{err}");
        // Blessing the what-if run is allowed and the baseline then matches it.
        let (code, _, _) = run(&[
            "eval",
            "run",
            "--sets",
            "fixtures/eval",
            "--param",
            "search.k1=2.0",
            "--write-baseline",
            f,
        ]);
        assert_eq!(code, 0);
    }

    #[test]
    fn gate_fails_on_regression() {
        let tmp = tempfile::tempdir().unwrap();
        let mut b: Value = serde_json::from_str(
            &std::fs::read_to_string(fixtures().join("baseline.json")).unwrap(),
        )
        .unwrap();
        for m in b["metrics"].as_array_mut().unwrap() {
            if m["name"] == "retrieval.ndcg@10" && m.get("labels").is_none() {
                let v = m["value"].as_f64().unwrap();
                m["value"] = Value::from(v + 0.1);
            }
        }
        let file = tmp.path().join("raised.json");
        std::fs::write(&file, serde_json::to_string_pretty(&b).unwrap()).unwrap();
        let f = file.to_str().unwrap();
        let (code, out, _) = run(&[
            "eval",
            "run",
            "--sets",
            "fixtures/eval",
            "--baseline",
            f,
            "--gate",
        ]);
        assert_eq!(code, 1, "{out}");
        assert!(out.contains("REGRESSION retrieval.ndcg@10"), "{out}");
        assert!(out.contains("gate failed"), "{out}");
        // Without --gate the comparison is printed and the exit is 0.
        let (code, out, _) = run(&["eval", "run", "--sets", "fixtures/eval", "--baseline", f]);
        assert_eq!(code, 0, "{out}");
        // A changed set version fails the gate too.
        let mut b2: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        b2["sets"][0]["version"] = Value::from(9);
        std::fs::write(&file, serde_json::to_string_pretty(&b2).unwrap()).unwrap();
        let (code, out, _) = run(&[
            "eval",
            "run",
            "--sets",
            "fixtures/eval",
            "--baseline",
            f,
            "--gate",
        ]);
        assert_eq!(code, 1);
        assert!(out.contains("version changed: core"), "{out}");
        // --gate without a baseline is a usage error.
        let (code, _, _) = run(&["eval", "run", "--sets", "fixtures/eval", "--gate"]);
        assert_eq!(code, 2);
    }

    /// FEAT-077 R-22. Scoring `refs` on the folded answer leaves the fixture baseline as it is:
    /// no fixture export key has 50 targets.
    #[test]
    fn baseline_unchanged_by_folding() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("baseline.json");
        let (code, _, err) = run(&[
            "eval",
            "run",
            "--sets",
            "fixtures/eval",
            "--write-baseline",
            file.to_str().unwrap(),
        ]);
        assert_eq!(code, 0, "{err}");
        let made = std::fs::read_to_string(&file).unwrap();
        let kept = std::fs::read_to_string(fixtures().join("baseline.json")).unwrap();
        assert!(
            made == kept,
            "the re-blessed baseline differs from the checked-in one"
        );
    }

    #[test]
    fn fixture_suite_meets_baseline() {
        let (code, out, err) = run(&[
            "eval",
            "run",
            "--sets",
            "fixtures/eval",
            "--baseline",
            "fixtures/eval/baseline.json",
            "--gate",
        ]);
        assert_eq!(code, 0, "{out} {err}");
    }

    /// FEAT-093 R-9. The command CI's retrieval gate runs passes against the committed baseline.
    #[test]
    fn gate_passes_on_committed_baseline() {
        let (code, out, err) = run(&[
            "eval",
            "run",
            "--sets",
            "fixtures/eval",
            "--baseline",
            "fixtures/eval/baseline.json",
            "--gate",
        ]);
        assert_eq!(code, 0, "{out} {err}");
        assert!(out.contains("gate passed"), "{out}");
    }

    /// FEAT-072 R-2. The same gate against the same baseline at one thread.
    #[test]
    fn fixture_suite_meets_baseline_one_thread() {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(root())
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .env("KATALOGR_THREADS", "1")
            .args([
                "eval",
                "run",
                "--sets",
                "fixtures/eval",
                "--baseline",
                "fixtures/eval/baseline.json",
                "--gate",
            ])
            .output()
            .expect("run katalogr");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{} {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn sweep_reports_each_combination() {
        let (code, out, err) = run(&[
            "--json",
            "eval",
            "sweep",
            "--sets",
            "fixtures/eval",
            "--param",
            "search.k1=1.0,1.2",
            "--param",
            "search.b=0.5,0.75",
        ]);
        assert_eq!(code, 0, "{out} {err}");
        let v: Value = serde_json::from_str(&out).unwrap();
        let r = &v["result"];
        assert_eq!(r["combinations"], 4);
        assert_eq!(r["objective"], "retrieval.ndcg@10");
        let rows = r["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 4);
        let combos: std::collections::BTreeSet<String> = rows
            .iter()
            .map(|row| {
                format!(
                    "{}/{}",
                    row["params"]["search.k1"], row["params"]["search.b"]
                )
            })
            .collect();
        assert_eq!(combos.len(), 4);
        assert_eq!(rows.iter().filter(|r| r["is_default"] == true).count(), 1);
        let objs: Vec<f64> = rows
            .iter()
            .map(|r| r["objective"].as_f64().unwrap())
            .collect();
        assert!(objs.windows(2).all(|w| w[0] >= w[1]), "{objs:?}");
        // Unknown objectives and keys are usage errors.
        let (code, _, err) = run(&[
            "eval",
            "sweep",
            "--sets",
            "fixtures/eval",
            "--param",
            "search.k1=1.0",
            "--objective",
            "retrieval.nope",
        ]);
        assert_eq!(code, 2, "{err}");
        let (code, _, _) = run(&[
            "eval",
            "sweep",
            "--sets",
            "fixtures/eval",
            "--param",
            "search.zz=1",
        ]);
        assert_eq!(code, 2);
        let (code, out, _) = run(&[
            "eval",
            "sweep",
            "--sets",
            "fixtures/eval/core",
            "--param",
            "search.k1=1.0,1.2",
        ]);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("[default]"), "{out}");
    }

    #[test]
    fn golden_explain_contract() {
        let r = report(&["--sets", "fixtures/eval/cross"]);
        let c11 = query(&r, "cross", "c11");
        assert_eq!(c11["route_expected"], "found");
        assert_eq!(c11["route_actual"], "found");
        assert_eq!(c11["route_ok"], true);
        assert_eq!(c11["relevant_in_k"], 2);
        assert_eq!(c11["hit"]["10"], 1.0);
    }

    #[test]
    fn cross_repo_breakdown() {
        let r = report(&["--sets", "fixtures/eval/cross"]);
        assert_eq!(query(&r, "cross", "c01")["scope_class"], "cross");
        assert_eq!(query(&r, "cross", "c02")["scope_class"], "cross");
        assert_eq!(query(&r, "cross", "c03")["scope_class"], "within");
        assert_eq!(
            metric(&r, "retrieval.queries", &[("scope", "cross")]),
            Some(4.0)
        );
        assert_eq!(
            metric(&r, "retrieval.queries", &[("scope", "within")]),
            Some(7.0)
        );
        assert!(metric(&r, "retrieval.ndcg@10", &[("scope", "cross")]).is_some());
        // The WASM contract query finds both sides, in two repositories.
        let c05 = query(&r, "cross", "c05");
        assert_eq!(c05["hit"]["1"], 1.0);
        assert_eq!(c05["relevant_in_k"], 2);
    }

    fn copy_dir(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for e in std::fs::read_dir(from).unwrap().flatten() {
            let (src, dst) = (e.path(), to.join(e.file_name()));
            if src.is_dir() {
                copy_dir(&src, &dst);
            } else {
                std::fs::copy(&src, &dst).unwrap();
            }
        }
    }

    #[test]
    fn project_mode_reads_kindex_sets() {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        copy_dir(&fixtures().join("cross/repos"), &t.join("repos"));
        let kx = t.join("kx");
        let (svc, ui) = (t.join("repos/svc"), t.join("repos/ui"));
        let ok = |dir: &Path, args: &[&str]| {
            let (code, out, err) = run_in(dir, args);
            assert_eq!(code, 0, "{args:?}: {out} {err}");
            out
        };
        ok(t, &["kindex", "init", kx.to_str().unwrap()]);
        ok(
            t,
            &[
                "--kindex",
                kx.to_str().unwrap(),
                "project",
                "init",
                "demo",
                "--mark",
                "--repo",
                &format!("svc={}", svc.display()),
                "--repo",
                &format!("ui={}", ui.display()),
            ],
        );
        ok(&svc, &["index", "--full"]);
        // The project's golden sets: the cross set without `repos`.
        let text = std::fs::read_to_string(fixtures().join("cross/golden.yaml")).unwrap();
        let (head, queries) = text.split_once("queries:\n").unwrap();
        let head: String = head
            .lines()
            .filter(|l| !l.starts_with("repos:") && !l.starts_with("  - {name:"))
            .map(|l| format!("{l}\n"))
            .collect();
        let eval_dir = kx.join("projects/demo/eval");
        std::fs::create_dir_all(&eval_dir).unwrap();
        std::fs::write(
            eval_dir.join("cross.yaml"),
            format!("{head}queries:\n{queries}"),
        )
        .unwrap();

        let out = ok(&svc, &["--json", "eval", "run", "--project-sets"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        let r = &v["result"];
        assert_eq!(r["mode"], "project");
        assert_eq!(r["queries"].as_array().unwrap().len(), 11);
        assert_eq!(metric(r, "retrieval.evidence_missing", &[]), Some(0.0));
        assert!(v["watermark"].as_u64().unwrap() > 0);
        // Bless a project baseline; the default baseline is then picked up and gated.
        ok(
            &svc,
            &[
                "eval",
                "run",
                "--project-sets",
                "--write-baseline",
                eval_dir.join("baseline.json").to_str().unwrap(),
            ],
        );
        let out = ok(&svc, &["eval", "run", "--project-sets", "--gate"]);
        assert!(out.contains("gate passed"), "{out}");
        // A set with `repos` is refused in project mode; --sets and --project-sets conflict.
        std::fs::write(eval_dir.join("bad.yaml"), text).unwrap();
        let (code, _, err) = run_in(&svc, &["eval", "run", "--project-sets"]);
        assert_eq!(code, 2, "{err}");
        assert!(err.contains("repos"), "{err}");
        let (code, _, _) = run_in(&svc, &["eval", "run", "--project-sets", "--sets", "x"]);
        assert_eq!(code, 2);
    }

    #[test]
    fn output_deterministic() {
        let a = run(&["--json", "eval", "run", "--sets", "fixtures/eval"]);
        let b = run(&["--json", "eval", "run", "--sets", "fixtures/eval"]);
        assert_eq!(a.0, 0);
        assert_eq!(a.1, b.1);
        assert!(!a.1.is_empty());
    }
}
