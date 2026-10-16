//! FEAT-005 tests, run directly against the extension (the fixture runner arrives with T9).

use katalogr_ext_rust::{RustExtractor, TargetKind, WorkspaceContext};
use katalogr_model::{ExtractInput, Extractor, Name, RelPath};

fn file(path: &str, text: &str) -> (RelPath, Vec<u8>) {
    (RelPath::new(path).unwrap(), text.as_bytes().to_vec())
}

const ROOT: &str = r#"
[workspace]
members = ["crates/*"]

[workspace.dependencies]
serde = "1"
core-lib = { path = "crates/core-lib" }
renamed = { package = "real-name", version = "2" }
"#;

const CORE: &str = r#"
[package]
name = "core-lib"
version = "0.1.0"
edition = "2024"

[dependencies]
serde.workspace = true
"#;

const APP: &str = r#"
[package]
name = "app"
version = "0.2.0"
edition = "2024"

[lib]
name = "app_lib"

[[bin]]
name = "app-cli"
path = "src/cli.rs"

[dependencies]
core-lib = { workspace = true }
local = { path = "../core-lib" }
pkg = { package = "other-pkg", version = "3" }
renamed = { workspace = true }
tokio = "1"

[dev-dependencies]
serde = { workspace = true }
"#;

fn files() -> Vec<(RelPath, Vec<u8>)> {
    vec![
        file("crates/core-lib/Cargo.toml", CORE),
        file("Cargo.toml", ROOT),
        file("crates/app/Cargo.toml", APP),
        file("crates/app/src/lib.rs", "// ignored: not a manifest"),
    ]
}

mod rust {
    use super::*;

    #[test]
    fn workspace_crates_and_deps() {
        let bytes = RustExtractor.build_context(&files()).unwrap();
        // Deterministic: file order does not matter.
        let mut rev = files();
        rev.reverse();
        assert_eq!(bytes, RustExtractor.build_context(&rev).unwrap());

        let ctx = WorkspaceContext::from_bytes(&bytes).unwrap();
        let dirs: Vec<_> = ctx.crates.iter().map(|c| c.dir.as_str()).collect();
        assert_eq!(
            dirs,
            ["crates/app", "crates/core-lib"],
            "virtual root has no crate"
        );

        let app = ctx.crate_at("crates/app").unwrap();
        assert_eq!(
            (app.name.as_str(), app.lib_name.as_str()),
            ("app", "app_lib")
        );
        let targets: Vec<_> = app
            .targets
            .iter()
            .map(|t| (t.kind, t.name.as_str(), t.root.as_str()))
            .collect();
        assert_eq!(
            targets,
            [
                (TargetKind::Lib, "app_lib", "src/lib.rs"),
                (TargetKind::Bin, "app", "src/main.rs"),
                (TargetKind::Bin, "app-cli", "src/cli.rs"),
            ]
        );
        let deps: Vec<_> = app
            .deps
            .iter()
            .map(|d| (d.key.as_str(), d.package.as_str(), d.path.as_deref()))
            .collect();
        assert_eq!(
            deps,
            [
                // workspace = true: the root's path, relative to the root
                ("core_lib", "core-lib", Some("crates/core-lib")),
                // a plain path dependency, relative to the crate
                ("local", "local", Some("crates/core-lib")),
                // package = rename: the key is the name used in code
                ("pkg", "other-pkg", None),
                // workspace = true on a rename
                ("renamed", "real-name", None),
                // a dev dependency from the workspace, and a registry dependency
                ("serde", "serde", None),
                ("tokio", "tokio", None),
            ]
        );
        let core = ctx.crate_at("crates/core-lib").unwrap();
        assert_eq!(core.lib_name, "core_lib");
        assert_eq!(core.deps.len(), 1);
        assert_eq!(core.deps[0].key, "serde");
    }

    #[test]
    fn manifest_fragment() {
        let ctx = RustExtractor.build_context(&files()).unwrap();
        let path = RelPath::new("crates/app/Cargo.toml").unwrap();
        let f = RustExtractor
            .extract(&ExtractInput {
                repo: "r",
                path: &path,
                revision: "rev",
                content: APP.as_bytes(),
                context: &ctx,
            })
            .unwrap();
        assert_eq!(f.symbols.len(), 1);
        assert_eq!(
            f.symbols[0].name.canonical(),
            "r:crates/app/Cargo.toml#crate:app"
        );
        assert_eq!(f.exports[0].key.key, "crates/app");
        let cands: Vec<_> = f
            .refs
            .iter()
            .map(|r| {
                (
                    r.candidates[0].namespace.as_str(),
                    r.candidates[0].key.as_str(),
                )
            })
            .collect();
        assert_eq!(
            cands,
            [
                ("rs-crate-dir", "crates/core-lib"),
                ("rs-crate-dir", "crates/core-lib"),
                ("rs-package", "other-pkg"),
                ("rs-package", "real-name"),
                ("rs-package", "serde"),
                ("rs-package", "tokio"),
            ]
        );
    }
}
