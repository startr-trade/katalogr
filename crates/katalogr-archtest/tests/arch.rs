//! Architecture tests: neutrality of the core, analytics layering, thin wrappers, extension
//! boundaries and absence of C build dependencies (ADR-0011, Compliance).

mod archtest {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use cargo_metadata::{DependencyKind, Metadata, MetadataCommand, Package, PackageId};
    use proc_macro2::LineColumn;
    use syn::visit::Visit;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn metadata(deps: bool) -> Metadata {
        let mut cmd = MetadataCommand::new();
        cmd.current_dir(workspace_root());
        if !deps {
            cmd.no_deps();
        }
        cmd.exec().expect("cargo metadata")
    }

    fn workspace_package<'a>(md: &'a Metadata, name: &str) -> Option<&'a Package> {
        md.workspace_packages()
            .into_iter()
            .find(|p| p.name.as_str() == name)
    }

    fn normal_deps(p: &Package) -> BTreeSet<String> {
        p.dependencies
            .iter()
            .filter(|d| d.kind == DependencyKind::Normal)
            .map(|d| d.name.clone())
            .collect()
    }

    fn deny_list() -> BTreeSet<String> {
        let text =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("deny-list.txt"))
                .expect("deny-list.txt");
        text.lines()
            .map(|l| l.trim().to_lowercase())
            .filter(|l| !l.is_empty())
            .collect()
    }

    /// Split an identifier on '_' and case boundaries (`HTMLParser` -> html, parser).
    fn ident_tokens(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        for part in s.split('_') {
            let chars: Vec<char> = part.chars().collect();
            let mut cur = String::new();
            for (i, &c) in chars.iter().enumerate() {
                if !cur.is_empty() {
                    let prev = chars[i - 1];
                    let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
                    let boundary = (prev.is_lowercase() && c.is_uppercase())
                        || (prev.is_uppercase() && c.is_uppercase() && next_lower)
                        || (prev.is_alphabetic() != c.is_alphabetic());
                    if boundary {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                cur.push(c);
            }
            if !cur.is_empty() {
                out.push(cur);
            }
        }
        out.into_iter().map(|t| t.to_lowercase()).collect()
    }

    struct Scan<'a> {
        deny: &'a BTreeSet<String>,
        file: &'a Path,
        hits: Vec<String>,
    }

    impl Scan<'_> {
        fn hit(&mut self, line: LineColumn, token: &str, what: &str) {
            self.hits.push(format!(
                "{}:{}: {what} contains deny-listed word `{token}`",
                self.file.display(),
                line.line
            ));
        }
        fn ident(&mut self, id: &proc_macro2::Ident) {
            let name = id.to_string();
            for t in ident_tokens(name.trim_start_matches("r#")) {
                if self.deny.contains(&t) {
                    self.hit(id.span().start(), &t, &format!("identifier `{name}`"));
                }
            }
        }
    }

    impl<'ast> Visit<'ast> for Scan<'_> {
        fn visit_ident(&mut self, id: &'ast proc_macro2::Ident) {
            self.ident(id);
        }
        fn visit_lit_str(&mut self, lit: &'ast syn::LitStr) {
            let value = lit.value();
            for w in value.split(|c: char| !c.is_alphanumeric()) {
                let w = w.to_lowercase();
                if self.deny.contains(&w) {
                    self.hit(lit.span().start(), &w, "string literal");
                }
            }
        }
        // Macro bodies are token streams that `syn` does not descend into by default.
        fn visit_macro(&mut self, m: &'ast syn::Macro) {
            self.visit_path(&m.path);
            scan_tokens(self, m.tokens.clone());
        }
    }

    fn scan_tokens(scan: &mut Scan<'_>, ts: proc_macro2::TokenStream) {
        use proc_macro2::TokenTree;
        for tt in ts {
            match tt {
                TokenTree::Group(g) => scan_tokens(scan, g.stream()),
                TokenTree::Ident(i) => scan.ident(&i),
                TokenTree::Literal(l) => {
                    if let Ok(lit) = syn::parse_str::<syn::LitStr>(&l.to_string()) {
                        let value = lit.value();
                        for w in value.split(|c: char| !c.is_alphanumeric()) {
                            let w = w.to_lowercase();
                            if scan.deny.contains(&w) {
                                scan.hit(l.span().start(), &w, "string literal");
                            }
                        }
                    }
                }
                TokenTree::Punct(_) => {}
            }
        }
    }

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    fn core_crates(md: &Metadata) -> Vec<&Package> {
        [
            "katalogr-model",
            "katalogr-store",
            "katalogr-engine",
            "katalogr-query",
            "katalogr-analytics",
        ]
        .iter()
        .filter_map(|n| workspace_package(md, n))
        .collect()
    }

    const PARSER_PREFIXES: [&str; 5] = ["ra_ap_", "oxc", "quick-xml", "sqlparser", "tree-sitter"];

    #[test]
    fn core_is_neutral() {
        let md = metadata(false);
        let deny = deny_list();
        let crates = core_crates(&md);
        assert!(!crates.is_empty(), "no core crates found");
        let mut failures = Vec::new();

        for p in &crates {
            for d in normal_deps(p) {
                if d.starts_with("katalogr-ext-")
                    || PARSER_PREFIXES.iter().any(|x| d.starts_with(x))
                {
                    failures.push(format!("{} depends on forbidden crate `{d}`", p.name));
                }
            }
            let root = p.manifest_path.parent().expect("manifest dir");
            let mut files = Vec::new();
            rs_files(root.join("src").as_std_path(), &mut files);
            files.sort();
            for f in files {
                let src = std::fs::read_to_string(&f).expect("read source");
                let ast = match syn::parse_file(&src) {
                    Ok(a) => a,
                    Err(e) => {
                        failures.push(format!("{}: parse error: {e}", f.display()));
                        continue;
                    }
                };
                let mut scan = Scan {
                    deny: &deny,
                    file: &f,
                    hits: Vec::new(),
                };
                scan.visit_file(&ast);
                failures.append(&mut scan.hits);
            }
        }
        assert!(
            failures.is_empty(),
            "core is not neutral:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn analytics_layering() {
        let md = metadata(false);
        let mut failures = Vec::new();
        // (a) Only katalogr-dist may depend on katalogr-analytics, whatever the kind.
        for p in md.workspace_packages() {
            if p.name.as_str() == "katalogr-dist" || p.name.as_str() == "katalogr-analytics" {
                continue;
            }
            if p.dependencies
                .iter()
                .any(|d| d.name == "katalogr-analytics")
            {
                failures.push(format!("{} must not depend on katalogr-analytics", p.name));
            }
        }
        // (b) Analytics depends on model, store and query only.
        let allowed: BTreeSet<&str> = ["katalogr-model", "katalogr-store", "katalogr-query"]
            .into_iter()
            .collect();
        match workspace_package(&md, "katalogr-analytics") {
            None => failures.push("package katalogr-analytics not found".to_string()),
            Some(a) => {
                for d in normal_deps(a) {
                    if d.starts_with("katalogr-") && !allowed.contains(d.as_str()) {
                        failures.push(format!("katalogr-analytics depends on `{d}`"));
                    }
                }
            }
        }
        // (c) Dist lists analytics as a normal dependency.
        match workspace_package(&md, "katalogr-dist") {
            None => failures.push("package katalogr-dist not found".to_string()),
            Some(d) => {
                if !normal_deps(d).contains("katalogr-analytics") {
                    failures.push("katalogr-dist must depend on katalogr-analytics".to_string());
                }
            }
        }
        assert!(
            failures.is_empty(),
            "analytics layering violated:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn wrappers_are_thin() {
        let md = metadata(false);
        let p = workspace_package(&md, "katalogr").expect("package katalogr");
        let allowed: BTreeSet<String> = ["katalogr-dist", "clap", "anyhow", "serde_json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // The MCP wrapper's dependencies, which must stay optional (FEAT-067, ADR-0023).
        let optional_allowed: BTreeSet<String> =
            ["rmcp", "tokio"].iter().map(|s| s.to_string()).collect();
        let extra: Vec<_> = normal_deps(p)
            .into_iter()
            .filter(|d| !allowed.contains(d) && !optional_allowed.contains(d))
            .collect();
        assert!(
            extra.is_empty(),
            "katalogr has disallowed dependencies: {extra:?}"
        );
        let not_optional: Vec<_> = p
            .dependencies
            .iter()
            .filter(|d| d.kind == DependencyKind::Normal)
            .filter(|d| optional_allowed.contains(&d.name) && !d.optional)
            .map(|d| d.name.clone())
            .collect();
        assert!(
            not_optional.is_empty(),
            "katalogr must declare these dependencies optional: {not_optional:?}"
        );
    }

    /// R-14. The async runtime and the MCP SDK live in the `katalogr` wrapper crate, behind
    /// its default-on `mcp` feature, and never gain a network, file, process, signal or
    /// multi-thread driver on a release target.
    #[test]
    fn async_runtime_is_confined() {
        let md = metadata(false);
        let mut failures = Vec::new();
        for p in md.workspace_packages() {
            if p.name.as_str() == "katalogr" {
                continue;
            }
            for d in p
                .dependencies
                .iter()
                .filter(|d| matches!(d.kind, DependencyKind::Normal | DependencyKind::Build))
            {
                if d.name == "rmcp" || d.name == "tokio" {
                    failures.push(format!("{} depends on `{}`", p.name, d.name));
                }
            }
        }
        let k = workspace_package(&md, "katalogr").expect("package katalogr");
        for name in ["rmcp", "tokio"] {
            let deps: Vec<_> = k
                .dependencies
                .iter()
                .filter(|d| d.name == name && d.kind == DependencyKind::Normal)
                .collect();
            if deps.is_empty() || deps.iter().any(|d| !d.optional) {
                failures.push(format!("katalogr must depend on `{name}` optionally"));
            }
        }
        let mcp = k.features.get("mcp").cloned().unwrap_or_default();
        if mcp != ["dep:rmcp", "dep:tokio"] {
            failures.push(format!("katalogr feature `mcp` is {mcp:?}"));
        }
        if !k
            .features
            .get("default")
            .is_some_and(|d| d.iter().any(|f| f == "mcp"))
        {
            failures.push("katalogr feature `default` must contain `mcp`".to_string());
        }
        const FORBIDDEN: [&str; 5] = ["net", "fs", "process", "signal", "rt-multi-thread"];
        for t in release_targets() {
            let md = metadata_for(&t);
            let resolve = md.resolve.as_ref().expect("resolve");
            for n in resolve.nodes.iter().filter(|n| {
                md.packages
                    .iter()
                    .any(|p| p.id == n.id && p.name.as_str() == "tokio")
            }) {
                for f in n
                    .features
                    .iter()
                    .filter(|f| FORBIDDEN.contains(&f.as_str()))
                {
                    failures.push(format!("{t}: tokio has feature `{f}` enabled"));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "the async runtime is not confined:\n{}",
            failures.join("\n")
        );
    }

    /// The path of a `syn::Path` as `a::b::c`.
    fn path_string(p: &syn::Path) -> String {
        p.segments
            .iter()
            .map(|s| s.ident.to_string())
            .collect::<Vec<_>>()
            .join("::")
    }

    /// Every `use` tree flattened to full paths.
    fn use_paths(tree: &syn::UseTree, prefix: &str, out: &mut Vec<String>) {
        let join = |name: &str| {
            if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}::{name}")
            }
        };
        match tree {
            syn::UseTree::Path(p) => use_paths(&p.tree, &join(&p.ident.to_string()), out),
            syn::UseTree::Name(n) => out.push(join(&n.ident.to_string())),
            syn::UseTree::Rename(r) => out.push(join(&r.ident.to_string())),
            syn::UseTree::Glob(_) => out.push(join("*")),
            syn::UseTree::Group(g) => {
                for t in &g.items {
                    use_paths(t, prefix, out);
                }
            }
        }
    }

    /// Whether `path` is `prefix` or lies under it (whole segments).
    fn under(path: &str, prefix: &str) -> bool {
        path == prefix || path.starts_with(&format!("{prefix}::"))
    }

    /// Paths the MCP module must not name: the query, store, engine and model crates; the
    /// facade's query, hook and Claude modules; and network, file, process and signal APIs.
    const MCP_DENIED_PATHS: [&str; 14] = [
        "katalogr_query",
        "katalogr_store",
        "katalogr_engine",
        "katalogr_model",
        "katalogr_dist::query",
        "katalogr_dist::hooks",
        "katalogr_dist::claude",
        "std::net",
        "std::fs",
        "std::process",
        "tokio::net",
        "tokio::fs",
        "tokio::process",
        "tokio::signal",
    ];

    /// Facade or function names that edit source files, settings, hooks or the project.
    const MCP_DENIED_NAMES: [&str; 6] = [
        "replace",
        "notify",
        "add_root",
        "claude_sync",
        "skill_install",
        "session_start",
    ];

    /// The facade calls `tools.rs` makes: each exactly once.
    const MCP_FACADE_CALLS: [&str; 13] = [
        "resolve", "find", "refs", "callers", "explain", "show", "path", "map", "search",
        "dangling", "why", "index", "status",
    ];

    #[derive(Default)]
    struct McpScan {
        file: String,
        is_tools: bool,
        violations: Vec<String>,
        facade_calls: Vec<String>,
    }

    impl McpScan {
        fn violation(&mut self, line: usize, rule: &str) {
            self.violations
                .push(format!("{}:{line}: {rule}", self.file));
        }

        fn check_path(&mut self, line: usize, path: &str) {
            for denied in MCP_DENIED_PATHS {
                if under(path, denied) {
                    self.violation(line, &format!("path `{path}` is under `{denied}`"));
                }
            }
            let segs: Vec<&str> = path.split("::").collect();
            let via_facade = segs
                .iter()
                .any(|s| *s == "Katalogr" || *s == "katalogr_dist");
            if let Some(last) = segs.last()
                && via_facade
                && MCP_DENIED_NAMES.contains(last)
            {
                self.violation(
                    line,
                    &format!("path `{path}` names the editing call `{last}`"),
                );
            }
            if segs.len() >= 2 && segs[0] == "Katalogr" && !self.is_tools {
                self.violation(
                    line,
                    &format!("`{path}`: only tools.rs may call the facade"),
                );
            }
        }
    }

    impl<'ast> Visit<'ast> for McpScan {
        fn visit_item_use(&mut self, i: &'ast syn::ItemUse) {
            let mut paths = Vec::new();
            use_paths(&i.tree, "", &mut paths);
            let line = i.use_token.span.start().line;
            for p in paths {
                self.check_path(line, &p);
            }
        }

        fn visit_path(&mut self, p: &'ast syn::Path) {
            let line = p
                .segments
                .first()
                .map_or(0, |s| s.ident.span().start().line);
            self.check_path(line, &path_string(p));
            syn::visit::visit_path(self, p);
        }

        fn visit_expr_call(&mut self, c: &'ast syn::ExprCall) {
            if let syn::Expr::Path(p) = &*c.func {
                let segs: Vec<String> = p
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect();
                if segs.len() == 2 && segs[0] == "Katalogr" {
                    self.facade_calls.push(segs[1].clone());
                }
            }
            syn::visit::visit_expr_call(self, c);
        }

        fn visit_expr_method_call(&mut self, m: &'ast syn::ExprMethodCall) {
            // `tools.rs` is the only file with a facade value in hand.
            if self.is_tools && MCP_DENIED_NAMES.contains(&m.method.to_string().as_str()) {
                self.violation(
                    m.method.span().start().line,
                    &format!("method `{}` edits what the MCP wrapper must not", m.method),
                );
            }
            syn::visit::visit_expr_method_call(self, m);
        }
    }

    /// The violations of the wrapper rules in one file's source.
    fn mcp_violations(file: &str, is_tools: bool, src: &str) -> Vec<String> {
        let ast = match syn::parse_file(src) {
            Ok(a) => a,
            Err(e) => return vec![format!("{file}: parse error: {e}")],
        };
        let mut scan = McpScan {
            file: file.to_string(),
            is_tools,
            ..McpScan::default()
        };
        scan.visit_file(&ast);
        let mut out = scan.violations;
        if is_tools {
            let mut want: Vec<&str> = MCP_FACADE_CALLS.to_vec();
            want.sort_unstable();
            let mut got: Vec<&str> = scan.facade_calls.iter().map(String::as_str).collect();
            got.sort_unstable();
            if got != want {
                out.push(format!(
                    "{file}: facade calls are {got:?}, want exactly one of each of {want:?}"
                ));
            }
        }
        out
    }

    /// R-2, R-11 (ADR-0023). The MCP module is a wrapper: each tool is one facade call, and
    /// nothing under `src/mcp` reaches the core crates, edits files or opens a socket.
    #[test]
    fn mcp_is_a_wrapper() {
        let dir = workspace_root().join("crates/katalogr/src/mcp");
        let mut files = Vec::new();
        rs_files(&dir, &mut files);
        files.sort();
        assert!(!files.is_empty(), "no files under {}", dir.display());
        assert!(
            files.iter().any(|f| f.ends_with("tools.rs")),
            "no tools.rs under src/mcp"
        );
        let mut failures = Vec::new();
        for f in &files {
            let src = std::fs::read_to_string(f).expect("read source");
            let name = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
            failures.extend(mcp_violations(
                &f.display().to_string(),
                name == "tools.rs",
                &src,
            ));
        }
        assert!(
            failures.is_empty(),
            "the MCP module is not a thin wrapper:\n{}",
            failures.join("\n")
        );
    }

    /// The rules above catch what they claim to, so a pass of `mcp_is_a_wrapper` means something.
    #[test]
    fn mcp_rules_detect_violations() {
        let bad = r#"
            use std::{fs, net::TcpStream};
            use katalogr_query::resolve;
            fn f(k: &Katalogr) {
                katalogr_dist::claude::sync();
                Katalogr::replace(k);
                tokio::process::Command::new("x");
            }
        "#;
        let v = mcp_violations("other.rs", false, bad);
        for needle in [
            "std::fs",
            "std::net",
            "katalogr_query",
            "katalogr_dist::claude",
            "`Katalogr::replace`",
            "tokio::process",
        ] {
            assert!(v.iter().any(|l| l.contains(needle)), "{needle}: {v:?}");
        }
        let tools = "fn a(k: &Katalogr) { Katalogr::find(k); Katalogr::find(k); k.notify(); }";
        let v = mcp_violations("tools.rs", true, tools);
        assert!(v.iter().any(|l| l.contains("facade calls are")), "{v:?}");
        assert!(v.iter().any(|l| l.contains("method `notify`")), "{v:?}");
        let fine = "use katalogr_dist::Katalogr;\nfn a() { let _ = 1; }";
        assert!(mcp_violations("other.rs", false, fine).is_empty());
    }

    #[test]
    fn extensions_depend_on_model_only() {
        // Vacuous today: no `katalogr-ext-*` packages exist yet, so the loop body
        // does not run and the test passes.
        let md = metadata(false);
        for p in md.workspace_packages() {
            if !p.name.starts_with("katalogr-ext-") {
                continue;
            }
            let deps = normal_deps(p);
            assert!(
                deps.contains("katalogr-model"),
                "{} must depend on katalogr-model",
                p.name
            );
            for bad in ["katalogr-store", "katalogr-engine", "katalogr-dist"] {
                assert!(!deps.contains(bad), "{} must not depend on {bad}", p.name);
            }
        }
    }

    /// Whether an enabled feature of `pkg` activates the optional dependency `key`.
    fn feature_activates(pkg: &Package, enabled: &[String], key: &str) -> bool {
        let dep_form = format!("dep:{key}");
        let slash = format!("{key}/");
        let uses_dep_syntax = pkg.features.values().flatten().any(|e| *e == dep_form);
        enabled.iter().any(|f| {
            (!uses_dep_syntax && f == key)
                || pkg.features.get(f).is_some_and(|items| {
                    items.iter().any(|e| {
                        *e == dep_form || e.starts_with(&slash) || (!uses_dep_syntax && e == key)
                    })
                })
        })
    }

    /// The release targets, read from the matrix of `.github/workflows/release.yml`.
    fn release_targets() -> Vec<String> {
        let text = std::fs::read_to_string(workspace_root().join(".github/workflows/release.yml"))
            .expect("release.yml");
        text.lines()
            .filter_map(|l| l.trim().strip_prefix("- target:"))
            .map(|t| t.trim().to_string())
            .collect()
    }

    fn metadata_at(dir: &Path, target: &str, offline: bool) -> Metadata {
        let mut cmd = MetadataCommand::new();
        cmd.current_dir(dir);
        let mut opts = vec!["--filter-platform".to_string(), target.to_string()];
        if offline {
            opts.push("--offline".to_string());
        }
        cmd.other_options(opts);
        cmd.exec().expect("cargo metadata --filter-platform")
    }

    /// Metadata of the workspace resolved for one release target.
    fn metadata_for(target: &str) -> Metadata {
        metadata_at(&workspace_root(), target, false)
    }

    /// Packages built for the metadata's target: the walk from the workspace members along
    /// activated edges (`cargo metadata` lists edges to optional dependencies that no enabled
    /// feature activates, and the lockfile lists even more packages).
    fn resolved_packages(md: &Metadata) -> BTreeSet<&PackageId> {
        let resolve = md.resolve.as_ref().expect("resolve");
        let nodes: BTreeMap<_, _> = resolve.nodes.iter().map(|n| (&n.id, n)).collect();
        let pkgs: BTreeMap<_, _> = md.packages.iter().map(|p| (&p.id, p)).collect();
        let mut resolved = BTreeSet::new();
        let mut stack: Vec<&PackageId> = md.workspace_members.iter().collect();
        while let Some(id) = stack.pop() {
            if !resolved.insert(id) {
                continue;
            }
            let (node, pkg) = (nodes[id], pkgs[id]);
            let enabled: Vec<String> = node.features.iter().map(|f| f.to_string()).collect();
            for dep in &node.deps {
                let dp = pkgs[&dep.pkg];
                let activated = pkg
                    .dependencies
                    .iter()
                    .filter(|d| d.name == dp.name.as_str())
                    .any(|d| {
                        let key = d.rename.as_deref().unwrap_or(&d.name);
                        !d.optional || feature_activates(pkg, &enabled, key)
                    });
                if activated {
                    stack.push(&dep.pkg);
                }
            }
        }
        resolved
    }

    /// Every package built for the metadata's target that declares `links` or build-depends
    /// on `cc` or `cmake`.
    fn c_build_failures(md: &Metadata) -> Vec<String> {
        let mut failures = Vec::new();
        let resolved = resolved_packages(md);
        let resolve = md.resolve.as_ref().expect("resolve");
        let pkgs: BTreeMap<_, _> = md.packages.iter().map(|p| (&p.id, p)).collect();
        for node in resolve.nodes.iter().filter(|n| resolved.contains(&n.id)) {
            let p = pkgs[&node.id];
            // `rayon-core` declares `links = "rayon-core"` only so cargo refuses two versions in
            // one build; it has no C code (FEAT-072, ADR-0027).
            if p.links.is_some() && p.name != "rayon-core" {
                failures.push(format!("{} declares `links`", p.name));
            }
            // `node.deps` is already filtered by `--filter-platform`; `p.dependencies` is not
            // (it lists target-specific dependencies of every platform).
            for dep in &node.deps {
                let name = pkgs[&dep.pkg].name.as_str();
                if (name == "cc" || name == "cmake")
                    && dep
                        .dep_kinds
                        .iter()
                        .any(|k| k.kind == DependencyKind::Build)
                {
                    failures.push(format!("{} has build-dependency `{name}`", p.name));
                }
            }
        }
        failures
    }

    /// FEAT-068 R-16, ADR-0026. The load-test harness is a tool: it is not published, only the
    /// `katalogr` crate's tests may depend on it (as a dev-dependency), and the release
    /// workflow never names it.
    #[test]
    fn bench_not_shipped() {
        let md = metadata(false);
        let mut failures = Vec::new();
        match workspace_package(&md, "katalogr-bench") {
            None => failures.push("package katalogr-bench not found".to_string()),
            Some(b) => {
                if b.publish != Some(Vec::new()) {
                    failures.push("katalogr-bench must declare `publish = false`".to_string());
                }
            }
        }
        for p in md.workspace_packages() {
            for d in p.dependencies.iter().filter(|d| d.name == "katalogr-bench") {
                let allowed =
                    p.name.as_str() == "katalogr" && d.kind == DependencyKind::Development;
                if !allowed {
                    failures.push(format!(
                        "{} depends on katalogr-bench ({:?}); only katalogr's tests may, as a dev-dependency",
                        p.name, d.kind
                    ));
                }
            }
        }
        let release =
            std::fs::read_to_string(workspace_root().join(".github/workflows/release.yml"))
                .expect("release workflow");
        if release.contains("katalogr-bench") {
            failures.push(".github/workflows/release.yml names katalogr-bench".to_string());
        }
        assert!(
            failures.is_empty(),
            "the harness would be shipped:\n{}",
            failures.join("\n")
        );
    }

    #[test]
    fn no_c_build_dependencies() {
        let mut failures = BTreeSet::new();
        for t in release_targets() {
            let md = metadata_for(&t);
            for f in c_build_failures(&md) {
                failures.insert(format!("{t}: {f}"));
            }
        }
        assert!(
            failures.is_empty(),
            "C build dependencies found:\n{}",
            failures.into_iter().collect::<Vec<_>>().join("\n")
        );
    }

    #[test]
    fn release_targets_match_workflow() {
        assert_eq!(
            release_targets(),
            [
                "x86_64-unknown-linux-musl",
                "aarch64-unknown-linux-musl",
                "x86_64-apple-darwin",
                "aarch64-apple-darwin",
                "x86_64-pc-windows-msvc",
            ]
        );
    }

    #[test]
    fn target_filter_catches_target_cc() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/target-cc");
        let haiku = c_build_failures(&metadata_at(&fixture, "x86_64-unknown-haiku", true));
        assert!(
            haiku.contains(&"a has build-dependency `cc`".to_string()),
            "haiku walk missed the target-specific cc: {haiku:?}"
        );
        let musl = c_build_failures(&metadata_at(&fixture, "x86_64-unknown-linux-musl", true));
        assert!(musl.is_empty(), "musl walk reported {musl:?}");
    }

    #[test]
    fn rust_version_covers_dependencies() {
        let root: toml::Table = std::fs::read_to_string(workspace_root().join("Cargo.toml"))
            .expect("root Cargo.toml")
            .parse()
            .expect("root Cargo.toml parses");
        let declared = root["workspace"]["package"]["rust-version"]
            .as_str()
            .expect("workspace rust-version");
        let declared = pad_version(declared);
        let mut offenders = BTreeSet::new();
        for t in release_targets() {
            let md = metadata_for(&t);
            let resolved = resolved_packages(&md);
            for p in md.packages.iter().filter(|p| resolved.contains(&p.id)) {
                if let Some(rv) = &p.rust_version
                    && *rv > declared
                {
                    offenders.insert(format!("{} {} (rust-version {rv})", p.name, p.version));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "packages need a newer Rust than the workspace rust-version {declared}:\n{}",
            offenders.into_iter().collect::<Vec<_>>().join("\n")
        );
    }

    /// `1.88` -> `1.88.0`.
    fn pad_version(v: &str) -> cargo_metadata::semver::Version {
        let mut parts: Vec<&str> = v.split('.').collect();
        while parts.len() < 3 {
            parts.push("0");
        }
        parts.join(".").parse().expect("semver")
    }
}
