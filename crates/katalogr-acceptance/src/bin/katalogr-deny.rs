//! Adds a denied term to `fixtures/privacy.sha256`. The term is read from stdin and never echoed.
//! Usage: `katalogr-deny [path-to-privacy.sha256]`

use katalogr_acceptance::privacy::{hash, to_hex};
use std::io::Read;

fn main() {
    if let Err(m) = run() {
        eprintln!("katalogr-deny: {m}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "fixtures/privacy.sha256".to_string());
    let mut term = String::new();
    std::io::stdin()
        .read_to_string(&mut term)
        .map_err(|e| format!("reading stdin: {e}"))?;
    let term = term.trim().to_lowercase();
    if term.is_empty() {
        return Err("no term on stdin".into());
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lines: Vec<String> = existing
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    let digest = to_hex(&hash(&term));
    let added = !lines.contains(&digest);
    lines.push(digest);
    lines.sort();
    lines.dedup();
    let mut out = lines.join("\n");
    out.push('\n');
    std::fs::write(&path, out).map_err(|e| format!("{path}: {e}"))?;
    println!("{}", if added { "added" } else { "already present" });
    Ok(())
}
