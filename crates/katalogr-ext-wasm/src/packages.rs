//! Package discovery: which crates build to wasm packages, where the glue lands, and under
//! which npm names it is imported (FEAT-016 section 6.5).

use crate::pkg::{join_norm, normalise, parent_dir};
use crate::{CARGO_TOML, DEFAULT_OUT_DIR};
use serde::{Deserialize, Serialize};

/// Every wasm package of one repository. Serialised as compact JSON; fields are declared in
/// key order and every vector is sorted, so equal inputs give equal bytes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WasmPackages {
    /// Sorted by `key`; keys are unique.
    pub packages: Vec<WasmPackage>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WasmPackage {
    /// Repo-relative crate directory, when the crate's `Cargo.toml` is in the repository.
    pub crate_dir: Option<String>,
    /// `pkg::normalise` of the crate (or package) name.
    pub key: String,
    /// npm names that import it (crate name, scoped name, `file:` dependency aliases); sorted, unique.
    pub npm_names: Vec<String>,
    /// Where wasm-pack writes (or wrote) the glue; sorted, unique.
    pub outputs: Vec<Output>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Output {
    /// Repo-relative output directory.
    pub dir: String,
    /// Glue file stem: `<stem>.js`, `<stem>.d.ts`, `<stem>_bg.js`.
    pub stem: String,
}

/// A crate found in a `Cargo.toml`, with what the scripts need.
struct Krate {
    dir: String,
    name: String,
    lib: String,
}

const DEP_TABLES: [&str; 1] = ["dependencies"];

fn has_wasm_bindgen(deps: &toml::Table) -> bool {
    deps.iter().any(|(k, v)| {
        k == "wasm-bindgen"
            || v.as_table()
                .and_then(|t| t.get("package"))
                .and_then(|p| p.as_str())
                == Some("wasm-bindgen")
    })
}

fn is_wasm_crate(doc: &toml::Table) -> bool {
    for t in DEP_TABLES {
        if doc
            .get(t)
            .and_then(|v| v.as_table())
            .is_some_and(has_wasm_bindgen)
        {
            return true;
        }
    }
    doc.get("target")
        .and_then(|v| v.as_table())
        .is_some_and(|targets| {
            targets.values().any(|t| {
                t.get("dependencies")
                    .and_then(|v| v.as_table())
                    .is_some_and(has_wasm_bindgen)
            })
        })
}

fn blank() -> WasmPackage {
    WasmPackage {
        crate_dir: None,
        key: String::new(),
        npm_names: Vec::new(),
        outputs: Vec::new(),
    }
}

/// One `wasm-pack build` invocation read from a script.
#[derive(Debug, Default, PartialEq, Eq)]
struct Invocation {
    path: Option<String>,
    out_dir: Option<String>,
    out_name: Option<String>,
    scope: Option<String>,
}

const VALUE_OPTS: [&str; 6] = [
    "--out-dir",
    "--out-name",
    "--scope",
    "--target",
    "--mode",
    "--profile",
];

fn short_to_long(t: &str) -> &str {
    match t {
        "-d" => "--out-dir",
        "-s" => "--scope",
        "-t" => "--target",
        o => o,
    }
}

/// Tokenise a `package.json` script (§6.5 step 2); text only, nothing is executed.
fn wasm_pack_invocations(script: &str) -> Vec<Invocation> {
    let normalised = script
        .replace("&&", ";")
        .replace("||", ";")
        .replace('|', ";");
    let mut out = Vec::new();
    for cmd in normalised.split(';') {
        let toks: Vec<String> = cmd
            .split_whitespace()
            .map(|t| t.replace(['\'', '"'], ""))
            .collect();
        let Some(i) = toks.iter().position(|t| t == "wasm-pack") else {
            continue;
        };
        if toks.get(i + 1).map(String::as_str) != Some("build") {
            continue;
        }
        let mut inv = Invocation::default();
        let mut it = toks[i + 2..].iter();
        while let Some(t) = it.next() {
            if t == "--" {
                break;
            }
            if t.starts_with('-') {
                let (opt, inline) = match t.split_once('=') {
                    Some((o, v)) if o.starts_with("--") => (o, Some(v.to_string())),
                    _ => (t.as_str(), None),
                };
                let opt = short_to_long(opt);
                if VALUE_OPTS.contains(&opt) {
                    let v = inline.or_else(|| it.next().cloned());
                    match opt {
                        "--out-dir" => inv.out_dir = v,
                        "--out-name" => inv.out_name = v,
                        "--scope" => inv.scope = v,
                        _ => {}
                    }
                }
            } else if inv.path.is_none() {
                inv.path = Some(t.clone());
            }
        }
        out.push(inv);
    }
    out
}

fn add_unique<T: Ord>(v: &mut Vec<T>, x: T) {
    if let Err(i) = v.binary_search(&x) {
        v.insert(i, x);
    }
}

impl WasmPackages {
    /// Build from the repository's `Cargo.toml` and `package.json` files (repo-relative path,
    /// content). Unparsable files are skipped, never an error.
    pub fn build(cargo: &[(String, String)], package_json: &[(String, String)]) -> WasmPackages {
        let mut cargo: Vec<&(String, String)> = cargo.iter().collect();
        cargo.sort_by(|a, b| a.0.cmp(&b.0));
        let mut pjs: Vec<(&str, serde_json::Value)> = package_json
            .iter()
            .filter_map(|(p, c)| Some((parent_dir(p), serde_json::from_str(c).ok()?)))
            .collect();
        pjs.sort_by(|a, b| a.0.cmp(b.0));

        let mut pkgs: Vec<WasmPackage> = Vec::new();
        let mut krates: Vec<Krate> = Vec::new();

        // 1. Crates.
        for (path, text) in cargo {
            if path.rsplit('/').next() != Some(CARGO_TOML) {
                continue;
            }
            let Ok(doc) = text.parse::<toml::Table>() else {
                continue;
            };
            let Some(name) = doc
                .get("package")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
            else {
                continue;
            };
            if !is_wasm_crate(&doc) {
                continue;
            }
            let dir = parent_dir(path).to_string();
            let lib = doc
                .get("lib")
                .and_then(|l| l.get("name"))
                .and_then(|n| n.as_str())
                .map_or_else(|| name.replace('-', "_"), str::to_string);
            let mut p = blank();
            p.crate_dir = Some(dir.clone());
            p.key = normalise(name);
            p.npm_names = vec![name.to_string()];
            p.outputs = vec![Output {
                dir: join_norm(&dir, DEFAULT_OUT_DIR).unwrap_or_default(),
                stem: lib.clone(),
            }];
            pkgs.push(p);
            krates.push(Krate {
                dir,
                name: name.to_string(),
                lib,
            });
        }

        // 2. Scripts.
        for (d, v) in &pjs {
            let Some(scripts) = v.get("scripts").and_then(|s| s.as_object()) else {
                continue;
            };
            for script in scripts.values().filter_map(|s| s.as_str()) {
                for inv in wasm_pack_invocations(script) {
                    let Some(cdir) = join_norm(d, inv.path.as_deref().unwrap_or(".")) else {
                        continue;
                    };
                    let Some(k) = krates.iter().find(|k| k.dir == cdir) else {
                        continue;
                    };
                    let Some(p) = pkgs
                        .iter_mut()
                        .find(|p| p.crate_dir.as_deref() == Some(&k.dir))
                    else {
                        continue;
                    };
                    if let Some(dir) =
                        join_norm(&cdir, inv.out_dir.as_deref().unwrap_or(DEFAULT_OUT_DIR))
                    {
                        let stem = inv.out_name.clone().unwrap_or_else(|| k.lib.clone());
                        add_unique(&mut p.outputs, Output { dir, stem });
                    }
                    if let Some(s) = &inv.scope {
                        add_unique(&mut p.npm_names, format!("@{s}/{}", k.name));
                    }
                }
            }
        }

        // 3. Committed glue packages.
        for (d, v) in &pjs {
            let Some(name) = v.get("name").and_then(|n| n.as_str()) else {
                continue;
            };
            let Some(entry) = v.get("files").and_then(|f| f.as_array()).and_then(|f| {
                f.iter()
                    .filter_map(|e| e.as_str())
                    .find(|e| e.ends_with("_bg.wasm"))
            }) else {
                continue;
            };
            let stem_path = entry.strip_suffix("_bg.wasm").unwrap_or(entry);
            let Some(full) = join_norm(d, stem_path) else {
                continue;
            };
            let out = Output {
                dir: parent_dir(&full).to_string(),
                stem: full.rsplit('/').next().unwrap_or("").to_string(),
            };
            let key = normalise(name);
            let i = match pkgs.iter().position(|p| p.key == key) {
                Some(i) => i,
                None => {
                    let mut p = blank();
                    p.key = key;
                    pkgs.push(p);
                    pkgs.len() - 1
                }
            };
            add_unique(&mut pkgs[i].outputs, out);
            add_unique(&mut pkgs[i].npm_names, name.to_string());
        }

        // 4. Path dependencies.
        for (d, v) in &pjs {
            for table in [
                "dependencies",
                "devDependencies",
                "peerDependencies",
                "optionalDependencies",
            ] {
                let Some(deps) = v.get(table).and_then(|t| t.as_object()) else {
                    continue;
                };
                for (n, spec) in deps {
                    let Some(p) = spec
                        .as_str()
                        .and_then(|s| s.strip_prefix("file:").or_else(|| s.strip_prefix("link:")))
                    else {
                        continue;
                    };
                    let p = p.trim_end_matches('/');
                    let q = join_norm(d, p);
                    let hit = q.as_ref().and_then(|q| {
                        pkgs.iter()
                            .position(|pk| pk.outputs.iter().any(|o| &o.dir == q))
                    });
                    if let Some(i) = hit {
                        add_unique(&mut pkgs[i].npm_names, n.clone());
                    } else if p.rsplit('/').next() == Some(DEFAULT_OUT_DIR) {
                        let key = normalise(n);
                        let i = match pkgs.iter().position(|pk| pk.key == key) {
                            Some(i) => i,
                            None => {
                                let mut np = blank();
                                np.key = key;
                                pkgs.push(np);
                                pkgs.len() - 1
                            }
                        };
                        add_unique(&mut pkgs[i].npm_names, n.clone());
                    }
                }
            }
        }

        // 5. Sort and merge duplicate keys.
        pkgs.sort_by(|a, b| {
            a.key
                .cmp(&b.key)
                .then_with(|| a.crate_dir.cmp(&b.crate_dir))
        });
        let mut merged: Vec<WasmPackage> = Vec::new();
        for p in pkgs {
            match merged.last_mut() {
                Some(m) if m.key == p.key => {
                    if m.crate_dir.is_none() {
                        m.crate_dir = p.crate_dir;
                    }
                    m.npm_names.extend(p.npm_names);
                    m.outputs.extend(p.outputs);
                }
                _ => merged.push(p),
            }
        }
        for m in &mut merged {
            m.npm_names.sort();
            m.npm_names.dedup();
            m.outputs.sort();
            m.outputs.dedup();
        }
        WasmPackages { packages: merged }
    }

    /// The package an import refers to: `spec` is the specifier as written, `modules` the
    /// `ts-file` candidate paths ext-ts resolved for it (in order).
    pub fn for_import(&self, spec: &str, modules: &[&str]) -> Option<&WasmPackage> {
        if !spec.starts_with('.') && !spec.starts_with('/') {
            let mut parts = spec.split('/');
            let first = parts.next().unwrap_or("");
            let name = if first.starts_with('@') {
                format!("{first}/{}", parts.next().unwrap_or(""))
            } else {
                first.to_string()
            };
            return self.packages.iter().find(|p| p.npm_names.contains(&name));
        }
        modules.iter().find_map(|m| {
            let dir = parent_dir(m);
            self.packages
                .iter()
                .find(|p| p.outputs.iter().any(|o| o.dir == dir))
        })
    }

    /// When `path` is a generated glue file (`<stem>.js|.mjs|.cjs|.d.ts` or `<stem>_bg.js`
    /// directly in an output dir), its package. `<stem>_bg.wasm.d.ts` is never glue.
    pub fn glue_of(&self, path: &str) -> Option<&WasmPackage> {
        let dir = parent_dir(path);
        let file = path.rsplit('/').next().unwrap_or(path);
        self.packages.iter().find(|p| {
            p.outputs.iter().any(|o| {
                o.dir == dir
                    && file.strip_prefix(o.stem.as_str()).is_some_and(|rest| {
                        matches!(rest, ".js" | ".mjs" | ".cjs" | ".d.ts" | "_bg.js")
                    })
            })
        })
    }

    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }
}

