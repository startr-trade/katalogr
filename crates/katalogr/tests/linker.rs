//! FEAT-019 integration tests: Markdown, board items, commit trailers and annotations, run
//! through the built binary against fixtures copied into temporary directories.

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

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=Example Author",
            "-c",
            "user.email=a@example.test",
        ])
        .args([
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
}

/// A kindex and a project `demo` with the given `(name, dir)` repos, all marked.
fn project(tmp: &Path, repos: &[(&str, &Path)]) -> PathBuf {
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
    kx
}

/// One marked repository holding a copy of a fixture; indexed. Returns the repo directory.
fn single(tmp: &Path, fixture: &str) -> PathBuf {
    let repo = tmp.join("app");
    copy_dir(&fixtures().join(fixture), &repo);
    std::fs::remove_file(repo.join("expected.yaml")).unwrap();
    project(tmp, &[("app", &repo)]);
    ok(&repo, &["index"]);
    repo
}

/// A git repository `code` with a `.tx` file changed by two commits (the first names a
/// requirement of the fixture board, the second an item the board lacks), and a `board` repo
/// holding a copy of the fixture board. Indexed, including history.
fn code_and_board(tmp: &Path) -> PathBuf {
    let code = tmp.join("code");
    let board = tmp.join("board");
    std::fs::create_dir_all(code.join("src")).unwrap();
    copy_dir(&fixtures().join("kanbanr/board"), &board);
    std::fs::remove_file(board.join("expected.yaml")).unwrap();
    git(&code, &["init", "-q"]);
    std::fs::write(code.join("src/a.tx"), "sym fn alpha\nsym fn beta\n").unwrap();
    git(&code, &["add", "src"]);
    git(
        &code,
        &[
            "commit",
            "-q",
            "-m",
            "feat: add alpha and beta\n\nRefs: kanbanr:FEAT-001/R-1, kanbanr:FEAT-002",
        ],
    );
    std::fs::write(
        code.join("src/a.tx"),
        "sym fn alpha\nsym fn beta\nsym fn gamma\n",
    )
    .unwrap();
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
    std::fs::write(code.join("src/b.tx"), "sym fn delta\n").unwrap();
    git(&code, &["add", "src"]);
    git(&code, &["commit", "-q", "-m", "chore: no reference here"]);
    project(tmp, &[("code", &code), ("board", &board)]);
    ok(&code, &["index", "--full"]);
    ok(&code, &["index", "--history"]);
    code
}

/// An empty git repository `code` and a `board` repo holding a copy of the fixture board,
/// both marked in a project; nothing indexed yet.
fn bare_code_and_board(tmp: &Path) -> PathBuf {
    let code = tmp.join("code");
    let board = tmp.join("board");
    std::fs::create_dir_all(code.join("src")).unwrap();
    copy_dir(&fixtures().join("kanbanr/board"), &board);
    std::fs::remove_file(board.join("expected.yaml")).unwrap();
    git(&code, &["init", "-q"]);
    project(tmp, &[("code", &code), ("board", &board)]);
    code
}

mod md {
    use super::*;

    /// Canonical graph of the `md/emails` fixture, indexed in memory.
    fn emails_graph() -> String {
        let case =
            camino::Utf8Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/md/emails");
        katalogr_acceptance::index_case(&katalogr_acceptance::full_registry, &case).unwrap()
    }

    #[test]
    fn emails_are_urls() {
        let graph = emails_graph();
        // The `mailto:` link and the autolink are both recorded as `mailto:` URL facts.
        let artifact = graph
            .lines()
            .find(|l| l.contains(r#""node":"artifact""#) && l.contains("contact.md"))
            .expect("the fixture's artifact node");
        assert!(
            artifact.contains(r#""mailto:someone@example.com""#),
            "{artifact}"
        );
        assert!(
            artifact.contains(r#""mailto:other@example.org""#),
            "{artifact}"
        );
        // Emails create no reference: no link in the graph points at an address.
        assert!(
            !graph
                .lines()
                .any(|l| l.contains(r#""kind":"links-to""#) && l.contains("example")),
            "{graph}"
        );
    }

    #[test]
    fn emails_not_dangling() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = single(tmp.path(), "md/emails");

        // The relative self-link resolves, and the addresses are not reported.
        let (code, out, _) = katalogr(&repo, &["dangling", "--kind", "links-to"]);
        assert_eq!(code, 0, "{out}");
        assert!(!out.contains("example"), "{out}");
    }

    #[test]
    fn sections_and_links() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = single(tmp.path(), "md/sections-links");

        let out = ok(&repo, &["find", "usage"]);
        assert!(
            out.contains(
                "section  app:README.md#section:example-project/section:usage  README.md:6\n"
            ),
            "{out}"
        );
        assert!(
            out.contains("section:usage~2"),
            "duplicate heading keeps an ordinal: {out}"
        );

        // Nested sections; the link in the second `Usage` points at `Options`.
        let out = ok(&repo, &["refs", "options"]);
        assert!(
            out.contains("links-to  app:README.md#section:example-project/section:usage~2"),
            "{out}"
        );
        // A relative link with a fragment reaches the section of the other document.
        let out = ok(&repo, &["refs", "app:guide.md#section:guide/section:setup"]);
        assert!(
            out.contains("links-to  app:README.md#section:example-project/section:usage"),
            "{out}"
        );
        // A link back up the tree resolves to the document.
        let (code, _, _) = katalogr(&repo, &["dangling", "--kind", "links-to"]);
        assert_eq!(code, 0, "every link of the fixture resolves");
    }

    #[test]
    fn dangling_links_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = single(tmp.path(), "md/dangling-links");

        let (code, out, _) = katalogr(&repo, &["dangling", "--kind", "links-to"]);
        assert_eq!(code, 1, "{out}");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert!(
            lines
                .iter()
                .all(|l| l.starts_with("links-to  app:index.md#section:index  index.md:"))
        );
        assert!(out.contains("wants file:app:absent.md\n"), "{out}");
        assert!(
            out.contains("wants md-section:index.md#no-such-section"),
            "{out}"
        );
        assert!(
            out.contains("wants md-section:absent.md#intro | file:app:absent.md"),
            "{out}"
        );

        // Create the missing page: two of the three resolve, and the exit status stays 1.
        std::fs::write(repo.join("absent.md"), "# Absent\n\n## Intro\n").unwrap();
        ok(&repo, &["index"]);
        let (code, out, _) = katalogr(&repo, &["dangling", "--kind", "links-to"]);
        assert_eq!(code, 1, "{out}");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("no-such-section"), "{out}");

        // Drop the last bad link: nothing dangles.
        std::fs::write(
            repo.join("index.md"),
            "# Index\n\nA [present](present.md) page and a [missing page](absent.md#intro).\n\n## Real\n",
        )
        .unwrap();
        ok(&repo, &["index"]);
        let (code, out, _) = katalogr(&repo, &["dangling", "--kind", "links-to"]);
        assert_eq!((code, out.as_str()), (0, ""));
    }
}

mod kanbanr {
    use super::*;

    #[test]
    fn board_items_indexed() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = single(tmp.path(), "kanbanr/board");

        let out = ok(&repo, &["find", "FEAT-001"]);
        assert!(
            out.contains("item  app:projects/demo/features/Planned/FEAT-001.yaml#item:FEAT-001  "),
            "{out}"
        );
        let out = ok(&repo, &["find", "R-2"]);
        assert!(
            out.contains(
                "req  app:projects/demo/features/Planned/FEAT-001.yaml#item:FEAT-001/req:R-2  "
            ),
            "{out}"
        );
        let out = ok(&repo, &["find", "TL-001"]);
        assert_eq!(out.matches("task  ").count(), 2, "{out}");
        let out = ok(&repo, &["find", "G-2"]);
        assert!(
            out.contains(
                "goal  app:projects/demo/charter.yaml#goal:G-2  projects/demo/charter.yaml:"
            ),
            "{out}"
        );
        let out = ok(&repo, &["find", "ADR-0001"]);
        assert!(
            out.contains("adr  app:projects/demo/docs/decisions/0001-example.md#adr:ADR-0001  "),
            "{out}"
        );

        // The ADR affects both items; the second item serves both goals.
        let out = ok(
            &repo,
            &[
                "refs",
                "app:projects/demo/features/Implementation/FEAT-002.yaml#item:FEAT-002",
            ],
        );
        assert!(
            out.contains("affects  app:projects/demo/docs/decisions/0001-example.md#adr:ADR-0001"),
            "{out}"
        );
        let out = ok(&repo, &["refs", "app:projects/demo/charter.yaml#goal:G-2"]);
        assert!(
            out.contains(
                "serves  app:projects/demo/features/Implementation/FEAT-002.yaml#item:FEAT-002"
            ),
            "{out}"
        );

        // A monitor link in a document reaches the item.
        let out = ok(
            &repo,
            &[
                "refs",
                "app:projects/demo/features/Planned/FEAT-001.yaml#item:FEAT-001",
            ],
        );
        assert!(
            out.contains("links-to  app:projects/demo/docs/overview.md#section:overview"),
            "{out}"
        );
        let (code, out, _) = katalogr(&repo, &["dangling"]);
        assert_eq!(
            (code, out.as_str()),
            (0, ""),
            "the fixture board is self-consistent"
        );
    }

    /// `bare_code_and_board` plus the `.tx` files `(path, text)` written to the code repo and
    /// indexed.
    fn code_tx_and_board(tmp: &Path, files: &[(&str, &str)]) -> PathBuf {
        let code = bare_code_and_board(tmp);
        for (p, text) in files {
            std::fs::write(code.join(p), text).unwrap();
        }
        ok(&code, &["index", "--full"]);
        code
    }

    const REQ_1: &str = "board:projects/demo/features/Planned/FEAT-001.yaml#item:FEAT-001/req:R-1";

    #[test]
    fn requirement_tests_linked() {
        let tmp = tempfile::tempdir().unwrap();
        let code = code_tx_and_board(
            tmp.path(),
            &[(
                "src/t.tx",
                "sym fn adds_one\nexport test-name:unit::adds_one adds_one\nfact adds_one test case\n",
            )],
        );
        let out = ok(&code, &["explain", REQ_1]);
        assert!(
            out.contains("verified-by") && out.contains("code:src/t.tx#fn:adds_one"),
            "{out}"
        );
        assert!(out.to_lowercase().contains("extracted"), "{out}");
    }

    #[test]
    fn unmatched_test_name_dangling() {
        let tmp = tempfile::tempdir().unwrap();
        let code = code_tx_and_board(
            tmp.path(),
            &[(
                "src/t.tx",
                "sym fn adds_one\nexport test-name:unit::adds_one adds_one\nfact adds_one test case\n",
            )],
        );
        let (status, out, _) = katalogr(&code, &["dangling", "--kind", "verified-by"]);
        assert_eq!(status, 1, "{out}");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("req:R-2"), "{out}");
        assert!(out.contains("wants test-name:unit::missing_case"), "{out}");
        let (_, out, _) = katalogr(&code, &["dangling"]);
        assert!(!out.contains("verified-by"), "{out}");
    }

    #[test]
    fn trailer_links() {
        let tmp = tempfile::tempdir().unwrap();
        let code = code_and_board(tmp.path());

        // One history artifact per commit that carries a trailer, none for the third.
        let shards = tmp.path().join("kx/projects/demo/shards/code/@history");
        let mut found: Vec<String> = std::fs::read_dir(&shards)
            .unwrap()
            .flatten()
            .map(|e| std::fs::read_to_string(e.path()).unwrap())
            .collect();
        found.sort();
        assert_eq!(found.len(), 2, "{found:?}");
        let both = found.join("\n");
        assert!(both.contains("\"key\": \"FEAT-001/R-1\""), "{both}");
        assert!(
            both.contains("\"key\": \"code:src/a.tx\""),
            "touches the changed file: {both}"
        );
        assert!(
            !both.contains("example.test"),
            "the author's email is never stored: {both}"
        );
        assert!(both.contains("Example Author"), "{both}");

        // The line was introduced by the first commit, which names a requirement and an item.
        let (c, out, err) = katalogr(&code, &["why", "src/a.tx:1"]);
        assert_eq!(c, 0, "{err}");
        assert!(
            out.contains("  changed-by  FEAT-001/R-1  board:projects/demo/features/Planned/FEAT-001.yaml#item:FEAT-001/req:R-1\n"),
            "{out}"
        );
        assert!(out.contains("  changed-by  FEAT-002  board:projects/demo/features/Implementation/FEAT-002.yaml#item:FEAT-002\n"), "{out}");
        assert!(out.contains("  commit  "), "{out}");
        assert!(out.contains("feat: add alpha and beta"), "{out}");

        // Re-running is incremental and changes nothing.
        let again = ok(&code, &["index", "--history"]);
        assert!(again.contains("queued 2 event(s)"), "{again}");
    }

    #[test]
    fn annotation_links() {
        for (fixture, file, line, want) in [
            ("rust/annotations", "src/lib.rs", "2", "FEAT-001/R-1"),
            ("rust/annotations", "src/lib.rs", "5", "FEAT-001/R-2"),
            ("ts/annotations", "src/index.ts", "2", "FEAT-001/R-1"),
            ("ts/annotations", "src/index.ts", "9", "FEAT-001"),
        ] {
            let tmp = tempfile::tempdir().unwrap();
            let repo = single(tmp.path(), fixture);
            let (code, out, err) = katalogr(&repo, &["why", &format!("{file}:{line}")]);
            assert_eq!(code, 0, "{fixture}:{line}: {err}");
            assert!(
                out.contains(&format!("  annotated-by  {want}  app:projects/demo/features/Planned/FEAT-001.yaml#item:FEAT-001")),
                "{fixture}:{line}: {out}"
            );
        }
    }

    #[test]
    fn dangling_refs_reported() {
        // Annotations: `FEAT-009` in the Rust fixture does not exist on the board.
        let tmp = tempfile::tempdir().unwrap();
        let repo = single(tmp.path(), "rust/annotations");
        let (code, out, _) = katalogr(&repo, &["dangling", "--kind", "annotated-by"]);
        assert_eq!(code, 1, "{out}");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(
            out.contains(
                "annotated-by  app:src/lib.rs#enum:Third  src/lib.rs:7  wants board-item:FEAT-009\n"
            ),
            "{out}"
        );
        // Nothing else dangles.
        let (code, _, _) = katalogr(&repo, &["dangling", "--kind", "refs-item"]);
        assert_eq!(code, 0);

        // Trailers: the second commit names `FEAT-099`.
        let tmp = tempfile::tempdir().unwrap();
        let code_repo = code_and_board(tmp.path());
        let (code, out, _) = katalogr(&code_repo, &["dangling", "--kind", "refs-item"]);
        assert_eq!(code, 1, "{out}");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("refs-item  code:@history/"), "{out}");
        assert!(out.contains("wants board-item:FEAT-099\n"), "{out}");
        // JSON form carries the same data.
        let (code, out, _) = katalogr(&code_repo, &["--json", "dangling"]);
        assert_eq!(code, 1);
        assert!(
            out.contains("\"wanted\":[\"board-item:FEAT-099\"]"),
            "{out}"
        );
    }

    #[test]
    fn history_includes_merged_branches() {
        let tmp = tempfile::tempdir().unwrap();
        let code = bare_code_and_board(tmp.path());
        std::fs::write(code.join("src/a.tx"), "sym fn alpha\nsym fn beta\n").unwrap();
        git(&code, &["add", "src"]);
        git(&code, &["commit", "-q", "-m", "chore: base"]);
        git(&code, &["checkout", "-q", "-b", "side"]);
        std::fs::write(
            code.join("src/a.tx"),
            "sym fn alpha\nsym fn beta\nsym fn gamma\n",
        )
        .unwrap();
        git(&code, &["add", "src"]);
        git(
            &code,
            &[
                "commit",
                "-q",
                "-m",
                "feat: add gamma\n\nRefs: kanbanr:FEAT-001/R-1, kanbanr:FEAT-002",
            ],
        );
        git(&code, &["checkout", "-q", "main"]);
        std::fs::write(code.join("src/b.tx"), "sym fn delta\n").unwrap();
        git(&code, &["add", "src"]);
        git(&code, &["commit", "-q", "-m", "chore: unrelated"]);
        git(
            &code,
            &["merge", "-q", "--no-ff", "-m", "merge: side", "side"],
        );
        ok(&code, &["index", "--full"]);
        ok(&code, &["index", "--history"]);

        let (c, out, err) = katalogr(&code, &["why", "src/a.tx:3"]);
        assert_eq!(c, 0, "{err}");
        assert!(out.contains("feat: add gamma"), "{out}");
        assert!(out.contains("  changed-by  FEAT-001/R-1  board:"), "{out}");
        assert!(out.contains("  changed-by  FEAT-002  board:"), "{out}");
    }

    #[test]
    fn dangling_ignores_non_item_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let code = bare_code_and_board(tmp.path());
        std::fs::write(
            code.join("src/lib.rs"),
            "// SP-001 and MS-001 are not items.\npub fn a() {}\n\n\
             /// Encoded as UTF-8, see R-1.\npub fn b() {}\n\n\
             /// Unknown item FEAT-999.\npub fn c() {}\n\n\
             /// Known FEAT-001.\npub fn d() {}\n",
        )
        .unwrap();
        git(&code, &["add", "src"]);
        git(
            &code,
            &["commit", "-q", "-m", "feat: code\n\nRefs: kanbanr:SP-001"],
        );
        ok(&code, &["index", "--full"]);
        ok(&code, &["index", "--history"]);
        let (c, out, _) = katalogr(&code, &["dangling"]);
        assert_eq!(c, 1, "{out}");
        assert_eq!(out.lines().count(), 1, "only FEAT-999 dangles: {out}");
        assert!(out.contains("wants board-item:FEAT-999"), "{out}");
    }

    #[test]
    fn adr_mentions_resolve() {
        let tmp = tempfile::tempdir().unwrap();
        let code = bare_code_and_board(tmp.path());
        std::fs::write(
            code.join("src/lib.rs"),
            "/// Decided in ADR-0001.\npub fn a() {}\n\n/// See ADR-0099.\npub fn b() {}\n",
        )
        .unwrap();
        git(&code, &["add", "src"]);
        git(&code, &["commit", "-q", "-m", "feat: code"]);
        ok(&code, &["index", "--full"]);
        let (c, out, err) = katalogr(&code, &["why", "src/lib.rs:2"]);
        assert_eq!(c, 0, "{err}");
        assert!(
            out.contains("  annotated-by  ADR-0001  board:projects/demo/docs/decisions/0001-example.md#adr:ADR-0001"),
            "{out}"
        );
        let (c, out, _) = katalogr(&code, &["dangling"]);
        assert_eq!(c, 1, "{out}");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert!(out.contains("wants board-adr:ADR-0099"), "{out}");
    }

    #[test]
    fn why_reaches_goal() {
        let tmp = tempfile::tempdir().unwrap();
        let code = code_and_board(tmp.path());
        let (c, out, err) = katalogr(&code, &["why", "src/a.tx:2"]);
        assert_eq!(c, 0, "{err}");
        // symbol, then items/requirements, then goals, then the commit.
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "fn  code:src/a.tx#fn:beta  src/a.tx:2", "{out}");
        let at = |needle: &str| {
            lines
                .iter()
                .position(|l| l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} not in {out}"))
        };
        let item = at("changed-by  FEAT-001/R-1");
        let goal1 = at("serves  G-1  board:projects/demo/charter.yaml#goal:G-1");
        let goal2 = at("serves  G-2  board:projects/demo/charter.yaml#goal:G-2");
        let commit = at("commit  ");
        assert!(item < goal1 && goal1 < commit && goal2 < commit, "{out}");

        // The line added by the second commit reaches no goal: its item is not on the board.
        let (c, out, _) = katalogr(&code, &["why", "src/a.tx:3"]);
        assert_eq!(c, 0);
        assert!(!out.contains("serves"), "{out}");

        // JSON carries the goals and the commit.
        let (c, out, _) = katalogr(&code, &["--json", "why", "src/a.tx:2"]);
        assert_eq!(c, 0);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["result"]["goals"].as_array().unwrap().len(), 2, "{out}");
        assert_eq!(v["result"]["commits"].as_array().unwrap().len(), 1, "{out}");
    }
}

