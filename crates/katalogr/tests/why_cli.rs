//! FEAT-087: `why` answers who uses the symbol, names the owner of each section, and agrees with
//! git. Synthetic repositories with the `.tx` extractor, run through the built binary.

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
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

fn katalogr(dir: &Path, args: &[&str]) -> (i32, String, String) {
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

fn ok(dir: &Path, args: &[&str]) -> String {
    let (code, out, err) = katalogr(dir, args);
    assert_eq!(code, 0, "{args:?} failed: {err}");
    out
}

/// Run git with a fixed identity and fixed dates, so commit ids repeat from run to run.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .args([
            "-c",
            "user.name=Example Author",
            "-c",
            "user.email=a@example.test",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn project(tmp: &Path, repos: &[(&str, &Path)]) {
    let kx = tmp.join("kx");
    ok(tmp, &["kindex", "init", kx.to_str().unwrap()]);
    let mut args: Vec<String> = vec![
        "--kindex".into(),
        kx.to_str().unwrap().into(),
        "project".into(),
        "init".into(),
        "demo".into(),
        "--mark".into(),
    ];
    for (n, p) in repos {
        args.push("--repo".into());
        args.push(format!("{n}={}", p.display()));
    }
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    ok(tmp, &refs);
}

/// `alpha` (line 1) has a caller, a test, a contract and a note.
const A_TX: &str = "sym fn alpha\nsym fn caller_one\nsym fn check_alpha\n\
fact check_alpha test case\nlink caller_one calls alpha\nlink check_alpha calls alpha\n\
bind alpha provides topic:orders\nnote alpha WHY the order matters\n";

/// A git repository `code` with `src/a.tx` added by one commit that names a requirement, and a
/// second commit that appends a line and names an item the board lacks. With `board`, a repo
/// holding a copy of the fixture board is part of the project. Indexed, including history.
fn setup(tmp: &Path, board: bool) -> PathBuf {
    let code = tmp.join("code");
    std::fs::create_dir_all(code.join("src")).unwrap();
    git(&code, &["init", "-q"]);
    std::fs::write(code.join("src/a.tx"), A_TX).unwrap();
    git(&code, &["add", "src"]);
    git(
        &code,
        &[
            "commit",
            "-q",
            "-m",
            "feat: add alpha\n\nRefs: kanbanr:FEAT-001/R-1",
        ],
    );
    std::fs::write(code.join("src/a.tx"), format!("{A_TX}sym fn gamma\n")).unwrap();
    git(&code, &["add", "src"]);
    git(
        &code,
        &[
            "commit",
            "-q",
            "-m",
            "feat: add gamma\n\nRefs: kanbanr:FEAT-099",
        ],
    );
    if board {
        let b = tmp.join("board");
        copy_dir(&fixtures().join("kanbanr/board"), &b);
        std::fs::remove_file(b.join("expected.yaml")).unwrap();
        project(tmp, &[("code", &code), ("board", &b)]);
    } else {
        project(tmp, &[("code", &code)]);
    }
    ok(&code, &["index", "--full"]);
    ok(&code, &["index", "--history"]);
    code
}

fn json(dir: &Path, args: &[&str]) -> serde_json::Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(args);
    serde_json::from_str(&ok(dir, &all)).expect("json answer")
}

mod why_cli {
    use super::*;

