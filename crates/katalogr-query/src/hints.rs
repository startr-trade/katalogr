//! Next-query hints: fixed rules per command, so output stays deterministic (FEAT-025 R-9).

use crate::result::{
    CallerHit, DanglingRef, Explain, MapEntry, MirrorDrift, PathStep, RefHit, SearchHit,
    ShowResult, SymbolHit, WhyResult,
};

/// The most useful symbol of a result, if there is one.
pub trait HintSource {
    /// Canonical name of the leading symbol.
    fn lead(&self) -> Option<String>;
}

/// Unresolved references have no useful symbol to follow up on, so no hints.
impl HintSource for Vec<DanglingRef> {
    fn lead(&self) -> Option<String> {
        None
    }
}
/// A drift is followed up by looking at the contract, which `explain` can do from the output.
impl HintSource for Vec<MirrorDrift> {
    fn lead(&self) -> Option<String> {
        None
    }
}
impl HintSource for Vec<SymbolHit> {
    fn lead(&self) -> Option<String> {
        self.first().map(|h| h.name.clone())
    }
}
impl HintSource for Vec<SearchHit> {
    fn lead(&self) -> Option<String> {
        self.first().map(|h| h.symbol.name.clone())
    }
}
impl HintSource for Vec<RefHit> {
    fn lead(&self) -> Option<String> {
        self.first().map(|h| h.from.name.clone())
    }
}
impl HintSource for Vec<CallerHit> {
    fn lead(&self) -> Option<String> {
        self.first().map(|h| h.caller.name.clone())
    }
}
impl HintSource for Vec<MapEntry> {
    fn lead(&self) -> Option<String> {
        self.first().map(|h| h.symbol.name.clone())
    }
}
impl HintSource for Vec<PathStep> {
    fn lead(&self) -> Option<String> {
        self.first().map(|s| s.from.clone())
    }
}
impl HintSource for WhyResult {
    fn lead(&self) -> Option<String> {
        Some(self.symbol.name.clone())
    }
}
impl HintSource for Explain {
    fn lead(&self) -> Option<String> {
        Some(self.symbol.name.clone())
    }
}
impl HintSource for ShowResult {
    fn lead(&self) -> Option<String> {
        Some(self.symbol.name.clone())
    }
}

/// Quote a name for a shell when it holds anything but plain name characters.
fn quote(name: &str) -> String {
    let plain = |c: char| c.is_alphanumeric() || "_-.:/#+@".contains(c);
    if !name.is_empty() && name.chars().all(plain) {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "'\\''"))
    }
}

/// Up to three follow-up commands for the result of `command`; none when the result has no
/// leading symbol. The commands to suggest are fixed per command.
pub fn hints(command: &str, result: &impl HintSource) -> Vec<String> {
    let Some(lead) = result.lead() else {
        return Vec::new();
    };
    let next: &[&str] = match command {
        "find" | "search" | "map" | "why" | "hubs" | "communities" | "rationale" => {
            &["explain", "show", "refs"]
        }
        "refs" | "callers" => &["explain", "show"],
        "explain" => &["show", "refs", "callers"],
        "show" => &["explain", "refs", "callers"],
        "path" => &["explain", "show"],
        _ => &[],
    };
    next.iter()
        .take(3)
        .map(|c| format!("katalogr {c} {}", quote(&lead)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quote("app:a.rs#fn:x"), "app:a.rs#fn:x");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("it's"), "'it'\\''s'");
    }
}
