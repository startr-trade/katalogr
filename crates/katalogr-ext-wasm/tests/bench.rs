//! FEAT-016 R-9 budgets. Ignored in debug runs; run with
//! `cargo test --release -p katalogr-ext-wasm --test bench -- --ignored --nocapture`.

mod bench {
    use katalogr_ext_wasm::WasmPackages;
    use katalogr_ext_wasm::rust::{CrateRef, apply, mentions_wasm_bindgen};
    use katalogr_model::{
        ArtifactName, ArtifactNode, Confidence, Fact, Fragment, Link, LinkKind, Name, Provenance,
        Segment, Span, SymbolName, SymbolNode,
    };
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    const RUNS: usize = 7;

    fn span() -> Span {
        Span {
            start_line: 1,
            start_col: 1,
            end_line: 1,
            end_col: 2,
        }
    }

    /// `total` symbols: `wasm` of them `#[wasm_bindgen] pub fn`, the rest plain functions; every
    /// tenth wasm item is a method of one of a hundred `#[wasm_bindgen] impl` blocks.
    fn fragment(total: usize, wasm: usize) -> Fragment {
        let artifact = ArtifactName::new("fx", "src/lib.rs").unwrap();
        let provenance = Provenance {
            repo: "fx".into(),
            path: artifact.path.clone(),
            revision: "r".into(),
        };
        let mut symbols = Vec::with_capacity(total);
        let mut links = Vec::new();
        let sym = |segments: Vec<Segment>, kind: &str, attrs: &[&str]| {
            let mut facts = BTreeMap::new();
            facts.insert("vis".to_string(), Fact::Str("pub".into()));
            facts.insert("signature".to_string(), Fact::Str("pub fn f()".into()));
            if !attrs.is_empty() {
                facts.insert(
                    "attrs".to_string(),
                    Fact::List(attrs.iter().map(|a| Fact::Str(a.to_string())).collect()),
                );
            }
            SymbolNode {
                name: SymbolName {
                    artifact: artifact.clone(),
                    segments,
                    ordinal: 1,
                },
                kind: kind.into(),
                span: span(),
                facts,
            }
        };
        let seg = |k: &str, n: String| Segment {
            kind: k.into(),
            name: n,
        };
        let impls = 100;
        for i in 0..impls {
            symbols.push(sym(
                vec![seg("impl", format!("C{i}"))],
                "impl",
                &["wasm_bindgen"],
            ));
        }
        let mut methods = 0;
        for i in 0..total - impls {
            if i < wasm {
                if i % 10 == 0 && methods < wasm / 2 {
                    methods += 1;
                    let parent = i % impls;
                    let p = symbols[parent].name.clone();
                    let m = sym(
                        vec![p.segments[0].clone(), seg("fn", format!("m{i}"))],
                        "fn",
                        &[],
                    );
                    links.push(Link {
                        from: p.id(),
                        to: m.name.id(),
                        kind: LinkKind("contains".into()),
                        confidence: Confidence::Extracted,
                        span: Some(span()),
                        provenance: provenance.clone(),
                    });
                    symbols.push(m);
                } else {
                    symbols.push(sym(
                        vec![seg("fn", format!("f{i}"))],
                        "fn",
                        &["wasm_bindgen(js_name = jsName)"],
                    ));
                }
            } else {
                symbols.push(sym(vec![seg("fn", format!("g{i}"))], "fn", &["inline"]));
            }
        }
        if wasm == 0 {
            for s in &mut symbols {
                s.facts.remove("attrs");
            }
        }
        Fragment {
            provenance,
            extension: "rust".into(),
            artifact: ArtifactNode {
                name: artifact,
                facts: BTreeMap::new(),
            },
            symbols,
            links,
            refs: Vec::new(),
            exports: Vec::new(),
            bindings: Vec::new(),
        }
    }

    fn best(mut f: impl FnMut() -> Duration) -> Duration {
        (0..RUNS).map(|_| f()).min().unwrap()
    }

    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn wasm_rust_pass_10k_symbols() {
        let krate = CrateRef {
            name: "demo-wasm",
            dir: "",
        };
        let base = fragment(10_000, 1_000);
        assert_eq!(base.symbols.len(), 10_000);
        let took = best(|| {
            let mut f = base.clone();
            let t = Instant::now();
            assert!(mentions_wasm_bindgen(&f));
            apply(&mut f, Some(krate));
            let d = t.elapsed();
            assert!(f.bindings.len() >= 1_000, "{}", f.bindings.len());
            d
        });
        println!("wasm_rust_pass_10k_symbols (1,000 wasm items): {took:?}");
        assert!(took < Duration::from_millis(20), "{took:?}");

        // Control: no wasm item, so the pass is skipped after the guard.
        let plain = fragment(10_000, 0);
        let took = best(|| {
            let t = Instant::now();
            assert!(!mentions_wasm_bindgen(&plain));
            t.elapsed()
        });
        println!("wasm_rust_pass_10k_symbols control (0 wasm items): {took:?}");
        assert!(took < Duration::from_millis(2), "{took:?}");
    }

    #[test]
    #[ignore = "benchmark: run in release with --ignored"]
    fn wasm_context_build_400_manifests() {
        let mut cargo = Vec::new();
        let mut pjs = Vec::new();
        for i in 0..200 {
            let wasm = if i % 2 == 0 {
                "wasm-bindgen = \"0.2\"\n"
            } else {
                ""
            };
            cargo.push((
                format!("crates/c{i}/Cargo.toml"),
                format!(
                    "[package]\nname = \"crate-{i}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\n\
                     [dependencies]\nserde = \"1\"\n{wasm}"
                ),
            ));
            pjs.push((
                format!("web/w{i}/package.json"),
                format!(
                    r#"{{ "name": "web-{i}", "scripts": {{ "build": "wasm-pack build ../../crates/c{i} --out-dir ../../web/w{i}/gen --scope s{i} && tsc", "test": "jest" }}, "dependencies": {{ "crate-{i}": "file:../../crates/c{i}/pkg", "react": "^18" }} }}"#
                ),
            ));
        }
        let mut packages = 0;
        let took = best(|| {
            let t = Instant::now();
            let p = WasmPackages::build(&cargo, &pjs);
            let d = t.elapsed();
            packages = p.packages.len();
            d
        });
        println!("wasm_context_build_400_manifests ({packages} packages): {took:?}");
        assert!(packages >= 100, "{packages}");
        assert!(took < Duration::from_millis(50), "{took:?}");
    }
}
