//! The user guide and feature pages on the documentation site are listed in the book, name only
//! commands and flags the built binary accepts, and include the kanbanr and MCP pages (FEAT-116).

mod guide {
    use std::collections::{BTreeSet, HashMap};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// Every `.md` file under `docs/src/guide/` and `docs/src/features/`, as `guide/x.md` or
    /// `features/x.md`, sorted.
    fn pages() -> Vec<String> {
        let mut out = Vec::new();
        for dir in ["guide", "features"] {
            let path = root().join("docs/src").join(dir);
            let entries = fs::read_dir(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            for e in entries {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                if name.ends_with(".md") {
                    out.push(format!("{dir}/{name}"));
                }
            }
        }
        out.sort();
        out
    }

    fn page(rel: &str) -> String {
        read(&root().join("docs/src").join(rel))
    }

    /// The built binary: the one cargo built for this test if it knows it, else the workspace's
    /// `target/debug` or `target/release` build. The archtest crate has no binary dependency, so
    /// the binary has to be built first (`cargo build -p katalogr`).
    fn binary() -> PathBuf {
        if let Some(p) = option_env!("CARGO_BIN_EXE_katalogr") {
            return PathBuf::from(p);
        }
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| root().join("target"));
        let exe = format!("katalogr{}", std::env::consts::EXE_SUFFIX);
        for profile in ["debug", "release"] {
            let p = target.join(profile).join(&exe);
            if p.is_file() {
                return p;
            }
        }
        panic!(
            "no katalogr binary under {}: build it first with `cargo build -p katalogr`",
            target.display()
        );
    }