    #[test]
    fn no_use_omits_section() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), false);
        let with = json(&code, &["why", "src/a.tx:1"]);
        assert!(with["result"]["used_by"].is_object(), "{with}");
        let without = json(&code, &["why", "src/a.tx:1", "--no-use"]);
        assert!(without["result"].get("used_by").is_none(), "{without}");
        let sections: Vec<&str> = without["result"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["section"].as_str().unwrap())
            .collect();
        assert!(!sections.contains(&"used_by"), "{sections:?}");
        let text = ok(&code, &["why", "src/a.tx:1", "--no-use"]);
        assert!(!text.contains("  caller  "), "{text}");
        assert!(!text.contains("  test  "), "{text}");
    }

    #[test]
    fn text_sections_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), true);
        let out = ok(&code, &["why", "src/a.tx:1"]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "fn  code:src/a.tx#fn:alpha  src/a.tx:1", "{out}");
        let at = |needle: &str| {
            lines
                .iter()
                .position(|l| l.starts_with(needle))
                .unwrap_or_else(|| panic!("{needle:?} not in {out}"))
        };
        let caller = at("  caller  ");
        let test = at("  test  ");
        let contract = at("  contract  contract:topic:orders");
        let note = at("  WHY  line 8  the order matters");
        let commit = at("  commit  ");
        let item = at("  changed-by  FEAT-001/R-1");
        let goal = at("  serves  G-1");
        assert!(
            caller < test && test < contract && contract < note && note < commit,
            "{out}"
        );
        assert!(commit < item && item < goal, "{out}");
    }

    #[test]
    fn board_items_labelled_index() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), true);
        let v = json(&code, &["why", "src/a.tx:1"]);
        let src: Vec<(&str, &str, &str)> = v["result"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                (
                    s["section"].as_str().unwrap(),
                    s["owner"].as_str().unwrap(),
                    s["source"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(
            src,
            [
                ("symbol", "index", "index"),
                ("used_by", "index", "index"),
                ("notes", "index", "index"),
                ("commits", "both", "index"),
                ("rationale", "board", "index"),
                ("goals", "board", "index"),
            ]
        );
        assert!(
            v["result"]["rationale"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["label"] == "FEAT-001/R-1"),
            "{v}"
        );
    }

    #[test]
    fn hint_only_when_item_reached() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), true);
        // Line 1 was added by a commit that names a board requirement.
        let out = ok(&code, &["why", "src/a.tx:1"]);
        let next = out.lines().last().unwrap();
        assert!(next.starts_with("next: "), "{out}");
        assert!(next.contains("kanbanr why src/a.tx:1"), "{out}");
        let v = json(&code, &["why", "src/a.tx:1"]);
        assert!(
            v["next"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n == "kanbanr why src/a.tx:1"),
            "{v}"
        );
        // Line 9 was added by a commit whose item the board lacks: no item is reached.
        let out = ok(&code, &["why", "src/a.tx:9"]);
        assert!(!out.contains("kanbanr"), "{out}");
        let v = json(&code, &["why", "src/a.tx:9"]);
        assert!(
            !v["next"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n.as_str().unwrap().contains("kanbanr")),
            "{v}"
        );
    }

    #[test]
    fn works_with_no_board() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), false);
        let v = json(&code, &["why", "src/a.tx:1"]);
        assert_eq!(v["result"]["symbol"]["name"], "code:src/a.tx#fn:alpha");
        assert_eq!(v["result"]["used_by"]["callers_total"], 2, "{v}");
        assert!(
            v["result"]["rationale"].as_array().unwrap().is_empty(),
            "{v}"
        );
        // The commit's item is not on any indexed board, so no goal and no board follow-up.
        assert!(v["result"]["goals"].as_array().unwrap().is_empty(), "{v}");
        let text = ok(&code, &["why", "src/a.tx:1"]);
        assert!(!text.contains("serves"), "{text}");
        assert!(text.contains("  caller  "), "{text}");
        assert!(text.contains("  contract  contract:topic:orders"), "{text}");
    }

    #[test]
    fn commit_agrees_with_git_blame() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), false);
        for line in [1u32, 9] {
            let blame = git(
                &code,
                &[
                    "blame",
                    "--porcelain",
                    "-L",
                    &format!("{line},{line}"),
                    "src/a.tx",
                ],
            );
            let sha = blame.split_whitespace().next().unwrap().to_string();
            let v = json(&code, &["why", &format!("src/a.tx:{line}")]);
            let commits = v["result"]["commits"].as_array().unwrap();
            assert_eq!(commits.len(), 1, "{v}");
            assert_eq!(commits[0]["sha"], sha.as_str(), "line {line}: {v}");
        }
    }

    #[test]
    fn uncommitted_line_has_symbol_no_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), false);
        let mut text = std::fs::read_to_string(code.join("src/a.tx")).unwrap();
        text.push_str("sym fn fresh\n");
        std::fs::write(code.join("src/a.tx"), text).unwrap();
        ok(&code, &["index"]);
        let v = json(&code, &["why", "src/a.tx:10"]);
        assert_eq!(
            v["result"]["symbol"]["name"], "code:src/a.tx#fn:fresh",
            "{v}"
        );
        assert!(v["result"]["commits"].as_array().unwrap().is_empty(), "{v}");
        let note = v["result"]["note"].as_str().unwrap_or_default();
        assert!(note.starts_with("commit section omitted: "), "{v}");
    }

    /// R-3. Every answer says what it read: positions per repo, and whether that is exact.
    #[test]
    fn answer_carries_position() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), true);
        let head = git(&code, &["rev-parse", "HEAD"]).trim().to_string();
        let v = json(&code, &["why", "src/a.tx:1"]);
        assert_eq!(v["exact"], true, "{v}");
        let positions = v["positions"].as_array().unwrap();
        let repos: Vec<&str> = positions
            .iter()
            .map(|p| p["repo"].as_str().unwrap())
            .collect();
        assert_eq!(repos, ["board", "code"], "{v}");
        let mine = positions.iter().find(|p| p["repo"] == "code").unwrap();
        assert_eq!(mine["commit"], head.as_str());
        // The marker file the project init wrote is untracked, so the checkout is not clean.
        assert!(mine["dirty"].is_string(), "{v}");
        // The board directory is not a git repository: it has no commit.
        let board = positions.iter().find(|p| p["repo"] == "board").unwrap();
        assert!(board["commit"].is_null(), "{v}");
    }

    /// R-3. `--at` the current position answers the same as the live read.
    #[test]
    fn at_position_answers() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), false);
        let live = json(&code, &["why", "src/a.tx:1"]);
        let head = git(&code, &["rev-parse", "HEAD"]).trim().to_string();
        let at = json(&code, &["--at", &head, "why", "src/a.tx:1"]);
        // The position's index holds no history, so only the code facts must agree.
        assert_eq!(at["result"]["symbol"], live["result"]["symbol"]);
        assert_eq!(at["result"]["used_by"], live["result"]["used_by"]);
        // The commit is the same; the dirty part is what the checkout adds to it.
        assert_eq!(at["positions"][0]["commit"], live["positions"][0]["commit"]);
        assert!(at["positions"][0]["dirty"].is_null(), "{at}");
        assert_eq!(at["exact"], true, "{at}");
    }

    /// R-3. A position nobody holds is exit 6, and says what is available.
    #[test]
    fn at_unavailable_exit_6() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), false);
        let nowhere = "0".repeat(40);
        let (c, out, err) = katalogr(&code, &["--json", "--at", &nowhere, "why", "src/a.tx:1"]);
        assert_eq!(c, 6, "{out} {err}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"], "position-unavailable");
        assert_eq!(v["wanted"], nowhere.as_str());
        assert!(!v["available"].as_array().unwrap().is_empty(), "{v}");
        let (c, out, err) = katalogr(&code, &["--at", &nowhere, "why", "src/a.tx:1"]);
        assert_eq!((c, out.as_str()), (6, ""));
        assert!(err.contains("position unavailable"), "{err}");
    }

    /// R-1. `why.use_limit` in the project settings sets how many callers are listed.
    #[test]
    fn use_limit_setting_applies() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), false);
        let cfg = tmp.path().join("kx/projects/demo/config.yaml");
        let text = std::fs::read_to_string(&cfg).unwrap();
        let (head, _) = text.split_once("settings:").expect("settings key");
        std::fs::write(&cfg, format!("{head}settings:\n  why.use_limit: 1\n")).unwrap();
        let v = json(&code, &["why", "src/a.tx:1"]);
        assert_eq!(
            v["result"]["used_by"]["callers"].as_array().unwrap().len(),
            1,
            "{v}"
        );
        assert_eq!(v["result"]["used_by"]["callers_total"], 2);
        assert_eq!(v["partial"][0]["cap"], "limit", "{v}");
    }

    #[test]
    fn golden_contract_file() {
        let tmp = tempfile::tempdir().unwrap();
        let code = setup(tmp.path(), true);
        let got = ok(&code, &["--json", "why", "src/a.tx:1"]);
        let v: serde_json::Value = serde_json::from_str(&got).unwrap();
        let mut want = serde_json::to_string_pretty(&v).unwrap();
        want.push('\n');
        let file = fixtures().join("contract/why-v1.json");
        if std::env::var_os("KATALOGR_BLESS").is_some() {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, &want).unwrap();
        }
        let have =
            std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        assert_eq!(
            have, want,
            "the contract file is out of date: re-run with KATALOGR_BLESS=1 after review"
        );
    }
}
