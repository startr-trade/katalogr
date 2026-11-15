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

    /// The repo-relative form of a relative filesystem path: its components joined with '/',
    /// whatever the system's separator (on Windows `a\b` becomes `a/b`). A leading `.` is
    /// dropped. Fails on an absolute path, a drive prefix, a `..` segment or a non-UTF-8
    /// segment, and on anything [`RelPath::new`] rejects.
    pub fn from_path(p: &std::path::Path) -> Result<RelPath, NameError> {
        use std::path::Component;
        let bad = |why: &str| NameError::BadPath(format!("{why}: {p:?}"));
        let mut parts = Vec::new();
        for c in p.components() {
            match c {
                Component::Normal(s) => parts.push(s.to_str().ok_or_else(|| bad("not UTF-8"))?),
                Component::CurDir => {}
                Component::ParentDir => return Err(bad("'..' segment")),
                Component::RootDir | Component::Prefix(_) => return Err(bad("not relative")),
            }
        }
        RelPath::new(&parts.join("/"))
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

    /// FEAT-082: a path built with the system separator becomes a '/'-separated `RelPath`.
    #[test]
    fn from_path_joins_with_slash() {
        let p = std::path::Path::new("src").join("sub").join("a.rs");
        assert_eq!(RelPath::from_path(&p).unwrap().as_str(), "src/sub/a.rs");
        let p = std::path::Path::new(".").join("a").join("b");
        assert_eq!(RelPath::from_path(&p).unwrap().as_str(), "a/b");
        for bad in ["/a", "../a", "", "a/../b"] {
            assert!(
                RelPath::from_path(std::path::Path::new(bad)).is_err(),
                "{bad:?}"
            );
        }
    }

    /// FEAT-082: on Windows a backslash is a separator; elsewhere it is part of a name, which a
    /// `RelPath` cannot hold.
    #[test]
    fn from_path_backslash() {
        let r = RelPath::from_path(std::path::Path::new("g0\\t0.tx"));
        if cfg!(windows) {
            assert_eq!(r.unwrap().as_str(), "g0/t0.tx");
        } else {
            assert!(matches!(r, Err(NameError::BadPath(_))));
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
