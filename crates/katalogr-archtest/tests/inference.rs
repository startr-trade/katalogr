//! No inference model of any kind in the dependency graph (FEAT-025 R-10, ADR-0007).

mod ci {
    use std::collections::BTreeSet;
    use std::path::Path;

    use cargo_metadata::MetadataCommand;

    /// Crate names that run or load models: exact names and name prefixes.
    const EXACT: [&str; 6] = [
        "ort",
        "tch",
        "tokenizers",
        "rust-bert",
        "fastembed",
        "smartcore",
    ];
    const PREFIXES: [&str; 8] = [
        "candle-",
        "tract-",
        "burn",
        "llama",
        "onnx",
        "linfa",
        "whisper",
        "rust_tokenizers",
    ];

    fn is_inference(name: &str) -> bool {
        let n = name.to_lowercase();
        EXACT.contains(&n.as_str()) || PREFIXES.iter().any(|p| n.starts_with(p))
    }

    #[test]
    fn no_inference_dependencies() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let md = MetadataCommand::new()
            .current_dir(&root)
            .exec()
            .expect("cargo metadata");
        let resolve = md.resolve.as_ref().expect("resolve");
        let resolved: BTreeSet<&cargo_metadata::PackageId> =
            resolve.nodes.iter().map(|n| &n.id).collect();
        let found: Vec<String> = md
            .packages
            .iter()
            .filter(|p| resolved.contains(&p.id))
            .filter(|p| is_inference(p.name.as_str()))
            .map(|p| format!("{} {}", p.name, p.version))
            .collect();
        assert!(
            found.is_empty(),
            "inference crates in the dependency graph:\n{}",
            found.join("\n")
        );
        assert!(
            resolved.len() > md.workspace_members.len(),
            "the resolved graph includes external crates"
        );
    }

    #[test]
    fn inference_names_are_recognised() {
        for bad in [
            "candle-core",
            "ort",
            "tch",
            "tract-onnx",
            "burn",
            "burn-ndarray",
            "llama-cpp-2",
            "onnxruntime",
            "tokenizers",
            "rust-bert",
            "fastembed",
            "linfa-clustering",
            "smartcore",
            "rust_tokenizers",
            "whisper-rs",
        ] {
            assert!(is_inference(bad), "{bad}");
        }
        for fine in [
            "serde",
            "redb",
            "ra_ap_syntax",
            "sort",
            "rustc-hash",
            "tracing",
        ] {
            assert!(!is_inference(fine), "{fine}");
        }
    }
}
