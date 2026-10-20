//! The `coverage` command over the synthetic two-repository corpus `fixtures/coverage/mixed`
//! (FEAT-062).

mod coverage {
    use katalogr_dist::analytics::CoverageReport;
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

    fn copy_dir(src: &Path, dst: &Path) {
        std::fs::create_dir_all(dst).unwrap();
        for e in std::fs::read_dir(src).unwrap() {
            let e = e.unwrap();
            let to = dst.join(e.file_name());
            if e.file_type().unwrap().is_dir() {
                copy_dir(&e.path(), &to);
            } else {
                std::fs::copy(e.path(), to).unwrap();
            }
        }
    }

    /// The corpus copied under `tmp`, a two-repository project over it, indexed. Returns the
    /// working directory (the `alpha` repository).
    fn mixed(tmp: &Path) -> PathBuf {
        let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/coverage/mixed");
        let alpha = tmp.join("alpha");
        let beta = tmp.join("beta");
        copy_dir(&fx.join("alpha"), &alpha);
        copy_dir(&fx.join("beta"), &beta);
        let kx = tmp.join("kx");
        ok(tmp, &["kindex", "init", kx.to_str().unwrap()]);
        ok(
            tmp,
            &[
                "--kindex",
                kx.to_str().unwrap(),
                "project",
                "init",
                "demo",
                "--repo",
                &format!("alpha={}", alpha.display()),
                "--repo",
                &format!("beta={}", beta.display()),
                "--mark",
            ],
        );
        ok(&alpha, &["index", "--full"]);
        alpha
    }

    fn report(repo: &Path, args: &[&str]) -> (Value, CoverageReport) {
        let mut all = vec!["--json", "coverage"];
        all.extend_from_slice(args);
        let out = ok(repo, &all);
        let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        let r: CoverageReport = serde_json::from_value(v["result"].clone()).unwrap();
        (v, r)
    }

    #[test]
    fn reports_per_language_extension_repo() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = mixed(tmp.path());
        let (v, r) = report(&repo, &[]);
        assert_eq!(v["schema"], 1);

        // Counted by hand from the corpus: alpha holds core.tx and twin.tx (the test format),
        // notes.md, broken.ts and app.js; beta holds client.tx.
        let t = &r.totals;
        assert_eq!(t.files_indexed, 6);
        // core.tx: one resolved call, one call with two targets (inferred), one to a name
        // nothing exports; notes.md: a link to a missing file.
        assert_eq!(t.refs_total, 4);
        assert_eq!(t.refs_resolved, 1);
        assert_eq!(t.refs_inferred, 1);
        assert_eq!(t.refs_unresolved, 2);
        assert_eq!(t.refs_unlinked, 0);
        assert_eq!(t.resolution_rate, Some(0.25));
        assert_eq!(t.inferred_rate, Some(0.25));
        assert_eq!(t.unresolved_rate, Some(0.5));
        // payments is provided and never consumed; orders is consumed and never provided.
        assert_eq!(t.contracts_dangling, 2);
        assert_eq!(r.dangling_contracts_by_kind.len(), 1);
        assert_eq!(r.dangling_contracts_by_kind[0].kind, "channel");
        assert_eq!(r.dangling_contracts_by_kind[0].dangling, 2);
        // broken.ts keeps its fragment and records its parse errors.
        assert_eq!(t.files_parse_errors, 1);
        assert_eq!(t.parse_errors, 2);
        assert_eq!(t.parse_error_rate, Some(0.166667));

        let groups: Vec<(String, String, String, u64)> = r
            .groups
            .iter()
            .map(|g| {
                (
                    g.repo.clone().unwrap(),
                    g.extension.clone().unwrap(),
                    g.language.clone().unwrap(),
                    g.counts.files_indexed,
                )
            })
            .collect();
        let want =
            |a: &str, b: &str, c: &str, n: u64| (a.to_string(), b.to_string(), c.to_string(), n);
        assert_eq!(
            groups,
            [
                want("alpha", "md", "md", 1),
                want("alpha", "test", "test", 2),
                want("alpha", "ts", "javascript", 1),
                want("alpha", "ts", "typescript", 1),
                want("beta", "test", "test", 1),
            ]
        );
        let ts = r
            .by_language
            .iter()
            .find(|g| g.language.as_deref() == Some("typescript"))
            .unwrap();
        assert_eq!(ts.counts.parse_error_rate, Some(1.0));
        let js = r
            .by_language
            .iter()
            .find(|g| g.language.as_deref() == Some("javascript"))
            .unwrap();
        assert_eq!(js.counts.files_parse_errors, 0);
        let ext: Vec<&str> = r
            .by_extension
            .iter()
            .map(|g| g.extension.as_deref().unwrap())
            .collect();
        assert_eq!(ext, ["md", "test", "ts"]);
        let repos: Vec<&str> = r
            .by_repo
            .iter()
            .map(|g| g.repo.as_deref().unwrap())
            .collect();
        assert_eq!(repos, ["alpha", "beta"]);
        let kinds: Vec<(&str, u64, u64, u64, u64)> = r
            .by_link_kind
            .iter()
            .map(|k| {
                (
                    k.kind.as_str(),
                    k.total,
                    k.resolved,
                    k.inferred,
                    k.unresolved,
                )
            })
            .collect();
        assert_eq!(kinds, [("calls", 3, 1, 1, 1), ("links-to", 1, 0, 0, 1)]);

