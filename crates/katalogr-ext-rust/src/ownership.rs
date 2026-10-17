//! Which crate, target and module a source file belongs to (FEAT-005 section 4, "Ownership").
//! Pure path arithmetic over the [`WorkspaceContext`]; nothing here reads the filesystem.

use crate::context::{CrateInfo, TargetKind, WorkspaceContext, parent_dir};
use katalogr_model::RelPath;

/// Directories whose files are each their own target (Cargo's auto-discovery).
const CONVENTIONAL: [(&str, TargetKind); 4] = [
    ("src/bin", TargetKind::Bin),
    ("tests", TargetKind::Test),
    ("examples", TargetKind::Example),
    ("benches", TargetKind::Bench),
];

/// The target a file belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerTarget {
    pub kind: TargetKind,
    pub name: String,
}

/// The result of [`owner_of`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner<'a> {
    /// The crate whose directory is the longest prefix of the path.
    pub krate: Option<&'a CrateInfo>,
    pub target: Option<OwnerTarget>,
    /// Module path segments from the target root directory; empty for the target root file.
    pub module: Vec<String>,
    /// True when the file is outside every target; the module path is then the file stem.
    pub orphan: bool,
}

impl Owner<'_> {
    /// `a::b`, or the empty string for the root module.
    pub fn module_path(&self) -> String {
        self.module.join("::")
    }
}

/// Segments of `rel` (a path inside a root directory) as a module path: `.rs` dropped, and a
/// trailing `mod` dropped (`a/mod.rs` is module `a`).
fn module_of(rel: &str) -> Vec<String> {
    let rel = rel.strip_suffix(".rs").unwrap_or(rel);
    let mut segs: Vec<String> = rel.split('/').map(str::to_string).collect();
    if segs.len() > 1 && segs.last().is_some_and(|s| s == "mod") {
        segs.pop();
    }
    segs
}

fn under<'p>(path: &'p str, dir: &str) -> Option<&'p str> {
    path.strip_prefix(dir)?.strip_prefix('/')
}

/// Owner of the file at repo-relative `path`.
pub fn owner_of<'a>(ctx: &'a WorkspaceContext, path: &RelPath) -> Owner<'a> {
    let full = path.as_str();
    let Some(krate) = ctx.crate_of(full) else {
        return orphan(None, full);
    };
    let rel = if krate.dir.is_empty() {
        full
    } else {
        &full[krate.dir.len() + 1..]
    };
    let target = |kind, name: &str| {
        Some(OwnerTarget {
            kind,
            name: name.to_string(),
        })
    };

    // 1. A target root file: the root module.
    if let Some(t) = krate.targets.iter().find(|t| t.root == rel) {
        return Owner {
            krate: Some(krate),
            target: target(t.kind, &t.name),
            module: Vec::new(),
            orphan: false,
        };
    }
    // 2. Cargo's conventional directories: each file or directory in them is a target.
    for (dir, kind) in CONVENTIONAL {
        if let Some(inside) = under(rel, dir) {
            let first = inside.split('/').next().unwrap_or(inside);
            let name = first.strip_suffix(".rs").unwrap_or(first);
            return Owner {
                krate: Some(krate),
                target: target(kind, name),
                module: module_of(inside),
                orphan: false,
            };
        }
    }
    // 3. A non-root file under `src/` (amendment 1, the exact rule): lib if `src/lib.rs`
    //    exists, else bin if `src/main.rs` exists, else an orphan.
    if let Some(inside) = under(rel, "src") {
        let pick = |kind: TargetKind, root: &str, fallback: &str| {
            let name = krate
                .targets
                .iter()
                .find(|t| t.kind == kind && t.root == root)
                .map_or(fallback, |t| t.name.as_str());
            target(kind, name)
        };
        let found = if krate.has_lib {
            Some(pick(TargetKind::Lib, "src/lib.rs", &krate.lib_name))
        } else if krate.has_main {
            Some(pick(TargetKind::Bin, "src/main.rs", &krate.name))
        } else {
            None
        };
        if let Some(t) = found {
            return Owner {
                krate: Some(krate),
                target: t,
                module: module_of(inside),
                orphan: false,
            };
        }
        return orphan(Some(krate), full);
    }
    // 4. Beside a target root declared outside `src/` (`[lib] path`, `[[bin]] path`): the
    //    longest such root directory wins, and on a tie the first target (the lib).
    let mut best: Option<(&str, &crate::context::Target)> = None;
    for t in &krate.targets {
        let root_dir = parent_dir(&t.root);
        if !root_dir.is_empty()
            && root_dir != "src"
            && under(rel, root_dir).is_some()
            && best.is_none_or(|(d, _)| root_dir.len() > d.len())
        {
            best = Some((root_dir, t));
        }
    }
    if let Some((root_dir, t)) = best {
        let inside = under(rel, root_dir).unwrap_or(rel);
        return Owner {
            krate: Some(krate),
            target: target(t.kind, &t.name),
            module: module_of(inside),
            orphan: false,
        };
    }
    orphan(Some(krate), full)
}

