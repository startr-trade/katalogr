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
}
