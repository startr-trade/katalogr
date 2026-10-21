//! Synthetic Rust corpus for the indexing benchmarks (FEAT-072). Generated identifiers only
//! (`c<n>`, `m<n>`, `f<c>_<m>_<n>`); no scanned repository's names, paths or code appear.

use std::fmt::Write as _;
use std::path::Path;

/// A workspace of `crates` crates `c<i>`, each with `modules` modules `m<j>` of `fns` functions.
/// Crate `c<i>` depends on `c<i-1>` by path and calls its functions. Files per crate:
/// `modules + 3` (manifest, `lib.rs`, README and the modules), so 20 crates of 40 modules give
/// 860 files and 50 give 2,150.
pub fn rust_corpus(root: &Path, crates: usize, modules: usize, fns: usize) {
    let mut ws = String::from("[workspace]\nmembers = [");
    for i in 0..crates {
        let _ = write!(ws, "{}\"c{i}\"", if i == 0 { "" } else { ", " });
    }
    ws.push_str("]\n");
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join("Cargo.toml"), ws).unwrap();

    for i in 0..crates {
        let dir = root.join(format!("c{i}"));
        let src = dir.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut manifest = format!(
            "[package]\nname = \"c{i}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n"
        );
        if i > 0 {
            let _ = writeln!(manifest, "c{} = {{ path = \"../c{}\" }}", i - 1, i - 1);
        }
        std::fs::write(dir.join("Cargo.toml"), manifest).unwrap();
        std::fs::write(
            dir.join("README.md"),
            format!(
                "# c{i}\n\nSynthetic crate {i}.\n\n## Modules\n\nModules m0 to m{}.\n",
                modules - 1
            ),
        )
        .unwrap();
        let mut lib = String::new();
        for j in 0..modules {
            let _ = writeln!(lib, "pub mod m{j};");
        }
        std::fs::write(src.join("lib.rs"), lib).unwrap();
        for j in 0..modules {
            std::fs::write(src.join(format!("m{j}.rs")), module(i, j, fns)).unwrap();
        }
    }
}

fn module(c: usize, m: usize, fns: usize) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "//! Module m{m} of crate c{c}.\n");
    let _ = writeln!(s, "/// A value of module m{m}.");
    let _ = writeln!(
        s,
        "pub struct Item{c}_{m} {{\n    pub id: u32,\n    pub name: String,\n}}\n"
    );
    let _ = writeln!(s, "/// Shapes of module m{m}.");
    let _ = writeln!(
        s,
        "pub trait Shape{c}_{m} {{\n    fn area{c}_{m}(&self) -> u32;\n}}\n"
    );
    let _ = writeln!(
        s,
        "impl Shape{c}_{m} for Item{c}_{m} {{\n    fn area{c}_{m}(&self) -> u32 {{\n        std::cmp::max(self.id, 1)\n    }}\n}}\n"
    );
    for k in 0..fns {
        let _ = writeln!(s, "/// Function {k} of m{m}.");
        let _ = writeln!(s, "pub fn f{c}_{m}_{k}(x: u32) -> u32 {{");
        // Names every file shares (prelude and std paths): the keys that made the stitch
        // quadratic.
        let _ = writeln!(s, "    let mut v: Vec<u32> = Vec::new();");
        let _ = writeln!(s, "    let label = String::from(\"f{c}_{m}_{k}\");");
        let _ = writeln!(s, "    let o: Option<u32> = Some(x);");
        let _ = writeln!(s, "    v.push(o.unwrap_or(0) + label.len() as u32);");
        let _ = writeln!(s, "    let sibling = f{c}_{m}_{}(x);", (k + 1) % fns);
        if c > 0 {
            let _ = writeln!(s, "    let dep = c{}::m{m}::f{}_{m}_{k}(x);", c - 1, c - 1);
        } else {
            let _ = writeln!(s, "    let dep = 0;");
        }
        let _ = writeln!(s, "    std::cmp::max(sibling + dep, v.len() as u32)");
        let _ = writeln!(s, "}}\n");
    }
    let _ = writeln!(s, "#[cfg(test)]\nmod tests {{\n    use super::*;\n");
    let _ = writeln!(
        s,
        "    #[test]\n    fn t{c}_{m}() {{\n        assert!(f{c}_{m}_0(1) >= 1);\n    }}\n}}"
    );
    s
}
