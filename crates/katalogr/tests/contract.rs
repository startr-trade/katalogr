//! Contract-mediated linking through the CLI (FEAT-002 R-3, R-4).

mod contract {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn katalogr(dir: &Path, args: &[&str]) -> (i32, String) {
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
        )
    }

    fn ok(dir: &Path, args: &[&str]) -> String {
        let (code, out) = katalogr(dir, args);
        assert_eq!(code, 0, "{args:?}: {out}");
        out
    }

    /// A project `demo` with one marked repo `app`, holding the given `.tx` files, indexed.
    fn project(tmp: &Path, files: &[(&str, &str)]) -> PathBuf {
        let kx = tmp.join("kx");
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        for (name, text) in files {
            std::fs::write(repo.join(name), text).unwrap();
        }
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
        ok(&repo, &["index"]);
        repo
    }

    const CONTRACT: &str = "contract:endpoint:orders";

    const API: &str = "sym fn serve\nbind serve provides endpoint:orders\n";
    const CLIENT: &str = "sym fn fetch\nsym fn main\nbind fetch consumes endpoint:orders\n\
                          link main calls fetch\n";

    #[test]
    fn queries_traverse_contracts() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(tmp.path(), &[("api.tx", API), ("client.tx", CLIENT)]);

        // refs on the contract lists both ends, by role.
        let refs = ok(&repo, &["refs", CONTRACT]);
        assert!(
            refs.contains("provides") && refs.contains("#fn:serve"),
            "{refs}"
        );
        assert!(
            refs.contains("consumes") && refs.contains("#fn:fetch"),
            "{refs}"
        );

        // callers of the provider: the consumer through the contract, then its own caller.
        let callers = ok(&repo, &["callers", "serve", "--depth", "2"]);
        let fetch = callers
            .lines()
            .find(|l| l.contains("#fn:fetch"))
            .expect(&callers);
        assert!(
            fetch.contains("depth 1") && fetch.contains(&format!("via {CONTRACT}")),
            "{fetch}"
        );
        let main = callers
            .lines()
            .find(|l| l.contains("#fn:main"))
            .expect(&callers);
        assert!(main.contains("depth 2") && !main.contains("via"), "{main}");
        let json = ok(&repo, &["--json", "callers", "serve"]);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["result"][0]["via"], CONTRACT, "{json}");

        // path hops provider -> contract -> consumer, naming the contract in each hop.
        let path = ok(&repo, &["path", "serve", "fetch"]);
        let steps: Vec<&str> = path.lines().filter(|l| l.contains(CONTRACT)).collect();
        assert_eq!(steps.len(), 2, "{path}");
        assert!(steps[0].contains("-provides->"), "{path}");
        assert!(steps[1].contains("<-consumes-"), "{path}");

        // A contract no binding names is not a target.
        let (code, _) = katalogr(&repo, &["refs", "contract:endpoint:absent"]);
        assert_ne!(code, 0);
    }

    #[test]
    fn dangling_contracts_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = project(
            tmp.path(),
            &[
                ("api.tx", API),
                ("client.tx", CLIENT),
                (
                    "lonely.tx",
                    "sym fn emit\nsym fn listen\nsym fn spec\n\
                     bind emit provides channel:alerts\n\
                     bind listen consumes queue:jobs\n\
                     bind spec declares endpoint:docs\n",
                ),
            ],
        );
        // Providers without consumers and consumers without providers are reported; matched
        // contracts and declaration-only contracts are not.
        let (code, out) = katalogr(&repo, &["dangling", "--kind", "contract"]);
        assert_eq!(code, 1, "{out}");
        let lines: Vec<&str> = out.lines().filter(|l| l.contains("contract:")).collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert!(
            lines.iter().any(|l| l.starts_with("provides")
                && l.contains("#fn:emit")
                && l.contains("contract:channel:alerts")),
            "{out}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("consumes")
                && l.contains("#fn:listen")
                && l.contains("contract:queue:jobs")),
            "{out}"
        );
        assert!(
            !out.contains(CONTRACT) && !out.contains("endpoint:docs"),
            "{out}"
        );

        // The default kinds do not report contracts.
        let (_, default) = katalogr(&repo, &["dangling"]);
        assert!(!default.contains("contract:"), "{default}");

        // Once the other end appears, the contract is no longer reported.
        std::fs::write(
            repo.join("peer.tx"),
            "sym fn peer\nbind peer consumes channel:alerts\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        let (_, out) = katalogr(&repo, &["dangling", "--kind", "contract"]);
        assert!(!out.contains("channel:alerts"), "{out}");
    }

    const ORDERS: &str = "contract:channel:orders";
    const SERVE: &str = "sym fn serve\nbind serve provides channel:orders\n";
    const HANDLE: &str = "sym fn handle\nbind handle consumes channel:orders\n";

    fn orders_repo(tmp: &Path) -> PathBuf {
        project(tmp, &[("api.tx", SERVE), ("app.tx", HANDLE)])
    }

    #[test]
    fn explain_lists_roles() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = orders_repo(tmp.path());
        let out = ok(&repo, &["explain", ORDERS]);
        assert!(out.contains(&format!("contract  {ORDERS}")), "{out}");
        assert!(
            out.contains("kind: channel") && out.contains("key: orders"),
            "{out}"
        );
        let line = |role: &str, name: &str| {
            let at = out
                .lines()
                .position(|l| l.starts_with(role))
                .unwrap_or_else(|| panic!("{role}: {out}"));
            out.lines()
                .nth(at + 1)
                .filter(|l| l.contains(name) && l.contains(".tx:") && l.contains("[Extracted]"))
                .unwrap_or_else(|| panic!("{role} {name}: {out}"))
                .to_string()
        };
        line("providers (1):", "app:api.tx#fn:serve");
        line("consumers (1):", "app:app.tx#fn:handle");
        assert!(!out.contains("declarers"), "{out}");
        assert!(!out.contains("incoming"), "{out}");
        let next: Vec<&str> = out.lines().filter(|l| l.contains("next")).collect();
        assert!(!out.contains(&format!("show {ORDERS}")), "{next:?}");
    }

    #[test]
    fn explain_json_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = orders_repo(tmp.path());
        let out = ok(&repo, &["--json", "explain", ORDERS]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let c = &v["result"]["contract"];
        assert_eq!(c["kind"], "channel", "{out}");
        assert_eq!(c["key"], "orders", "{out}");
        assert_eq!(c["providers"][0]["name"], "app:api.tx#fn:serve", "{out}");
        assert_eq!(c["providers_total"], 1, "{out}");
        assert_eq!(c["consumers"][0]["name"], "app:app.tx#fn:handle", "{out}");
        assert_eq!(c["consumers"][0]["path"], "app.tx", "{out}");
        assert_eq!(c["declarers_total"], 0, "{out}");
        assert_eq!(v["result"]["symbol"]["kind"], "contract", "{out}");
    }

    #[test]
    fn explain_schema_additive() {
        let v = katalogr_dist::schemas()
            .into_iter()
            .find(|(name, _)| *name == "explain")
            .expect("explain schema")
            .1;
        let text = serde_json::to_string(&v).unwrap();
        assert!(text.contains("\"contract\""), "{text}");
        assert!(text.contains("ContractInfo"), "{text}");
        // No `required` list names it: the field is optional.
        fn required_has_contract(v: &serde_json::Value) -> bool {
            match v {
                serde_json::Value::Object(m) => {
                    m.get("required")
                        .and_then(|r| r.as_array())
                        .is_some_and(|r| r.iter().any(|x| x == "contract"))
                        || m.values().any(required_has_contract)
                }
                serde_json::Value::Array(a) => a.iter().any(required_has_contract),
                _ => false,
            }
        }
        assert!(!required_has_contract(&v), "{text}");
    }

    #[test]
    fn explain_caps_per_side() {
        let tmp = tempfile::tempdir().unwrap();
        let consumers = "sym fn a\nsym fn b\nsym fn c\n\
                         bind c consumes channel:orders\nbind a consumes channel:orders\n\
                         bind b consumes channel:orders\n";
        let repo = project(tmp.path(), &[("api.tx", SERVE), ("users.tx", consumers)]);
        let out = ok(&repo, &["--json", "explain", ORDERS, "--per-side", "1"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let c = &v["result"]["contract"];
        assert_eq!(c["consumers"].as_array().unwrap().len(), 1, "{out}");
        assert_eq!(c["consumers"][0]["name"], "app:users.tx#fn:a", "{out}");
        assert_eq!(c["consumers_total"], 3, "{out}");
        assert!(out.contains("partial"), "{out}");
        let text = ok(&repo, &["explain", ORDERS, "--per-side", "1"]);
        assert!(text.contains("consumers (first 1 of 3):"), "{text}");
        assert!(text.contains("partial:"), "{text}");
    }

    #[test]
    fn explain_unknown_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = orders_repo(tmp.path());
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(&repo)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(["explain", "contract:channel:absent"])
            .output()
            .unwrap();
        let all = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.status.code(), Some(4), "{all}");
        assert!(!all.contains("cache row"), "{all}");
    }

    fn usage(dir: &Path, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_katalogr"))
            .current_dir(dir)
            .env_remove("KATALOGR_KINDEX")
            .env_remove("KATALOGR_PROJECT")
            .args(args)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr).to_string();
        assert_eq!(out.status.code(), Some(2), "{args:?}: {err}");
        err
    }

    #[test]
    fn show_contract_usage_error() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = orders_repo(tmp.path());
        let err = usage(&repo, &["show", ORDERS]);
        assert!(err.contains("no span of its own"), "{err}");
        assert!(err.contains("explain") && err.contains("refs"), "{err}");
    }

    #[test]
    fn replace_contract_usage_error() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = orders_repo(tmp.path());
        let body = tmp.path().join("body.txt");
        std::fs::write(&body, "x\n").unwrap();
        let err = usage(
            &repo,
            &["replace", ORDERS, "--file", body.to_str().unwrap()],
        );
        assert!(err.contains("no span of its own"), "{err}");
        assert!(err.contains("explain") && err.contains("refs"), "{err}");
    }

    #[test]
    fn why_contract_usage_error() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = orders_repo(tmp.path());
        for spec in [ORDERS, "contract:queue:7"] {
            let err = usage(&repo, &["why", spec]);
            assert!(
                err.contains("FILE:LINE") && err.contains("explain"),
                "{err}"
            );
        }
    }

    #[test]
    fn explain_large_fanout() {
        let tmp = tempfile::tempdir().unwrap();
        let mut src = String::new();
        for i in 0..2000 {
            src.push_str(&format!("sym fn f{i}\nbind f{i} consumes channel:orders\n"));
        }
        let repo = project(tmp.path(), &[("api.tx", SERVE), ("many.tx", &src)]);
        let t = std::time::Instant::now();
        let first = ok(&repo, &["--json", "explain", ORDERS]);
        let took = t.elapsed();
        let second = ok(&repo, &["--json", "explain", ORDERS]);
        let v: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(
            v["result"]["contract"]["consumers_total"], 2000,
            "{first:.200}"
        );
        assert_eq!(first, second, "repeated output differs");
        eprintln!("explain_large_fanout: {took:?} (process included)");
        if !cfg!(debug_assertions) {
            assert!(took.as_millis() < 500, "{took:?}");
        }
    }
}
