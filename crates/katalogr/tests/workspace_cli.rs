//! FEAT-020 T9: `katalogr workspace`, through the CLI. The projects are synthetic `.tx` repos.

#[path = "views_support/mod.rs"]
mod support;

mod workspace_cli {
    use super::support::*;
    use serde_json::Value;
    use std::path::{Path, PathBuf};

    /// Two projects in one kindex, `orders` (consumer) and `billing` (provider), and a second
    /// kindex `kx2` with project `stock`. Returns (kx, kx2, orders repo).
    fn world(tmp: &Path) -> (PathBuf, PathBuf, PathBuf) {
        let kx = tmp.join("kx");
        let kx2 = tmp.join("kx2");
        ok(tmp, &["kindex", "init", kx.to_str().unwrap()]);
        ok(tmp, &["kindex", "init", kx2.to_str().unwrap()]);
        let mut repos = Vec::new();
        for (k, project, text) in [
            (&kx, "orders", "sym fn call\nbind call consumes svc:load\n"),
            (
                &kx,
                "billing",
                "sym fn serve\nbind serve provides svc:load\n",
            ),
            (
                &kx2,
                "stock",
                "sym fn other\nbind other provides svc:stock\n",
            ),
        ] {
            let repo = tmp.join(format!("{project}-repo"));
            std::fs::create_dir_all(&repo).unwrap();
            std::fs::write(repo.join("a.tx"), text).unwrap();
            git(&repo, &["init", "-q", "-b", "main"]);
            git(&repo, &["config", "user.name", "Test"]);
            git(&repo, &["config", "user.email", "test@example.com"]);
            git(&repo, &["config", "commit.gpgsign", "false"]);
            git(&repo, &["add", "-A"]);
            git(&repo, &["commit", "-q", "-m", "c1"]);
            ok(
                tmp,
                &[
                    "--kindex",
                    k.to_str().unwrap(),
                    "project",
                    "init",
                    project,
                    "--repo",
                    &format!("r={}", repo.display()),
                    "--mark",
                ],
            );
            ok(&repo, &["index"]);
            repos.push(repo);
        }
        (kx, kx2, repos[0].clone())
    }

    fn json(dir: &Path, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        serde_json::from_str(&ok(dir, &all)).expect("JSON")
    }

    #[test]
    fn add_writes_no_path() {
        let tmp = tempfile::tempdir().unwrap();
        let (kx, kx2, orders) = world(tmp.path());
        ok(&orders, &["workspace", "add", "billing"]);
        ok(&orders, &["workspace", "add", "stock", "--alias", "shared"]);
        ok(
            &orders,
            &["workspace", "locate", "shared", kx2.to_str().unwrap()],
        );
        let cfg = std::fs::read_to_string(kx.join("projects/orders/config.yaml")).unwrap();
        assert!(
            cfg.contains("workspace.peers") && cfg.contains("billing"),
            "{cfg}"
        );
        assert!(cfg.contains("shared"), "{cfg}");
        // No machine path anywhere in the kindex.
        let tmp_s = tmp.path().to_str().unwrap();
        for entry in walk(&kx) {
            if entry
                .components()
                .any(|c| c.as_os_str() == ".git" || c.as_os_str() == ".cache")
            {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&entry) {
                assert!(
                    !text.contains(tmp_s),
                    "{} holds a machine path",
                    entry.display()
                );
            }
        }
        // Naming itself, or a path as an alias, is a usage error.
        let own = cli(&orders, &["workspace", "add", "orders"]);
        assert_eq!(own.status.code(), Some(2), "{own:?}");
        let bad = cli(&orders, &["workspace", "add", "stock", "--alias", "../x"]);
        assert_eq!(bad.status.code(), Some(2), "{bad:?}");
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn status_lists_each_peer() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, kx2, orders) = world(tmp.path());
        ok(&orders, &["workspace", "add", "billing"]);
        ok(&orders, &["workspace", "add", "ghost"]);
        ok(&orders, &["workspace", "add", "stock", "--alias", "shared"]);
        // `shared` is not located yet; then it is.
        let v = json(&orders, &["workspace", "status"]);
        let states: Vec<(String, String)> = v["peers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["project"].to_string(),
                    p["state"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        assert_eq!(states.len(), 3, "{v}");
        let state = |p: &str| {
            states
                .iter()
                .find(|(n, _)| n == &format!("\"{p}\""))
                .unwrap()
                .1
                .clone()
        };
        assert_eq!(state("billing"), "ok");
        assert_eq!(state("ghost"), "not-found");
        assert_eq!(state("stock"), "no-location");
        let text = ok(&orders, &["workspace", "status"]);
        assert!(
            text.contains("katalogr workspace locate shared <dir>"),
            "{text}"
        );
        ok(
            &orders,
            &["workspace", "locate", "shared", kx2.to_str().unwrap()],
        );
        let v = json(&orders, &["workspace", "status"]);
        let stock = v["peers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["project"] == "stock")
            .unwrap();
        assert_eq!(stock["state"], "ok");
        assert_eq!(stock["kindex"], "shared");
    }

    #[test]
    fn map_json_validates() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kx, _kx2, orders) = world(tmp.path());
        ok(&orders, &["workspace", "add", "billing"]);
        let out = ok(&orders, &["--json", "workspace", "map"]);
        let v: Value = serde_json::from_str(&out).unwrap();
        let schema: Value = serde_json::from_str(
            &std::fs::read_to_string(schema_dir().join("workspace-map.schema.json")).unwrap(),
        )
        .unwrap();
        validate(&schema, &schema, &v, "$").unwrap();
        assert_eq!(
            v["result"]["projects"],
            serde_json::json!(["billing", "orders"])
        );
        assert_eq!(v["result"]["edges"][0]["consumer"], "orders");
        assert_eq!(v["result"]["edges"][0]["provider"], "billing");
        assert_eq!(v["workspace"]["peers"][0]["state"], "ok");
        // The status shape validates against its own schema.
        let st = json(&orders, &["workspace", "status"]);
        let sschema: Value = serde_json::from_str(
            &std::fs::read_to_string(schema_dir().join("workspace-status.schema.json")).unwrap(),
        )
        .unwrap();
        validate(&sschema, &sschema, &st, "$").unwrap();
    }
}
