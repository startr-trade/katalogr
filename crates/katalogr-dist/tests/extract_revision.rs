//! R-6: every extractor declares a revision salt, and a change to its golden output needs a new one.
//!
//! The table `fixtures/extract-revision.json` maps an extractor id to its salt and to the SHA-256
//! of its output over its own fixture folder. Regenerate it with `KATALOGR_UPDATE_REVISIONS=1`.

use camino::{Utf8Path, Utf8PathBuf};
use katalogr_model::{ExtractInput, Extractor, RelPath};
use katalogr_store::shard;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Every shipped extractor with the fixture folder that exercises it.
fn extractors() -> Vec<(Box<dyn Extractor>, &'static str)> {
    vec![
        (Box::new(katalogr_ext_rust::RustExtractor), "rust"),
        (Box::new(katalogr_ext_ts::TsExtractor), "ts"),
        (Box::new(katalogr_ext_deps::DepsExtractor), "deps"),
        (Box::new(katalogr_ext_sql::SqlExtractor), "sql"),
        (Box::new(katalogr_ext_openapi::OpenApiExtractor), "openapi"),
        (Box::new(katalogr_ext_schema::XsdExtractor), "xsd"),
        (
            Box::new(katalogr_ext_schema::JsonSchemaExtractor),
            "json-schema",
        ),
        (Box::new(katalogr_ext_data::DataExtractor), "data"),
        (Box::new(katalogr_ext_kanbanr::BoardExtractor), "kanbanr"),
        (Box::new(katalogr_ext_md::MarkdownExtractor), "md"),
        (Box::new(katalogr_ext_text::TextExtractor), "text"),
    ]
}

fn workspace() -> Utf8PathBuf {
    Utf8Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize_utf8()
        .unwrap()
}

fn table_path() -> Utf8PathBuf {
    workspace().join("fixtures/extract-revision.json")
}

/// Files under `dir`, sorted by relative path, skipping case expectations and profiles.
fn files(dir: &Utf8Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Utf8Path, dir: &Utf8Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = Utf8PathBuf::from_path_buf(e.path()).unwrap();
            // Repo-relative paths are '/'-separated whatever the system's separator: a
            // backslash here would make the file unroutable and change the digest on Windows.
            let rel = RelPath::from_path(p.strip_prefix(base).unwrap().as_std_path())
                .unwrap()
                .as_str()
                .to_string();
            if rel == "expected.yaml"
                || rel.starts_with(".katalogr-profiles")
                || rel.starts_with("_profiles")
            {
                continue;
            }
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// The SHA-256 (hex) of the extractor's text form over every case of its fixture folder.
fn digest(ext: &dyn Extractor, folder: &str) -> String {
    let root = workspace().join("fixtures").join(folder);
    let mut cases: Vec<Utf8PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .filter_map(|e| Utf8PathBuf::from_path_buf(e.path()).ok())
        .filter(|p| p.is_dir())
        .collect();
    cases.sort();
    let mut h = Sha256::new();
    for case in cases {
        let all = files(&case);
        let ctx_files: Vec<(RelPath, Vec<u8>)> = all
            .iter()
            .filter_map(|(p, c)| {
                let rp = RelPath::new(p).ok()?;
                ext.is_context_file(&rp).then(|| (rp, c.clone()))
            })
            .collect();
        let context = if ctx_files.is_empty() {
            Vec::new()
        } else {
            ext.build_context(&ctx_files).unwrap_or_default()
        };
        for (p, content) in &all {
            let Ok(rel) = RelPath::new(p) else { continue };
            // Extractors that claim nothing by name are routed by profile; offer them every file.
            let routed_by_profile = !ext.handles(&rel) && matches!(folder, "data" | "text");
            if !ext.handles(&rel) && !routed_by_profile {
                continue;
            }
            h.update(format!("{}/{p}\n", case.file_name().unwrap_or("")).as_bytes());
            let input = ExtractInput {
                repo: "fixture",
                path: &rel,
                revision: "fixture-revision",
                content,
                context: &context,
            };
            match ext.extract_with_errors(&input) {
                Ok((fragment, errors)) => {
                    match shard::encode(&fragment) {
                        Ok(bytes) => h.update(&bytes),
                        Err(e) => h.update(format!("encode error: {e}").as_bytes()),
                    }
                    h.update(format!("\nerrors: {errors:?}\n").as_bytes());
                }
                Err(e) => h.update(format!("error: {e:?}\n").as_bytes()),
            }
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn every_extractor_declares_one() {
    let mut missing = Vec::new();
    for (ext, _) in extractors() {
        if ext.revision_salt().trim().is_empty() {
            missing.push(ext.id());
        }
    }
    assert!(
        missing.is_empty(),
        "extractors with an empty revision salt: {missing:?}"
    );
}

#[test]
fn golden_output_needs_new_salt() {
    let mut now: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for (ext, folder) in extractors() {
        now.insert(
            ext.id().to_string(),
            serde_json::json!({"salt": ext.revision_salt(), "digest": digest(ext.as_ref(), folder)}),
        );
    }
    if std::env::var_os("KATALOGR_UPDATE_REVISIONS").is_some_and(|v| v == "1") {
        let mut text = serde_json::to_string_pretty(&now).unwrap();
        text.push('\n');
        std::fs::write(table_path(), text).unwrap();
        return;
    }
    let stored: BTreeMap<String, serde_json::Value> = serde_json::from_slice(
        &std::fs::read(table_path()).expect("fixtures/extract-revision.json"),
    )
    .expect("table parses");
    let mut problems = Vec::new();
    for (id, cur) in &now {
        match stored.get(id) {
            None => problems.push(format!(
                "{id}: not in the table; regenerate with KATALOGR_UPDATE_REVISIONS=1"
            )),
            Some(old) if old["salt"] != cur["salt"] => problems.push(format!(
                "{id}: the salt changed but the table is stale; regenerate with KATALOGR_UPDATE_REVISIONS=1"
            )),
            Some(old) if old["digest"] != cur["digest"] => problems.push(format!(
                "{id}: output changed but the revision salt did not; change revision_salt of extractor {id}"
            )),
            Some(_) => {}
        }
    }
    for id in stored.keys() {
        if !now.contains_key(id) {
            problems.push(format!("{id}: in the table but no longer an extractor"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