    fn help(args: &[&str]) -> String {
        let out = Command::new(binary())
            .args(args)
            .arg("--help")
            .output()
            .unwrap_or_else(|e| panic!("run katalogr {args:?}: {e}"));
        assert!(out.status.success(), "katalogr {args:?} --help failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The names under `Commands:` in a help text.
    fn subcommands(help: &str) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let mut in_commands = false;
        for line in help.lines() {
            if line.starts_with("Commands:") {
                in_commands = true;
            } else if in_commands {
                if !line.starts_with("  ") {
                    break;
                }
                if let Some(name) = line.split_whitespace().next() {
                    out.insert(name.to_string());
                }
            }
        }
        out
    }

    /// Every `--flag` word in a text.
    fn flags(text: &str) -> BTreeSet<String> {
        let b = text.as_bytes();
        let mut out = BTreeSet::new();
        let mut i = 0;
        while i + 2 < b.len() {
            let boundary = i == 0 || !b[i - 1].is_ascii_alphanumeric() && b[i - 1] != b'-';
            if boundary && b[i] == b'-' && b[i + 1] == b'-' && b[i + 2].is_ascii_lowercase() {
                let mut j = i + 2;
                while j < b.len()
                    && (b[j].is_ascii_lowercase() || b[j].is_ascii_digit() || b[j] == b'-')
                {
                    j += 1;
                }
                out.insert(text[i..j].trim_end_matches('-').to_string());
                i = j;
            } else {
                i += 1;
            }
        }
        out
    }

    /// The text of every inline code span on a line.
    fn spans(line: &str) -> Vec<String> {
        let b = line.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] != b'`' {
                i += 1;
                continue;
            }
            let start = i;
            while i < b.len() && b[i] == b'`' {
                i += 1;
            }
            let n = i - start;
            // The closing run has exactly the same length.
            let mut j = i;
            let mut close = None;
            while j < b.len() {
                if b[j] == b'`' {
                    let s = j;
                    while j < b.len() && b[j] == b'`' {
                        j += 1;
                    }
                    if j - s == n {
                        close = Some(s);
                        break;
                    }
                } else {
                    j += 1;
                }
            }
            match close {
                Some(s) => {
                    out.push(line[i..s].to_string());
                    i = s + n;
                }
                None => break,
            }
        }
        out
    }

    /// `(line number, text)` for every `katalogr <word>` that starts a code span or a line of a
    /// fenced code block. A leading `$ ` prompt is ignored.
    fn commands(md: &str) -> Vec<(usize, String)> {
        let mut out = Vec::new();
        let mut fence: Option<(char, usize)> = None;
        for (n, line) in md.lines().enumerate() {
            let t = line.trim_start();
            let run = t.chars().take_while(|c| *c == '`' || *c == '~').count();
            let ch = t.chars().next().unwrap_or(' ');
            if run >= 3 && (ch == '`' || ch == '~') {
                match fence {
                    None => {
                        fence = Some((ch, run));
                        continue;
                    }
                    Some((c, len)) if c == ch && run >= len && t[run..].trim().is_empty() => {
                        fence = None;
                        continue;
                    }
                    _ => {}
                }
            }
            if fence.is_some() {
                let body = t.strip_prefix("$ ").unwrap_or(t);
                if body.starts_with("katalogr ") {
                    out.push((n + 1, body.to_string()));
                }
            } else {
                for s in spans(line) {
                    let s = s.trim().to_string();
                    if s.starts_with("katalogr ") {
                        out.push((n + 1, s));
                    }
                }
            }
        }
        out
    }

    /// R-1. Every page under `guide/` and `features/` is linked from `SUMMARY.md`, and every
    /// such link in `SUMMARY.md` points at a page that exists.
    #[test]
    fn pages_in_book() {
        let summary = read(&root().join("docs/src/SUMMARY.md"));
        let pages = pages();
        assert!(!pages.is_empty(), "no guide or feature pages found");
        for p in &pages {
            assert!(
                summary.contains(&format!("({p})")),
                "docs/src/SUMMARY.md does not link ({p})"
            );
        }
        for line in summary.lines() {
            for part in line.split("](").skip(1) {
                let target = part.split(')').next().unwrap_or("");
                if target.starts_with("guide/") || target.starts_with("features/") {
                    assert!(
                        root().join("docs/src").join(target).is_file(),
                        "SUMMARY.md links {target}, which does not exist"
                    );
                }
            }
        }
    }

    /// R-2. Every `katalogr <subcommand>` in the pages names a top-level subcommand the binary
    /// lists, and every `--flag` on that line is in the help of that subcommand (or of its
    /// nested subcommand) or in the global options.
    #[test]
    fn commands_exist() {
        let top = help(&[]);
        let known = subcommands(&top);
        assert!(!known.is_empty(), "katalogr --help lists no commands");
        let global = flags(&top);
        let mut cache: HashMap<Vec<String>, String> = HashMap::new();
        let mut problems = Vec::new();
        let mut checked = 0;
        for rel in pages() {
            for (line, cmd) in commands(&page(&rel)) {
                let rest = cmd.trim_start_matches("katalogr").trim_start();
                let mut words = rest.split_whitespace();
                let first = words.next().unwrap_or("");
                let word = first.trim_end_matches([':', ',', '.', ';']);
                let is_word = word.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                    && word.chars().all(|c| c.is_ascii_lowercase() || c == '-');
                let mut allowed = global.clone();
                if is_word {
                    if !known.contains(word) {
                        problems.push(format!(
                            "{rel}:{line}: `katalogr {word}` is not a subcommand: {cmd}"
                        ));
                        continue;
                    }
                    let key = vec![word.to_string()];
                    let sub = cache.entry(key).or_insert_with(|| help(&[word])).clone();
                    allowed.extend(flags(&sub));
                    if let Some(second) = words.next() {
                        let second = second.trim_end_matches([':', ',', '.', ';']);
                        if subcommands(&sub).contains(second) {
                            let nested = cache
                                .entry(vec![word.to_string(), second.to_string()])
                                .or_insert_with(|| help(&[word, second]))
                                .clone();
                            allowed.extend(flags(&nested));
                        }
                    }
                } else if !first.starts_with("--") {
                    // A placeholder, a version number or an ellipsis: output, not a command.
                    continue;
                }
                checked += 1;
                for flag in flags(rest) {
                    if !allowed.contains(&flag) {
                        problems.push(format!(
                            "{rel}:{line}: `{flag}` is not a flag of `katalogr {first}`: {cmd}"
                        ));
                    }
                }
            }
        }
        assert!(checked > 50, "only {checked} commands found in the pages");
        assert!(problems.is_empty(), "\n{}", problems.join("\n"));
    }

    /// R-3. The kanbanr page and the MCP page exist, and the MCP page names `katalogr mcp`.
    #[test]
    fn integration_pages_present() {
        for rel in ["guide/kanbanr.md", "guide/mcp.md"] {
            let text = page(rel);
            assert!(!text.trim().is_empty(), "{rel} is empty");
        }
        assert!(
            page("guide/mcp.md").contains("katalogr mcp"),
            "guide/mcp.md does not mention `katalogr mcp`"
        );
    }
}
