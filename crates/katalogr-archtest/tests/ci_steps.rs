//! The CI workflow keeps the checks that guard the optional MCP wrapper (FEAT-067).

mod ci {
    use std::path::Path;

    /// R-14. The `clippy` job lints the `katalogr` crate without default features, so a build
    /// without the `mcp` feature (no tokio, no rmcp) cannot rot.
    #[test]
    fn builds_without_mcp_feature() {
        let text = std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows/ci.yml"),
        )
        .expect("ci.yml");
        let step =
            "cargo clippy -p katalogr --no-default-features --all-targets --locked -- -D warnings";
        assert!(
            text.lines().any(|l| l.trim() == format!("run: {step}")),
            "ci.yml has no step `{step}`"
        );
    }

    fn workflows() -> Vec<(String, String)> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/workflows");
        let mut out = Vec::new();
        for e in std::fs::read_dir(dir).expect("workflows dir") {
            let p = e.expect("entry").path();
            if p.extension().is_some_and(|x| x == "yml") {
                let name = p.file_name().expect("name").to_string_lossy().into_owned();
                out.push((name, std::fs::read_to_string(&p).expect("workflow")));
            }
        }
        out
    }

    fn workflow(name: &str) -> String {
        std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../.github/workflows")
                .join(name),
        )
        .expect(name)
    }

    /// The lines of the top-level job `job` (from `  job:` to the next two-space key).
    fn job_lines(text: &str, job: &str) -> Vec<String> {
        let head = format!("  {job}:");
        let mut in_job = false;
        let mut out = Vec::new();
        for l in text.lines() {
            if l == head {
                in_job = true;
                continue;
            }
            if in_job {
                let top = l.starts_with("  ") && !l.starts_with("   ") && !l.trim().is_empty();
                if top || (!l.starts_with(' ') && !l.is_empty()) {
                    break;
                }
                out.push(l.to_string());
            }
        }
        out
    }

    /// The steps of a job, each as its lines (a step starts at `      - `).
    fn steps(lines: &[String]) -> Vec<Vec<String>> {
        let mut out: Vec<Vec<String>> = Vec::new();
        for l in lines {
            if l.starts_with("      - ") {
                out.push(vec![l.clone()]);
            } else if let Some(last) = out.last_mut() {
                last.push(l.clone());
            }
        }
        out
    }

    /// FEAT-113 R-1. The Windows test job keeps the target dir, TMP and TEMP on a trusted Dev Drive.
    #[test]
    fn windows_dev_drive() {
        let text = workflow("ci.yml");
        let lines = job_lines(&text, "test");
        let step = steps(&lines)
            .into_iter()
            .find(|s| {
                s.iter().any(|l| {
                    l.split_once("samypr100/setup-dev-drive@")
                        .is_some_and(|(_, r)| {
                            r.len() >= 40 && r[..40].chars().all(|c| c.is_ascii_hexdigit())
                        })
                })
            })
            .expect("test job has a samypr100/setup-dev-drive@<sha> step");
        let has = |t: &str| step.iter().any(|l| l.trim() == t);
        assert!(
            has("if: runner.os == 'Windows'"),
            "Dev Drive step is not Windows-only"
        );
        assert!(has("trusted-dev-drive: true"), "Dev Drive is not trusted");
        let joined = step.join("\n");
        for var in ["CARGO_TARGET_DIR", "TMP", "TEMP"] {
            assert!(
                step.iter()
                    .any(|l| l.trim().starts_with(&format!("{var},"))),
                "env-mapping lacks {var}: {joined}"
            );
        }
    }

    /// FEAT-113 R-2. No workflow sets `PROPTEST_CASES`, and the test and doctest steps run on every OS.
    #[test]
    fn windows_full_coverage() {
        for (name, text) in workflows() {
            assert!(
                !text.lines().any(|l| l.contains("PROPTEST_CASES")),
                "{name} sets PROPTEST_CASES"
            );
        }
        let text = workflow("ci.yml");
        let lines = job_lines(&text, "test");
        for cmd in [
            "cargo nextest run --workspace --locked --no-fail-fast",
            "cargo test --workspace --doc --locked",
        ] {
            let step = steps(&lines)
                .into_iter()
                .find(|s| s.iter().any(|l| l.trim() == format!("run: {cmd}")))
                .unwrap_or_else(|| panic!("test job has no step `{cmd}`"));
            for l in &step {
                let t = l.trim();
                assert!(
                    !(t.starts_with("if:") && (t.contains("os") || t.contains("OS"))),
                    "`{cmd}` is conditional on the OS: {t}"
                );
            }
        }
    }
}
