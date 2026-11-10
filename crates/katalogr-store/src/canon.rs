//! Canonical paths that every Windows file API accepts with `/`-joined repo-relative paths.

use std::path::{Path, PathBuf};

/// `std::fs::canonicalize` without Windows' verbatim prefix. On Windows it answers
/// `\\?\C:\repo`, and a verbatim path takes every character literally: a repo-relative path
/// joined with `/` (as shards and markers store them) would name no file. The plain form
/// `C:\repo` is returned instead whenever it means the same thing (a drive or UNC path with no
/// component Win32 would rewrite); otherwise the verbatim path is kept.
pub fn canonicalize(p: &Path) -> std::io::Result<PathBuf> {
    let c = std::fs::canonicalize(p)?;
    #[cfg(windows)]
    {
        if let Some(plain) = plain_windows(&c) {
            return Ok(plain);
        }
    }
    Ok(c)
}

/// The plain form of a verbatim Windows path, if it has one: `\\?\C:\a` → `C:\a` and
/// `\\?\UNC\srv\share\a` → `\\srv\share\a`. `None` when a component ends in a dot or a
/// space, or is a reserved device name, which Win32 parsing would change.
#[cfg_attr(not(windows), allow(dead_code))]
fn plain_windows(p: &Path) -> Option<PathBuf> {
    let s = p.to_str()?;
    let plain = if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        let rest = s.strip_prefix(r"\\?\")?;
        let b = rest.as_bytes();
        if b.len() < 2 || !b[0].is_ascii_alphabetic() || b[1] != b':' {
            return None;
        }
        rest.to_string()
    };
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    for part in plain.split('\\').skip(1) {
        if part.is_empty() {
            continue;
        }
        let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
        if part.ends_with('.') || part.ends_with(' ') || RESERVED.contains(&stem.as_str()) {
            return None;
        }
    }
    Some(PathBuf::from(plain))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verbatim_paths_made_plain() {
        let p = |s: &str| plain_windows(Path::new(s)).map(|p| p.to_str().unwrap().to_string());
        assert_eq!(
            p(r"\\?\C:\Users\a\repo").as_deref(),
            Some(r"C:\Users\a\repo")
        );
        assert_eq!(
            p(r"\\?\UNC\srv\share\repo").as_deref(),
            Some(r"\\srv\share\repo")
        );
        assert_eq!(p(r"C:\already\plain"), None);
        assert_eq!(p(r"\\?\C:\a\trailing."), None);
        assert_eq!(p(r"\\?\C:\a\con.txt"), None);
        assert_eq!(p(r"\\?\Volume{x}\a"), None);
    }
}
