//! Writes `schema/v1/<command>.schema.json`. Usage: `cargo run -p katalogr-dist --bin gen-schema [DIR]`.

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema/v1"));
    std::fs::create_dir_all(&dir)?;
    for (name, schema) in katalogr_dist::schemas() {
        let mut text = serde_json::to_string_pretty(&schema).expect("serialise schema");
        text.push('\n');
        std::fs::write(dir.join(format!("{name}.schema.json")), text)?;
    }
    Ok(())
}
