//! The load-test harness (`katalogr-bench`, FEAT-068) end to end: it drives the real `katalogr`
//! binary over a synthetic git corpus generated at test time, with invented identifiers. No
//! corpus name, clone or index lives in the repository.

mod loadtest {
    use camino::{Utf8Path, Utf8PathBuf};
    use katalogr_bench::disk::{DiskPolicy, Keep};
    use katalogr_bench::fetch;
    use katalogr_bench::git::Git;
    use katalogr_bench::idle::IdlePolicy;
    use katalogr_bench::manifest::{Corpus, Manifest};
    use katalogr_bench::measure::{self, CorpusResult, Phases, Settings};
    use katalogr_bench::proc;
    use katalogr_bench::workspace::Workspace;
    use std::process::Command;
    use std::time::Duration;

    const KATALOGR: &str = env!("CARGO_BIN_EXE_katalogr");

    fn git(dir: &Utf8Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn write(dir: &Utf8Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    /// A git repository of 20 Rust files, 20 TypeScript files, 5 Markdown files, 1 SQL file and 3
    /// `.go`-suffixed files, in 3 commits. Returns its `file://` URL and the commits, oldest first.
    pub fn synthetic_corpus(dir: &Utf8Path) -> (String, Vec<String>) {
        let repo = dir.join("remote");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        write(
            &repo,
            "Cargo.toml",
            "[package]\nname = \"unitsynth\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        );
        let mut lib = String::new();
        for i in 0..20 {
            lib.push_str(&format!("pub mod m{i};\n"));
            let call = if i > 0 {
                format!("crate::m{}::rs_unit_{}(x) + ", i - 1, i - 1)
            } else {
                String::new()
            };
            write(
                &repo,
                &format!("src/m{i}.rs"),
                &format!(
                    "/// Unit {i} of the synthetic corpus.\npub fn rs_unit_{i}(x: u32) -> u32 {{\n    {call}rs_helper_{i}(x)\n}}\n\nfn rs_helper_{i}(x: u32) -> u32 {{\n    x + {i}\n}}\n\n#[test]\nfn rs_unit_{i}_adds() {{\n    assert!(rs_unit_{i}(1) > 0);\n}}\n"
                ),
            );
        }
        write(&repo, "src/lib.rs", &lib);
        for i in 0..20 {
            let (imp, call) = if i > 0 {
                (
                    format!("import {{ tsUnit{} }} from \"./t{}\";\n", i - 1, i - 1),
                    format!("tsUnit{}(x) + ", i - 1),
                )
            } else {
                (String::new(), String::new())
            };
            write(
                &repo,
                &format!("web/t{i}.ts"),
                &format!(
                    "{imp}export function tsUnit{i}(x: number): number {{\n  return {call}x + {i};\n}}\n"
                ),
            );
        }
        for i in 0..5 {
            write(
                &repo,
                &format!("docs/d{i}.md"),
                &format!(
                    "# Synthetic document {i}\n\nSee [next](d{}.md) for the parse config details.\n",
                    (i + 1) % 5
                ),
            );
        }
        write(
            &repo,
            "db/schema.sql",
            "CREATE TABLE synth_orders (id INT PRIMARY KEY, total INT);\n",
        );
        for i in 0..3 {
            write(
                &repo,
                &format!("go/g{i}.go"),
                &format!(
                    "package g\n\n// Handler{i} answers a synthetic request.\nfunc Handler{i}() {{}}\n"
                ),
            );
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        let c1 = git(&repo, &["rev-parse", "HEAD"]);
        write(
            &repo,
            "src/m0.rs",
            "/// Unit 0, changed.\npub fn rs_unit_0(x: u32) -> u32 {\n    rs_helper_0(x) + 1\n}\n\nfn rs_helper_0(x: u32) -> u32 {\n    x\n}\n\n#[test]\nfn rs_unit_0_adds() {\n    assert!(rs_unit_0(1) > 0);\n}\n",
        );
        write(
            &repo,
            "web/t0.ts",
            "export function tsUnit0(x: number): number {\n  return x * 2;\n}\n",
        );
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "second"]);
        let c2 = git(&repo, &["rev-parse", "HEAD"]);
        write(
            &repo,
            "docs/d9.md",
            "# Added document\n\nA late addition about buffer size.\n",
        );
        write(
            &repo,
            "go/g1.go",
            "package g\n\n// Handler1 now does more.\nfunc Handler1() { println(\"more\") }\n",
        );
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "third"]);
        let c3 = git(&repo, &["rev-parse", "HEAD"]);
        (file_url(repo.as_str()), vec![c1, c2, c3])
    }

