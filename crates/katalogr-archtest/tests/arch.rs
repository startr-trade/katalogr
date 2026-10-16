//! Architecture tests: neutrality of the core, thin wrappers, extension
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
    fn wrappers_are_thin() {
        let md = metadata(false);
        let p = workspace_package(&md, "katalogr").expect("package katalogr");
        let allowed: BTreeSet<String> = ["katalogr-dist", "clap", "anyhow", "serde_json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let extra: Vec<_> = normal_deps(p).difference(&allowed).cloned().collect();
        assert!(
            extra.is_empty(),
            "katalogr has disallowed dependencies: {extra:?}"
        );
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

    #[test]
    fn no_c_build_dependencies() {
        let md = metadata(true);
        let mut failures = Vec::new();
        // `cargo metadata` lists edges to optional dependencies that no enabled
        // feature activates (and the lockfile lists even more packages), so walk
        // the graph from the workspace members and follow only activated edges.
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
        for p in md.packages.iter().filter(|p| resolved.contains(&p.id)) {
            if p.links.is_some() {
                failures.push(format!("{} declares `links`", p.name));
            }
            for d in &p.dependencies {
                if d.kind == DependencyKind::Build && (d.name == "cc" || d.name == "cmake") {
                    failures.push(format!("{} has build-dependency `{}`", p.name, d.name));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "C build dependencies found:\n{}",
            failures.join("\n")
        );
    }
}
