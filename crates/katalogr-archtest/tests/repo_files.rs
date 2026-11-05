//! Repository policy files, workflow pinning and the docs site (FEAT-100).

mod archtest {
    use std::fs;
    use std::path::{Path, PathBuf};

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read(rel: &str) -> String {
        fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    /// R-1. The files GitHub's rulesets, community profile and Pages expect are present, and the
    /// ones that name people or teams carry no email address.
    #[test]
    fn repo_policy_files_present() {
        for f in [
            ".github/CODEOWNERS",
            ".github/PULL_REQUEST_TEMPLATE.md",
            ".github/codeql/codeql-config.yml",
            ".github/workflows/codeql.yml",
            ".github/workflows/trivy.yml",
            ".github/workflows/docs.yml",
            ".trivyignore.yaml",
            "CONTRIBUTING.md",
            "SECURITY.md",
            "CODE_OF_CONDUCT.md",
            "CONTRIBUTORS",
        ] {
            assert!(root().join(f).is_file(), "missing {f}");
        }
        let owners = read(".github/CODEOWNERS");
        assert!(
            owners
                .lines()
                .any(|l| l.trim() == "* @startr-trade/katalogr-maintainers"),
            "CODEOWNERS must assign * to the maintainers team"
        );
        let contributors = read("CONTRIBUTORS");
        assert!(contributors.contains("katalogr-maintainers"));
        assert!(contributors.contains("vb-startr"));
        for f in [
            "CONTRIBUTORS",
            "SECURITY.md",
            "CODE_OF_CONDUCT.md",
            "CONTRIBUTING.md",
        ] {
            let text = read(f);
            let has_email = text.split_whitespace().any(|w| {
                let w = w.trim_matches(|c: char| !c.is_alphanumeric() && c != '@' && c != '.');
                match w.split_once('@') {
                    Some((l, d)) => !l.is_empty() && d.contains('.'),
                    None => false,
                }
            });
            assert!(!has_email, "{f} contains an email address");
        }
    }

    /// R-2. Every `uses:` in every workflow is pinned to a full 40-character commit SHA (local
    /// `./` actions excepted).
    #[test]
    fn workflow_actions_pinned_by_sha() {
        let dir = root().join(".github/workflows");
        let mut bad = Vec::new();
        let mut seen = 0;
        for entry in fs::read_dir(&dir).expect("workflows dir") {
            let path = entry.expect("entry").path();
            if !matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("yml" | "yaml")
            ) {
                continue;
            }
            let text = fs::read_to_string(&path).expect("workflow");
            for (n, line) in text.lines().enumerate() {
                let t = line.trim_start().trim_start_matches("- ").trim_start();
                let Some(spec) = t.strip_prefix("uses:") else {
                    continue;
                };
                seen += 1;
                let spec = spec.split('#').next().unwrap_or("").trim();
                if spec.starts_with("./") {
                    continue;
                }
                let sha = spec.rsplit_once('@').map(|(_, r)| r).unwrap_or("");
                if sha.len() != 40 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
                    bad.push(format!("{}:{}: {spec}", path.display(), n + 1));
                }
            }
        }
        assert!(seen > 0, "no `uses:` lines found");
        assert!(
            bad.is_empty(),
            "actions not pinned by SHA:\n{}",
            bad.join("\n")
        );
    }

    /// R-3. The docs site is configured: a `book.toml`, a summary, and every chapter the summary
    /// lists exists.
    #[test]
    fn docs_site_builds_config() {
        let book = read("docs/book.toml");
        let parsed: toml::Table = book.parse().expect("book.toml parses");
        assert_eq!(
            parsed["book"]["src"].as_str(),
            Some("src"),
            "book.toml must set src = \"src\""
        );
        let summary = read("docs/src/SUMMARY.md");
        let mut chapters = 0;
        for part in summary.split("](").skip(1) {
            let target = part.split(')').next().unwrap_or("");
            if target.ends_with(".md") {
                chapters += 1;
                assert!(
                    root().join("docs/src").join(target).is_file(),
                    "SUMMARY lists missing chapter {target}"
                );
            }
        }
        assert!(chapters >= 2, "SUMMARY lists {chapters} chapters");
        let deploy = read(".github/workflows/docs.yml");
        assert!(
            deploy.contains("github-pages"),
            "docs.yml must deploy to github-pages"
        );
    }
}