        // One repository.
        let (_, only) = report(&repo, &["--repo", "beta"]);
        assert_eq!(only.totals.files_indexed, 1);
        assert_eq!(only.totals.contracts_dangling, 1);
        assert_eq!(only.by_repo.len(), 1);
        // A repository the project does not have is a usage error.
        let (code, _, err) = run(&repo, &["coverage", "--repo", "nope"]);
        assert_eq!(code, 2, "{err}");

        // The text report.
        let text = ok(&repo, &["coverage"]);
        assert!(
            text.starts_with(
                "coverage  files 6 indexed, 1 parse errors (16.7%), 0 unsupported, 0 failed, 4 unclaimed\n\
                 refs      4: 25.0% resolved, 25.0% inferred, 50.0% unresolved; 2 dangling contracts\n"
            ),
            "{text}"
        );
    }

    #[test]
    fn unclaimed_suffixes_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = mixed(tmp.path());
        let (_, r) = report(&repo, &[]);
        // The logo, the lock file and the repository marker are claimed by nothing.
        let alpha = &r.unclaimed[0];
        assert_eq!(alpha.repo, "alpha");
        assert_eq!(alpha.files, 3);
        let sfx: Vec<(&str, u64)> = alpha
            .suffixes
            .iter()
            .map(|s| (s.suffix.as_str(), s.files))
            .collect();
        assert_eq!(sfx, [(".katalogr", 1), (".lock", 1), (".svg", 1)]);
        assert_eq!(r.unclaimed[1].repo, "beta");
        assert_eq!(r.totals.files_unclaimed, 4);
        let (_, capped) = report(&repo, &["--suffixes", "1"]);
        assert_eq!(capped.unclaimed[0].suffixes.len(), 1);
        assert_eq!(capped.unclaimed[0].files, 3);
        let text = ok(&repo, &["coverage"]);
        assert!(
            text.contains("unclaimed     alpha  3 files: .katalogr 1, .lock 1, .svg 1"),
            "{text}"
        );

        // A repository whose root is unknown here is listed, not walked, and does not fail.
        std::fs::remove_file(tmp.path().join("kx/projects/demo/.cache/roots.yaml")).unwrap();
        let (_, missing) = report(&repo, &[]);
        assert_eq!(missing.roots_missing, ["beta"]);
        assert_eq!(missing.unclaimed.len(), 1);
        assert_eq!(missing.totals.files_indexed, 6);
        let text = ok(&repo, &["coverage"]);
        assert!(text.contains("roots missing beta"), "{text}");
    }

    /// The names a schema object lists under `properties`.
    fn properties(schema: &Value, def: Option<&str>) -> Vec<String> {
        let node = match def {
            Some(d) => &schema["$defs"][d],
            None => schema,
        };
        let mut v: Vec<String> = node["properties"]
            .as_object()
            .unwrap_or_else(|| panic!("no properties in {def:?}"))
            .keys()
            .cloned()
            .collect();
        v.sort();
        v
    }

    fn keys(v: &Value) -> Vec<String> {
        let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        k.sort();
        k
    }

    #[test]
    fn json_matches_schema() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = mixed(tmp.path());
        let (v, _) = report(&repo, &[]);
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1/coverage.schema.json"),
            )
            .unwrap(),
        )
        .unwrap();
        // The envelope and the report carry exactly the fields the schema lists (the optional
        // ones may be absent).
        let env = properties(&schema, None);
        for k in keys(&v) {
            assert!(env.contains(&k), "envelope field {k} not in the schema");
        }
        let result = &v["result"];
        let top = keys(result);
        assert_eq!(
            top,
            [
                "by_extension",
                "by_language",
                "by_link_kind",
                "by_repo",
                "dangling_contracts_by_kind",
                "groups",
                "metrics",
                "roots_missing",
                "totals",
                "unclaimed"
            ]
        );
        assert!(!top.is_empty());
        let report_def = properties(&schema, Some("CoverageReport"));
        assert_eq!(top, report_def);
        assert_eq!(
            keys(&result["totals"]),
            properties(&schema, Some("CoverageCounts"))
        );
        assert_eq!(
            keys(&result["groups"][0]),
            properties(&schema, Some("CoverageRow"))
        );
        assert_eq!(
            keys(&result["unclaimed"][0]),
            properties(&schema, Some("UnclaimedRow"))
        );
        let metric = &result["metrics"][0];
        for k in keys(metric) {
            assert!(
                properties(&schema, Some("MetricSample")).contains(&k),
                "metric field {k} not in the schema"
            );
        }
        // Samples carry the metric names of the design.
        let names: std::collections::BTreeSet<&str> = result["metrics"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["name"].as_str().unwrap())
            .collect();
        for n in [
            "coverage.files.indexed",
            "coverage.files.unclaimed",
            "coverage.files.unsupported",
            "coverage.files.failed",
            "coverage.parse_error_rate",
            "coverage.refs.total",
            "coverage.resolution_rate",
            "coverage.inferred_rate",
            "coverage.unresolved_rate",
            "coverage.contracts.dangling",
        ] {
            assert!(names.contains(n), "{n} missing from {names:?}");
        }
    }

    #[test]
    fn output_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = mixed(tmp.path());
        let first = ok(&repo, &["--json", "coverage"]);
        let second = ok(&repo, &["--json", "coverage"]);
        assert_eq!(first, second);
        assert_eq!(ok(&repo, &["coverage"]), ok(&repo, &["coverage"]));
    }
}
