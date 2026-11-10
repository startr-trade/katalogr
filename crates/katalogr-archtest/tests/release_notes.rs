//! Every release's notes open its GitHub release page and are listed on the documentation site,
//! and a version cannot be tagged without them (FEAT-114).

mod release {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read(rel: &str) -> String {
        fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    /// `vX.Y.Z` notes files on disk, newest first.
    fn versions() -> Vec<String> {
        let mut v: Vec<(Vec<u64>, String)> = fs::read_dir(root().join("docs/src/releases"))
            .expect("docs/src/releases")
            .filter_map(|e| {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                let stem = name.strip_suffix(".md")?.strip_prefix('v')?.to_string();
                let parts: Option<Vec<u64>> = stem.split('.').map(|p| p.parse().ok()).collect();
                Some((parts?, format!("v{stem}")))
            })
            .collect();
        v.sort();
        v.reverse();
        v.into_iter().map(|(_, s)| s).collect()
    }

    fn python() -> Option<&'static str> {
        ["python3", "python"].into_iter().find(|p| {
            Command::new(p)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
    }

    /// R-1. The release is created with the notes as its body, built by the body script, which
    /// drops the title and makes book links absolute while keeping other links.
    #[test]
    fn notes_are_release_body() {
        let wf = read(".github/workflows/release.yml");
        let lines: Vec<&str> = wf.lines().collect();
        let at = |n: &str| {
            lines
                .iter()
                .position(|l| l.contains(n))
                .unwrap_or_else(|| panic!("release.yml has no `{n}`"))
        };
        let body = at("scripts/release-notes-body.py");
        let create = at("gh release create");
        assert!(
            body < create,
            "the body is built before the release is created"
        );
        assert!(
            wf.contains("--notes-file \"$RUNNER_TEMP/notes.md\""),
            "the release uses the notes as its body"
        );
        assert!(
            !lines[create].contains("--draft"),
            "the release is published once every gate has passed, not left as a draft"
        );

        let Some(py) = python() else {
            eprintln!("no python on PATH: the body script's output is not checked here");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("v9.9.9.md");
        let out = dir.path().join("out.md");
        fs::write(
            &src,
            "# katalogr v9.9.9\n\nSee [v9.9.8](v9.9.8.md), [install](../install.md#curl), \
             [GHSA](https://github.com/advisories/x) and [below](#limits).\n",
        )
        .unwrap();
        let st = Command::new(py)
            .arg(root().join("scripts/release-notes-body.py"))
            .arg(&src)
            .arg(&out)
            .status()
            .unwrap();
        assert!(st.success());
        let got = fs::read_to_string(&out).unwrap();
        assert!(!got.contains("# katalogr v9.9.9"), "title dropped: {got}");
        for want in [
            "[v9.9.8](https://katalogr.startr.trade/releases/v9.9.8.html)",
            "[install](https://katalogr.startr.trade/install.html#curl)",
            "[GHSA](https://github.com/advisories/x)",
            "[below](#limits)",
        ] {
            assert!(got.contains(want), "missing `{want}` in:\n{got}");
        }
    }

    /// R-2. The tag's notes are checked before anything is built, and CI checks the workspace
    /// version's notes; the check refuses a missing file or a missing listing.
    #[test]
    fn notes_required() {
        let wf = read(".github/workflows/release.yml");
        let lines: Vec<&str> = wf.lines().collect();
        let check = lines
            .iter()
            .position(|l| l.contains("scripts/release-notes-check.sh \"$GITHUB_REF_NAME\""))
            .expect("release.yml checks the tag's notes");
        let first_build = lines
            .iter()
            .position(|l| l.contains("needs: verify-tag"))
            .expect("jobs wait for verify-tag");
        assert!(check < first_build, "the notes check is in verify-tag");
        let ci = read(".github/workflows/ci.yml");
        assert!(
            ci.contains("sh scripts/release-notes-check.sh \"v$v\""),
            "ci.yml checks the workspace version's notes"
        );

        let script = read("scripts/release-notes-check.sh");
        for want in [
            "docs/src/releases/${tag}.md",
            "docs/src/releases/index.md",
            "docs/src/SUMMARY.md",
            "exit \"$fail\"",
        ] {
            assert!(script.contains(want), "check script lacks `{want}`");
        }

        let cargo = read("Cargo.toml");
        let v = cargo
            .lines()
            .find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"'))
            .expect("workspace version");
        assert!(
            versions().contains(&format!("v{v}")),
            "no release notes for the workspace version v{v}"
        );
    }

    /// R-3. Every notes file is listed in the index and in SUMMARY.md, newest first.
    #[test]
    fn notes_in_book() {
        let index = read("docs/src/releases/index.md");
        let summary = read("docs/src/SUMMARY.md");
        assert!(summary.contains("[Release notes](releases/index.md)"));
        let vs = versions();
        assert!(!vs.is_empty());
        for (text, prefix, name) in [
            (&index, "", "index.md"),
            (&summary, "releases/", "SUMMARY.md"),
        ] {
            let pos: Vec<usize> = vs
                .iter()
                .map(|v| {
                    text.find(&format!("({prefix}{v}.md)"))
                        .unwrap_or_else(|| panic!("{name} does not list {v}"))
                })
                .collect();
            assert!(
                pos.windows(2).all(|w| w[0] < w[1]),
                "{name} lists releases newest first"
            );
        }
    }
}