    /// A `file://` URL git accepts on every system: `file:///C:/a/b` on Windows.
    fn file_url(path: &str) -> String {
        let p = path.replace('\\', "/");
        if p.starts_with('/') {
            format!("file://{p}")
        } else {
            format!("file:///{p}")
        }
    }

    fn manifest(url: &str, head: &str, replay: u32) -> Corpus {
        let yaml = format!(
            "version: 1\ncorpora:\n  - name: synth\n    tier: S\n    category: synthetic\n    languages: [rust, ts, md, sql, go]\n    primary: main\n    replay: {replay}\n    profile: synth.yaml\n    repos:\n      - {{ name: main, url: '{url}', commit: {head}, depth: 3 }}\n    approx: {{ files: 50, tree_mib: 1, disk_gib: 0.01 }}\n"
        );
        Manifest::parse(&yaml).unwrap().corpora.remove(0)
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        ws: Workspace,
        corpus: Corpus,
        commits: Vec<String>,
    }

    fn fixture(replay: u32) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let base = Utf8Path::from_path(tmp.path()).unwrap().to_path_buf();
        let (url, commits) = synthetic_corpus(&base);
        let ws = Workspace::open(Some(&base.join("ws"))).unwrap();
        ws.init_layout().unwrap();
        std::fs::write(
            ws.profile("synth.yaml"),
            "profile: loadtest-go\nversion: 2\nselect:\n  - { extension: text, path: \"**/*.go\" }\nrules: []\n",
        )
        .unwrap();
        let corpus = manifest(&url, commits.last().unwrap(), replay);
        Fixture {
            _tmp: tmp,
            ws,
            corpus,
            commits,
        }
    }

    fn settings(phases: Phases) -> Settings {
        Settings {
            katalogr: Utf8PathBuf::from(KATALOGR),
            runs: 2,
            reps: 2,
            targets: 3,
            concurrency: 2,
            idle: IdlePolicy {
                max_load: 1000.0,
                wait: Duration::from_secs(1),
                poll: Duration::from_millis(10),
                allow_loaded: false,
            },
            disk: DiskPolicy {
                budget_bytes: 1 << 40,
                min_free_bytes: 0,
            },
            keep: Keep::All,
            refresh_workload: false,
            phases,
        }
    }

    fn run(f: &Fixture, phases: Phases) -> CorpusResult {
        let r = measure::run_corpus(&f.ws, &f.corpus, &settings(phases), &Git::default()).unwrap();
        assert_eq!(r.aborted, None);
        r
    }

    fn phases(index: bool, replay: bool, query: bool, concurrent: bool) -> Phases {
        Phases {
            index,
            replay,
            query,
            concurrent,
        }
    }

    #[test]
    fn fetch_pins_commit() {
        let f = fixture(2);
        let got = fetch::fetch(&f.ws, &f.corpus, &Git::default()).unwrap();
        assert_eq!(got.len(), 1);
        let dir = f.ws.clone_dir("synth", "main");
        assert_eq!(
            Git::default().run(&dir, &["rev-parse", "HEAD"]).unwrap(),
            f.commits[2]
        );
        // First-parent order, oldest first; the base is the oldest.
        assert_eq!(got[0].replay, f.commits);
        assert_eq!(got[0].base, f.commits[0]);
        assert_eq!(got[0].head, f.commits[2]);
        assert!(!dir.join(".git/hooks/pre-commit").exists());
    }

    #[test]
    fn fetch_is_idempotent() {
        let f = fixture(2);
        let first = fetch::fetch(&f.ws, &f.corpus, &Git::default()).unwrap();
        // Delete the remote: a second fetch must not touch the network (the file system).
        let remote = f.corpus.repos[0]
            .url
            .trim_start_matches("file://")
            .to_string();
        std::fs::remove_dir_all(&remote).unwrap();
        let second = fetch::fetch(&f.ws, &f.corpus, &Git::default()).unwrap();
        assert_eq!(first, second);
        // A clone left on another commit is re-checked out without a fetch.
        let dir = f.ws.clone_dir("synth", "main");
        Git::default()
            .run(&dir, &["checkout", "-q", "--detach", &f.commits[0]])
            .unwrap();
        fetch::fetch(&f.ws, &f.corpus, &Git::default()).unwrap();
        assert_eq!(
            Git::default().run(&dir, &["rev-parse", "HEAD"]).unwrap(),
            f.commits[2]
        );
    }

    #[test]
    fn full_index_measures() {
        let f = fixture(0);
        let r = run(&f, phases(true, false, false, false));
        let i = r.index.expect("index result");
        assert_eq!(i.runs, 2);
        assert_eq!(i.wall_s.n, 2);
        assert!(i.files_indexed > 0, "{i:?}");
        assert!(i.files_per_s > 0.0);
        assert!(i.shard_bytes > 0 && i.shard_files > 0);
        assert!(i.cache_bytes > 0);
        if cfg!(target_os = "linux") {
            assert!(i.peak_rss_mib.p50 > 0.0);
        }
        assert!(!r.loaded);
        // 20 Rust + lib.rs + Cargo.toml, 20 TS, 6 Markdown (one added), 1 SQL, 3 Go.
        assert_eq!(r.repos[0].files_in_tree, 52);
    }

    #[test]
    fn shards_identical_across_runs() {
        let f = fixture(0);
        let r = run(&f, phases(true, false, false, false));
        let i = r.index.unwrap();
        assert!(i.digests_equal, "two cold runs of one pinned corpus differ");
        assert_eq!(i.digest.len(), 64);
        // A third, separate measurement agrees with the first.
        let again = run(&f, phases(true, false, false, false)).index.unwrap();
        assert_eq!(again.digest, i.digest);
    }

    #[test]
    fn text_fallback_baseline() {
        let f = fixture(0);
        let r = run(&f, phases(true, false, false, false));
        let d = r.discovery.expect("discovery result");
        assert_eq!(d.text_files, 3, "{d:?}");
        assert!(d.text_chunks.unwrap_or(0) >= 3, "{d:?}");
        assert!(d.files_seen >= d.files_indexed);
    }

    #[test]
    fn replay_commits_measured() {
        let f = fixture(2);
        let r = run(&f, phases(true, true, false, false));
        let p = r.replay.expect("replay result");
        assert_eq!(p.commits, 2);
        assert_eq!(p.wall_ms.n, 2);
        assert!(p.files_changed.max >= 1.0, "{p:?}");
        let dir = f.ws.clone_dir("synth", "main");
        assert_eq!(
            Git::default().run(&dir, &["rev-parse", "HEAD"]).unwrap(),
            f.commits[2]
        );
    }

    #[test]
    fn query_latency_all_commands() {
        let f = fixture(0);
        let r = run(&f, phases(true, false, true, false));
        let want = [
            "find",
            "refs",
            "callers",
            "explain",
            "tests",
            "path",
            "search",
            "map",
            "dangling",
            "hubs",
            "cycles",
            "communities",
            "dead-code",
            "mirrors",
            "coverage",
            "doctor",
            "doctor-skip-git",
            "session-start",
        ];
        for cmd in want {
            let q = r
                .queries
                .iter()
                .find(|q| q.command == cmd)
                .unwrap_or_else(|| {
                    panic!(
                        "no result for {cmd}: {:?}",
                        r.queries.iter().map(|q| &q.command).collect::<Vec<_>>()
                    )
                });
            assert!(q.samples > 0, "{cmd}: {q:?}");
            assert_eq!(q.errors, 0, "{cmd}: {q:?}");
        }
        // The workload is frozen in the workspace for the next run.
        assert!(f.ws.workload("synth").is_file());
    }

    #[test]
    fn concurrent_queries_while_indexing() {
        let f = fixture(0);
        let mut s = settings(phases(true, false, true, true));
        s.concurrency = 2;
        let r = measure::run_corpus(&f.ws, &f.corpus, &s, &Git::default()).unwrap();
        assert_eq!(r.aborted, None);
        let c = r.concurrent.expect("concurrent result");
        assert_eq!(c.workers, 2);
        assert_eq!(c.errors, 0, "{c:?}");
        assert!(c.queries > 0, "{c:?}");
        assert!(c.digest_equal, "{c:?}");
        assert!(c.doctor_clean, "{c:?}");
        assert!(c.index_slowdown > 0.0);
    }

    #[test]
    fn harness_overhead_small() {
        let mut through = Vec::new();
        let mut bare = Vec::new();
        for _ in 0..50 {
            let t = std::time::Instant::now();
            let out = Command::new(KATALOGR).arg("--version").output().unwrap();
            bare.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(out.status.success());
            let r = proc::run(Command::new(KATALOGR).arg("--version")).unwrap();
            assert_eq!(r.status, Some(0));
            through.push(r.wall.as_secs_f64() * 1000.0);
        }
        let overhead =
            katalogr_bench::stats::median(&through) - katalogr_bench::stats::median(&bare);
        assert!(overhead < 5.0, "median overhead {overhead:.2} ms");
    }
}
