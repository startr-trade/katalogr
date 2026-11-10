//! One toolchain everywhere, a release that waits for CI, and the Windows and macOS compile check
//! in `make ci` (FEAT-109).

mod ci {
    use std::fs;
    use std::path::{Path, PathBuf};

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read(rel: &str) -> String {
        fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    fn is_exact_version(v: &str) -> bool {
        let parts: Vec<&str> = v.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    }

    fn workflows() -> Vec<(String, String)> {
        let dir = root().join(".github/workflows");
        let mut out = Vec::new();
        for e in fs::read_dir(&dir).expect("workflows dir") {
            let p = e.unwrap().path();
            if p.extension().is_some_and(|x| x == "yml" || x == "yaml") {
                let name = p.file_name().unwrap().to_string_lossy().into_owned();
                out.push((name, fs::read_to_string(&p).unwrap()));
            }
        }
        out.sort();
        out
    }

    /// R-2. `rust-toolchain.toml` names an exact x.y.z, and every workflow step that installs Rust
    /// installs that same version, so local and CI clippy agree.
    #[test]
    fn toolchain_pinned() {
        let doc: toml::Table = toml::from_str(&read("rust-toolchain.toml")).expect("toml");
        let channel = doc["toolchain"]["channel"]
            .as_str()
            .expect("toolchain.channel");
        assert!(
            is_exact_version(channel),
            "rust-toolchain.toml channel `{channel}` is not an exact x.y.z"
        );

        let mut bad = Vec::new();
        let mut installs = 0;
        for (name, text) in workflows() {
            let lines: Vec<&str> = text.lines().collect();
            for (i, l) in lines.iter().enumerate() {
                if l.contains("rustup toolchain install")
                    || l.contains("rustup default")
                    || l.contains("rustup override")
                {
                    bad.push(format!("{name}:{}: installs Rust by hand", i + 1));
                }
                if !l.contains("dtolnay/rust-toolchain@") {
                    continue;
                }
                installs += 1;
                // The step's `with:` block runs until the next list item.
                let found = lines[i + 1..]
                    .iter()
                    .take_while(|n| !n.trim_start().starts_with("- "))
                    .find_map(|n| n.trim().strip_prefix("toolchain:"))
                    .map(|v| v.split('#').next().unwrap_or("").trim().trim_matches('"'));
                match found {
                    Some(v) if v == channel => {}
                    Some(v) => bad.push(format!(
                        "{name}:{}: installs `{v}`, rust-toolchain.toml pins `{channel}`",
                        i + 1
                    )),
                    None => bad.push(format!("{name}:{}: no `toolchain:` input", i + 1)),
                }
            }
        }
        assert!(
            installs > 0,
            "no workflow installs Rust: the check is blind"
        );
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    /// R-3. The release workflow runs the CI gate in the job every other job waits for, before any
    /// toolchain is installed or cargo is run, and the script refuses anything but a success.
    #[test]
    fn release_waits_for_ci() {
        let text = read(".github/workflows/release.yml");
        let lines: Vec<&str> = text.lines().collect();
        let at = |needle: &str| lines.iter().position(|l| l.contains(needle));
        let gate = at("scripts/release-ci-gate.sh").expect("release.yml runs the gate script");
        assert!(
            lines[gate].trim_start().starts_with("run:"),
            "the gate is a run step"
        );
        for later in [
            "dtolnay/rust-toolchain@",
            "cargo build",
            "gh release create",
        ] {
            let n = at(later).unwrap_or_else(|| panic!("release.yml has no `{later}`"));
            assert!(gate < n, "the gate must come before `{later}`");
        }
        let job = lines[..gate]
            .iter()
            .rposition(|l| {
                l.starts_with("  ") && !l.starts_with("   ") && l.trim_end().ends_with(':')
            })
            .expect("the gate sits in a job");
        let job_name = lines[job].trim().trim_end_matches(':').to_string();
        assert!(
            text.contains(&format!("needs: {job_name}")),
            "no job needs `{job_name}`, so the gate would not hold the build back"
        );
        let block = &lines[job..gate];
        assert!(
            block.iter().any(|l| l.trim() == "actions: read"),
            "the gate's job needs `actions: read`"
        );
        assert!(
            text.contains("GITHUB_SHA: ${{ github.sha }}"),
            "GITHUB_SHA must be passed explicitly"
        );
        assert!(
            text.contains("GH_TOKEN: ${{ github.token }}"),
            "GH_TOKEN must be passed"
        );

        let script = read("scripts/release-ci-gate.sh");
        assert!(
            script.contains("conclusion\" = success"),
            "script checks for success"
        );
        assert!(script.contains("exit 1"), "script can refuse");
        assert!(script.contains("KATALOGR_CI_WAIT_TRIES"));
        assert!(script.contains("KATALOGR_CI_WAIT_SECONDS"));
        assert!(script.contains("workflows/ci.yml/runs"));

        #[cfg(unix)]
        exercise_gate();
    }

    /// Runs the real script against a stand-in `gh` that prints one canned answer per call.
    #[cfg(unix)]
    fn exercise_gate() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("release-ci-gate");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let gh = dir.join("gh");
        fs::write(
            &gh,
            "#!/bin/sh\nd=\"$(dirname \"$0\")\"\nn=$(cat \"$d/count\" 2>/dev/null || echo 0)\n\
             n=$((n + 1))\necho \"$n\" > \"$d/count\"\nsed -n \"${n}p\" \"$d/answers\"\n",
        )
        .unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        let script = root().join("scripts/release-ci-gate.sh");

        let run = |answers: &str| -> bool {
            fs::write(dir.join("answers"), answers).unwrap();
            let _ = fs::remove_file(dir.join("count"));
            Command::new("sh")
                .arg(&script)
                .env("GH", &gh)
                .env("GITHUB_REPOSITORY", "o/r")
                .env("GITHUB_SHA", "abc")
                .env("KATALOGR_CI_WAIT_TRIES", "3")
                .env("KATALOGR_CI_WAIT_SECONDS", "0")
                .output()
                .unwrap()
                .status
                .success()
        };
        let row = |s: &str, c: &str| format!("{s}\t{c}\thttps://example.invalid/run\n");

        assert!(run(&row("completed", "success")), "success proceeds");
        assert!(!run(&row("completed", "failure")), "a failed run refuses");
        assert!(
            !run(&row("completed", "cancelled")),
            "a cancelled run refuses"
        );
        assert!(!run(""), "no run at all refuses");
        assert!(
            run(&(row("in_progress", "") + &row("completed", "success"))),
            "a run in progress is waited for"
        );
        assert!(
            !run(&(row("in_progress", "") + &row("completed", "failure"))),
            "a run that fails after waiting refuses"
        );
        assert!(
            !run(&row("in_progress", "").repeat(3)),
            "a run that never finishes refuses after the tries"
        );
    }

    /// R-1. `make cross` runs clippy over every test for Windows and macOS, with and without the
    /// default features, and `make ci` runs it.
    #[test]
    fn windows_and_macos_compile() {
        let text = read("Makefile");
        let targets = text
            .lines()
            .find_map(|l| l.strip_prefix("TARGETS :="))
            .expect("Makefile defines TARGETS");
        for t in ["x86_64-pc-windows-msvc", "aarch64-apple-darwin"] {
            assert!(targets.split_whitespace().any(|x| x == t), "`{t}` missing");
        }
        let lines: Vec<&str> = text.lines().collect();
        let start = lines
            .iter()
            .position(|l| l.starts_with("cross:"))
            .expect("Makefile has a cross target");
        let recipe: Vec<&str> = lines[start + 1..]
            .iter()
            .take_while(|l| l.starts_with('\t'))
            .copied()
            .collect();
        let recipe = recipe.join("\n");
        assert!(recipe.contains("$(TARGETS)"), "cross loops over TARGETS");
        for want in [
            "cargo clippy --workspace --all-targets --locked",
            "cargo clippy -p katalogr --no-default-features --all-targets --locked",
        ] {
            assert!(
                recipe.matches(want).count() == 1 && recipe.contains("--target $$t"),
                "cross lacks `{want} ... --target`"
            );
        }
        assert!(
            lines
                .iter()
                .any(|l| l.trim() == "@$(MAKE) --no-print-directory cross"),
            "`make ci` does not run cross"
        );
    }
}