mod testlink {
    use super::*;

    /// One marked repository `app` holding the given `(path, text)` files; indexed.
    fn repo_with(tmp: &Path, files: &[(&str, &str)]) -> PathBuf {
        let repo = tmp.join("app");
        std::fs::create_dir_all(&repo).unwrap();
        for (p, text) in files {
            std::fs::write(repo.join(p), text).unwrap();
        }
        project(tmp, &[("app", &repo)]);
        ok(&repo, &["index"]);
        repo
    }

    /// The `tests` lines as `(role, name suffix after '#', depth, rest)`.
    fn lines(out: &str) -> Vec<String> {
        out.lines().map(str::to_string).collect()
    }

    fn line_of<'a>(ls: &'a [String], sym: &str) -> &'a str {
        ls.iter()
            .find(|l| l.contains(&format!("#fn:{sym}  ")))
            .unwrap_or_else(|| panic!("{sym} in {ls:?}"))
    }

    #[test]
    fn direct_and_transitive_tests() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with(
            tmp.path(),
            &[(
                "t.tx",
                "sym fn target\nsym fn helper\nsym fn t1\nsym fn t2\nsym fn t3\n\
                 link t1 calls target\nlink t2 calls helper\nlink helper calls target\n\
                 link t3 calls target\n\
                 fact t1 test case\nfact t2 test case\nfact t3 test support\n",
            )],
        );
        let out = ok(&repo, &["tests", "target"]);
        let ls = lines(&out);
        assert_eq!(
            ls.iter().filter(|l| !l.starts_with("next:")).count(),
            3,
            "{out}"
        );
        let t1 = line_of(&ls, "t1");
        assert!(t1.starts_with("case  ") && t1.contains("depth 1"), "{t1}");
        assert!(t1.contains("extracted") && t1.contains("1.0000"), "{t1}");
        let t3 = line_of(&ls, "t3");
        assert!(
            t3.starts_with("support  ") && t3.contains("depth 1"),
            "{t3}"
        );
        let t2 = line_of(&ls, "t2");
        assert!(
            t2.contains("depth 2") && t2.contains("through app:t.tx#fn:helper"),
            "{t2}"
        );
        // Sorted by depth, then confidence, score and name.
        let pos = |s: &str| {
            ls.iter()
                .position(|l| l.contains(&format!("#fn:{s}  ")))
                .unwrap()
        };
        assert!(pos("t1") < pos("t3") && pos("t3") < pos("t2"), "{out}");
        // `--cases` drops the support code.
        let out = ok(&repo, &["tests", "target", "--cases"]);
        assert!(!out.contains("#fn:t3"), "{out}");
        assert!(out.contains("#fn:t1") && out.contains("#fn:t2"), "{out}");
    }

    #[test]
    fn contract_mediated_tests() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with(
            tmp.path(),
            &[(
                "t.tx",
                "sym fn prov\nsym fn cons\nsym fn t\n\
                 bind prov provides endpoint:orders\nbind cons consumes endpoint:orders\n\
                 link t calls cons\nfact t test case\n",
            )],
        );
        let out = ok(&repo, &["tests", "prov"]);
        let ls = lines(&out);
        let t = line_of(&ls, "t");
        assert!(t.contains("depth 2"), "{t}");
        assert!(t.contains("via contract:endpoint:orders"), "{t}");
        assert!(t.contains("through app:t.tx#fn:cons"), "{t}");
    }

    #[test]
    fn depth_limits_tests() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with(
            tmp.path(),
            &[(
                "t.tx",
                "sym fn target\nsym fn h1\nsym fn h2\nsym fn h3\nsym fn t\n\
                 link h1 calls target\nlink h2 calls h1\nlink h3 calls h2\nlink t calls h3\n\
                 fact t test case\n",
            )],
        );
        let out = ok(&repo, &["tests", "target", "--depth", "3"]);
        assert!(!out.contains("#fn:t  "), "{out}");
        let out = ok(&repo, &["tests", "target", "--depth", "4"]);
        assert!(out.contains("#fn:t  ") && out.contains("depth 4"), "{out}");
        // The default depth is 3.
        let out = ok(&repo, &["tests", "target"]);
        assert!(!out.contains("#fn:t  "), "{out}");
    }

    #[test]
    fn inferred_path_confidence() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with(
            tmp.path(),
            &[(
                "t.tx",
                "sym fn target\nsym fn other\nsym fn t\n\
                 export ns:either target\nexport ns:either other\n\
                 iref t calls ns:either\nfact t test case\n",
            )],
        );
        let out = ok(&repo, &["--json", "tests", "target"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        let hit = &v["result"][0];
        assert_eq!(hit["role"], "case", "{out}");
        assert_eq!(hit["confidence"], "Inferred", "{out}");
        assert_eq!(hit["score"], 0.5, "{out}");
    }

    const T_CODE: &str = "sym fn adds_one\nexport test-name:unit::adds_one adds_one\n\
                          fact adds_one test case\nsym fn lib_fn\nlink adds_one calls lib_fn\n";
    const REQ_1: &str = "board:projects/demo/features/Planned/FEAT-001.yaml#item:FEAT-001/req:R-1";

    fn code_and_board_tx(tmp: &Path) -> PathBuf {
        let code = bare_code_and_board(tmp);
        std::fs::write(code.join("src/t.tx"), T_CODE).unwrap();
        ok(&code, &["index", "--full"]);
        code
    }

    #[test]
    fn requirements_on_hits() {
        let tmp = tempfile::tempdir().unwrap();
        let code = code_and_board_tx(tmp.path());
        let out = ok(&code, &["tests", "lib_fn"]);
        let line = out
            .lines()
            .find(|l| l.contains("#fn:adds_one"))
            .expect(&out);
        assert!(line.contains(&format!("verifies {REQ_1}")), "{line}");
    }

    #[test]
    fn requirement_lists_its_tests() {
        let tmp = tempfile::tempdir().unwrap();
        let code = code_and_board_tx(tmp.path());
        let out = ok(&code, &["tests", REQ_1]);
        let line = out
            .lines()
            .find(|l| l.contains("#fn:adds_one"))
            .expect(&out);
        assert!(
            line.starts_with("case  ") && line.contains("depth 1"),
            "{line}"
        );
    }

    #[test]
    fn multi_seed_nearest() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with(
            tmp.path(),
            &[(
                "t.tx",
                "sym fn a\nsym fn b\nsym fn t\nsym fn u\nsym fn h\n\
                 link t calls a\nlink b calls a\nlink u calls b\nlink u calls h\n\
                 link h calls a\nfact t test case\nfact u test case\n",
            )],
        );
        // `t` reaches seed `a` at depth 1; `u` reaches `b` at depth 1 and `a` at depth 2, and
        // is reported once with its nearest seed.
        let k =
            katalogr_dist::Katalogr::resolve(&repo, &katalogr_dist::Overrides::default()).unwrap();
        let reply = k
            .tests_for(
                &["a".to_string(), "b".to_string()],
                katalogr_dist::query::TestsOptions::default(),
                &katalogr_dist::Options::default(),
            )
            .unwrap();
        let katalogr_dist::Outcome::Found(hits) = reply.outcome else {
            panic!("seeds resolve");
        };
        assert_eq!(hits.len(), 2, "{hits:?}");
        let by = |n: &str| hits.iter().find(|h| h.test.name.ends_with(n)).unwrap();
        assert_eq!(
            (by("#fn:t").seed.as_str(), by("#fn:t").depth),
            ("app:t.tx#fn:a", 1)
        );
        assert_eq!(
            (by("#fn:u").seed.as_str(), by("#fn:u").depth),
            ("app:t.tx#fn:b", 1)
        );
    }

    #[test]
    fn tests_json_envelope() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = repo_with(
            tmp.path(),
            &[(
                "t.tx",
                "sym fn target\nsym fn t1\nsym fn t2\nsym fn t3\n\
                 link t1 calls target\nlink t2 calls target\nlink t3 calls target\n\
                 fact t1 test case\nfact t2 test case\nfact t3 test case\n",
            )],
        );
        let out = ok(&repo, &["--json", "tests", "target"]);
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["schema"], 1, "{out}");
        assert_eq!(v["result"][0]["role"], "case", "{out}");
        assert_eq!(v["result"].as_array().unwrap().len(), 3, "{out}");
        let out = ok(&repo, &["tests", "target", "--budget", "20"]);
        assert!(out.contains("more omitted"), "{out}");
    }
}