fn orphan<'a>(krate: Option<&'a CrateInfo>, full: &str) -> Owner<'a> {
    let file = full.rsplit('/').next().unwrap_or(full);
    Owner {
        krate,
        target: None,
        module: vec![file.strip_suffix(".rs").unwrap_or(file).to_string()],
        orphan: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context;

    fn ctx() -> WorkspaceContext {
        let root = r#"
[package]
name = "app-core"
[lib]
path = "src/lib.rs"
[[bin]]
name = "tool"
path = "src/cli.rs"
"#;
        let sub = "[package]\nname = \"sub\"\n";
        context::build(&[
            (RelPath::new("src/lib.rs").unwrap(), Vec::new()),
            (RelPath::new("crates/sub/src/lib.rs").unwrap(), Vec::new()),
            (
                RelPath::new("crates/bin-only/src/main.rs").unwrap(),
                Vec::new(),
            ),
            (
                RelPath::new("crates/bin-only/Cargo.toml").unwrap(),
                b"[package]\nname = \"bin-only\"\n".to_vec(),
            ),
            (
                RelPath::new("crates/none/Cargo.toml").unwrap(),
                b"[package]\nname = \"none\"\n".to_vec(),
            ),
            (
                RelPath::new("Cargo.toml").unwrap(),
                root.as_bytes().to_vec(),
            ),
            (
                RelPath::new("crates/sub/Cargo.toml").unwrap(),
                sub.as_bytes().to_vec(),
            ),
        ])
    }

    fn own(
        ctx: &WorkspaceContext,
        p: &str,
    ) -> (Option<String>, Option<(TargetKind, String)>, String, bool) {
        let o = owner_of(ctx, &RelPath::new(p).unwrap());
        (
            o.krate.map(|c| c.dir.clone()),
            o.target.as_ref().map(|t| (t.kind, t.name.clone())),
            o.module_path(),
            o.orphan,
        )
    }

    #[test]
    fn module_paths() {
        let c = ctx();
        let t = |k, n: &str| Some((k, n.to_string()));
        let root = Some(String::new());
        // lib root, nested modules, mod.rs
        assert_eq!(
            own(&c, "src/lib.rs"),
            (
                root.clone(),
                t(TargetKind::Lib, "app_core"),
                "".into(),
                false
            )
        );
        assert_eq!(
            own(&c, "src/a/b.rs"),
            (
                root.clone(),
                t(TargetKind::Lib, "app_core"),
                "a::b".into(),
                false
            )
        );
        assert_eq!(
            own(&c, "src/a/mod.rs"),
            (
                root.clone(),
                t(TargetKind::Lib, "app_core"),
                "a".into(),
                false
            )
        );
        assert_eq!(
            own(&c, "src/a.rs"),
            (
                root.clone(),
                t(TargetKind::Lib, "app_core"),
                "a".into(),
                false
            )
        );
        // main, and a declared bin root outside the conventional directory
        assert_eq!(
            own(&c, "src/main.rs"),
            (
                root.clone(),
                t(TargetKind::Bin, "app-core"),
                "".into(),
                false
            )
        );
        assert_eq!(
            own(&c, "src/cli.rs"),
            (root.clone(), t(TargetKind::Bin, "tool"), "".into(), false)
        );
        // bin/, tests/, examples/
        assert_eq!(
            own(&c, "src/bin/other.rs"),
            (
                root.clone(),
                t(TargetKind::Bin, "other"),
                "other".into(),
                false
            )
        );
        assert_eq!(
            own(&c, "src/bin/big/util.rs"),
            (
                root.clone(),
                t(TargetKind::Bin, "big"),
                "big::util".into(),
                false
            )
        );
        assert_eq!(
            own(&c, "tests/it.rs"),
            (root.clone(), t(TargetKind::Test, "it"), "it".into(), false)
        );
        assert_eq!(
            own(&c, "tests/common/mod.rs"),
            (
                root.clone(),
                t(TargetKind::Test, "common"),
                "common".into(),
                false
            )
        );
        assert_eq!(
            own(&c, "examples/demo.rs"),
            (
                root.clone(),
                t(TargetKind::Example, "demo"),
                "demo".into(),
                false
            )
        );
        // the longest crate directory wins
        assert_eq!(
            own(&c, "crates/sub/src/x/y.rs"),
            (
                Some("crates/sub".into()),
                t(TargetKind::Lib, "sub"),
                "x::y".into(),
                false
            )
        );
        // the exact rule: no lib.rs but a main.rs -> bin; neither -> orphan
        assert_eq!(
            own(&c, "crates/bin-only/src/x.rs"),
            (
                Some("crates/bin-only".into()),
                t(TargetKind::Bin, "bin-only"),
                "x".into(),
                false
            )
        );
        assert_eq!(
            own(&c, "crates/none/src/x.rs"),
            (Some("crates/none".into()), None, "x".into(), true)
        );
        // orphans: outside every target, and outside every crate
        assert_eq!(
            own(&c, "build.rs"),
            (root.clone(), None, "build".into(), true)
        );
        assert_eq!(
            own(&c, "crates/sub/scripts/gen.rs"),
            (Some("crates/sub".into()), None, "gen".into(), true)
        );
        assert_eq!(
            own(&WorkspaceContext::default(), "src/lib.rs"),
            (None, None, "lib".into(), true)
        );
    }
}
