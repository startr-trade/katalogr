//! Repo-relative paths.

use crate::name::NameError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Repo-relative path: UTF-8, '/'-separated, non-empty, no leading '/', no '.'/'..' segments,
/// no '\\', no drive prefix, no NUL, no empty segments. Construction is the only check.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RelPath(String);

impl RelPath {
    pub fn new(s: &str) -> Result<RelPath, NameError> {
        let bad = |why: &str| NameError::BadPath(format!("{why}: {s:?}"));
        if s.is_empty() {
            return Err(bad("empty"));
        }
        if s.starts_with('/') {
            return Err(bad("leading '/'"));
        }
        if s.contains('\\') {
            return Err(bad("backslash"));
        }
        if s.contains('\0') {
            return Err(bad("NUL"));
        }
        let b = s.as_bytes();
        if b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' {
            return Err(bad("drive prefix"));
        }
        for seg in s.split('/') {
            match seg {
                "" => return Err(bad("empty segment")),
                "." | ".." => return Err(bad("'.' or '..' segment")),
                _ => {}
            }
        }
        Ok(RelPath(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RelPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl Serialize for RelPath {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RelPath {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        RelPath::new(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_forms() {
        for bad in [
            "/a", "a/../b", "..", "a/./b", ".", "a\\b", "C:", "C:/a", "", "a//b", "a/", "a\0b",
        ] {
            assert!(
                matches!(RelPath::new(bad), Err(NameError::BadPath(_))),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn accepts_normal_paths() {
        for ok in [
            "a",
            "src/graph.rs",
            "a b/c#d",
            ".hidden/x",
            "a..b/c",
            "xy:z/w",
        ] {
            assert_eq!(RelPath::new(ok).unwrap().as_str(), ok);
        }
    }
}