#[cfg(test)]
fn s(a: &str, b: &str) -> (String, String) {
    (a.to_string(), b.to_string())
}

#[cfg(test)]
const CARGO: &str = "[package]\nname = \"demo-wasm\"\n[dependencies]\nwasm-bindgen = \"0.2\"\n";

#[test]
fn default_pkg_output() {
    let p = WasmPackages::build(&[s("Cargo.toml", CARGO)], &[]);
    assert_eq!(p.packages.len(), 1);
    let k = &p.packages[0];
    assert_eq!(k.key, "demo_wasm");
    assert_eq!(k.crate_dir.as_deref(), Some(""));
    assert_eq!(k.npm_names, ["demo-wasm"]);
    assert_eq!(
        k.outputs,
        [Output {
            dir: "pkg".into(),
            stem: "demo_wasm".into()
        }]
    );
    assert!(p.glue_of("pkg/demo_wasm.js").is_some());
    assert!(p.glue_of("pkg/demo_wasm_bg.js").is_some());
    assert!(p.glue_of("pkg/demo_wasm.d.ts").is_some());
    assert!(p.glue_of("pkg/demo_wasm_bg.wasm.d.ts").is_none());
    assert!(p.glue_of("pkg/other.js").is_none());
    assert!(p.glue_of("src/demo_wasm.js").is_none());
    let lib = "[package]\nname = \"a\"\n[lib]\nname = \"zed\"\n[target.'cfg(a)'.dependencies]\nwasm-bindgen = \"0.2\"\n";
    let p = WasmPackages::build(&[s("c/Cargo.toml", lib)], &[]);
    assert_eq!(
        p.packages[0].outputs[0],
        Output {
            dir: "c/pkg".into(),
            stem: "zed".into()
        }
    );
}

#[test]
fn scripts_override_out_dir_name_scope() {
    let cargo = "[package]\nname = \"engine-core\"\n[dependencies]\nwasm-bindgen = \"0.2\"\n";
    let pj = r#"{"scripts":{"build":"wasm-pack build crates/engine-core --out-dir ../../web/gen --out-name engine --scope acme && echo done","x":"wasm-pack build --target web 'crates/none'"}}"#;
    let p = WasmPackages::build(
        &[s("crates/engine-core/Cargo.toml", cargo)],
        &[s("package.json", pj)],
    );
    let k = &p.packages[0];
    assert!(k.outputs.contains(&Output {
        dir: "web/gen".into(),
        stem: "engine".into()
    }));
    assert!(k.npm_names.contains(&"@acme/engine-core".to_string()));
    assert_eq!(p.packages.len(), 1);
    assert!(p.for_import("@acme/engine-core", &[]).is_some());
    assert!(
        p.for_import(
            "./gen/engine.js",
            &["web/gen/engine.js", "web/gen/engine.ts"]
        )
        .is_some()
    );
    assert!(p.glue_of("web/gen/engine.js").is_some());
    assert_eq!(
        wasm_pack_invocations("wasm-pack build -d out -- --features x"),
        vec![Invocation {
            out_dir: Some("out".into()),
            ..Invocation::default()
        }]
    );
}

#[test]
fn file_dependency_names_package() {
    let pj = r#"{"dependencies":{"my-alias":"file:../pkg","far-away":"link:../core/pkg","other":"^1.0.0"}}"#;
    let p = WasmPackages::build(&[s("Cargo.toml", CARGO)], &[s("web/package.json", pj)]);
    let k = p.packages.iter().find(|k| k.key == "demo_wasm").unwrap();
    assert!(k.npm_names.contains(&"my-alias".to_string()));
    let far = p.packages.iter().find(|k| k.key == "far_away").unwrap();
    assert_eq!(far.crate_dir, None);
    assert_eq!(far.npm_names, ["far-away"]);
    assert!(far.outputs.is_empty());
    assert!(p.packages.iter().all(|k| k.key != "other"));
    assert!(p.for_import("my-alias", &[]).is_some());
}

#[test]
fn committed_package_json_detected() {
    let pj = r#"{"name":"@acme/demo-wasm","files":["demo_wasm_bg.wasm","demo_wasm.js"]}"#;
    let p = WasmPackages::build(&[], &[s("pkg/package.json", pj)]);
    let k = &p.packages[0];
    assert_eq!(k.key, "demo_wasm");
    assert_eq!(k.crate_dir, None);
    assert_eq!(
        k.outputs,
        [Output {
            dir: "pkg".into(),
            stem: "demo_wasm".into()
        }]
    );
    assert!(p.glue_of("pkg/demo_wasm.js").is_some());
    let m = WasmPackages::build(&[s("Cargo.toml", CARGO)], &[s("pkg/package.json", pj)]);
    assert_eq!(m.packages.len(), 1);
    assert_eq!(m.packages[0].crate_dir.as_deref(), Some(""));
}

#[test]
fn non_wasm_crate_ignored() {
    let plain = "[package]\nname = \"plain\"\n[dependencies]\nserde = \"1\"\n";
    let p = WasmPackages::build(
        &[s("Cargo.toml", plain), s("x/Cargo.toml", "not toml {")],
        &[s("package.json", "{bad")],
    );
    assert!(p.is_empty());
    let renamed = "[package]\nname = \"r\"\n[dependencies]\nwb = { package = \"wasm-bindgen\", version = \"0.2\" }\n";
    assert!(!WasmPackages::build(&[s("Cargo.toml", renamed)], &[]).is_empty());
}

#[test]
fn serialised_sorted() {
    let a = [
        s("a/Cargo.toml", CARGO),
        s("b/Cargo.toml", &CARGO.replace("demo-wasm", "zeta")),
    ];
    let b = [a[1].clone(), a[0].clone()];
    let pj = r#"{"dependencies":{"x":"file:../a/pkg"}}"#;
    let x = serde_json::to_vec(&WasmPackages::build(&a, &[s("w/package.json", pj)])).unwrap();
    let y = serde_json::to_vec(&WasmPackages::build(&b, &[s("w/package.json", pj)])).unwrap();
    assert_eq!(x, y);
}
