//! Stable names and ids (ADR-0012).

use crate::relpath::RelPath;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NameError {
    #[error("bad kind (must match [a-z0-9_]+): {0:?}")]
    BadKind(String),
    #[error("bad repo name (must match [a-z0-9][a-z0-9_-]*, not 'contract'): {0:?}")]
    BadRepo(String),
    #[error("bad path: {0}")]
    BadPath(String),
    #[error("bad hex id: {0:?}")]
    BadHex(String),
    #[error("bad escape in {0:?}")]
    BadEscape(String),
    #[error("bad ordinal in {0:?}")]
    BadOrdinal(String),
    #[error("malformed name: {0:?}")]
    Malformed(String),
}

/// 128-bit id: the first 16 bytes of SHA-256(canonical name). Hex is the display form.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Id(pub [u8; 16]);

impl Id {
    pub fn of(canonical: &str) -> Id {
        let digest = Sha256::digest(canonical.as_bytes());
        let mut out = [0u8; 16];
        out.copy_from_slice(&digest[..16]);
        Id(out)
    }

    /// 32 lowercase hex chars.
    pub fn to_hex(&self) -> String {
        const D: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(32);
        for b in self.0 {
            s.push(D[(b >> 4) as usize] as char);
            s.push(D[(b & 15) as usize] as char);
        }
        s
    }

    pub fn from_hex(s: &str) -> Result<Id, NameError> {
        let bad = || NameError::BadHex(s.to_string());
        let b = s.as_bytes();
        if b.len() != 32 {
            return Err(bad());
        }
        let nib = |c: u8| match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            _ => Err(bad()),
        };
        let mut out = [0u8; 16];
        for (i, o) in out.iter_mut().enumerate() {
            *o = (nib(b[2 * i])? << 4) | nib(b[2 * i + 1])?;
        }
        Ok(Id(out))
    }
}

impl std::fmt::Debug for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Id({})", self.to_hex())
    }
}

impl std::fmt::Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Id {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Id {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Id::from_hex(&s).map_err(serde::de::Error::custom)
    }
}

// ---------- escaping ----------

/// Percent-encode `: / # ~ %` and whitespace (uppercase hex, per UTF-8 byte).
fn escape(s: &str, keep_slash: bool, keep_colon: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut buf = [0u8; 4];
    for c in s.chars() {
        let reserved = match c {
            '/' => !keep_slash,
            ':' => !keep_colon,
            '#' | '~' | '%' => true,
            c => c.is_whitespace(),
        };
        if reserved {
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn unescape(s: &str) -> Result<String, NameError> {
    let bad = || NameError::BadEscape(s.to_string());
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let hex = s.get(i + 1..i + 3).ok_or_else(bad)?;
            if !hex.bytes().all(|c| c.is_ascii_hexdigit()) {
                return Err(bad());
            }
            out.push(u8::from_str_radix(hex, 16).map_err(|_| bad())?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| bad())
}

fn check_kind(k: &str) -> Result<(), NameError> {
    if !k.is_empty()
        && k.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    {
        Ok(())
    } else {
        Err(NameError::BadKind(k.to_string()))
    }
}

fn check_repo(r: &str) -> Result<(), NameError> {
    let b = r.as_bytes();
    let ok = !b.is_empty()
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
        && r != "contract";
    if ok {
        Ok(())
    } else {
        Err(NameError::BadRepo(r.to_string()))
    }
}

// ---------- names ----------

pub trait Name {
    fn canonical(&self) -> String;
    fn id(&self) -> Id {
        Id::of(&self.canonical())
    }
}

/// `<repo>:<path>`
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ArtifactName {
    pub repo: String,
    pub path: RelPath,
}

/// One nesting step of a symbol. `kind` matches `[a-z0-9_]+`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Segment {
    pub kind: String,
    pub name: String,
}

/// `<artifact>#<kind>:<name>/…[~<ordinal> if ordinal >= 2]`; ordinal 1 = first.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct SymbolName {
    pub artifact: ArtifactName,
    pub segments: Vec<Segment>,
    pub ordinal: u32,
}

/// `contract:<kind>:<key>`
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContractName {
    pub kind: String,
    pub key: String,
}

impl ArtifactName {
    /// Validating constructor.
    pub fn new(repo: &str, path: &str) -> Result<ArtifactName, NameError> {
        check_repo(repo)?;
        Ok(ArtifactName {
            repo: repo.to_string(),
            path: RelPath::new(path)?,
        })
    }
}

impl Name for ArtifactName {
    fn canonical(&self) -> String {
        format!("{}:{}", self.repo, escape(self.path.as_str(), true, true))
    }
}

impl Name for SymbolName {
    fn canonical(&self) -> String {
        let mut s = self.artifact.canonical();
        s.push('#');
        for (i, seg) in self.segments.iter().enumerate() {
            if i > 0 {
                s.push('/');
            }
            s.push_str(&seg.kind);
            s.push(':');
            s.push_str(&escape(&seg.name, false, false));
        }
        if self.ordinal >= 2 {
            s.push_str(&format!("~{}", self.ordinal));
        }
        s
    }
}

impl Name for ContractName {
    fn canonical(&self) -> String {
        format!("contract:{}:{}", self.kind, escape(&self.key, false, false))
    }
}

impl SymbolName {
    pub fn validate(&self) -> Result<(), NameError> {
        check_repo(&self.artifact.repo)?;
        self.segments.iter().try_for_each(|s| check_kind(&s.kind))
    }
}

impl ContractName {
    pub fn validate(&self) -> Result<(), NameError> {
        check_kind(&self.kind)
    }
}

/// Any of the three name forms.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum AnyName {
    Artifact(ArtifactName),
    Symbol(SymbolName),
    Contract(ContractName),
}

impl Name for AnyName {
    fn canonical(&self) -> String {
        match self {
            AnyName::Artifact(n) => n.canonical(),
            AnyName::Symbol(n) => n.canonical(),
            AnyName::Contract(n) => n.canonical(),
        }
    }
}

fn parse_artifact(s: &str) -> Result<ArtifactName, NameError> {
    let (repo, path) = s
        .split_once(':')
        .ok_or_else(|| NameError::Malformed(s.to_string()))?;
    check_repo(repo)?;
    Ok(ArtifactName {
        repo: repo.to_string(),
        path: RelPath::new(&unescape(path)?)?,
    })
}

/// Inverse of `canonical()`.
pub fn parse_name(s: &str) -> Result<AnyName, NameError> {
    let malformed = || NameError::Malformed(s.to_string());
    if let Some(rest) = s.strip_prefix("contract:") {
        let (kind, key) = rest.split_once(':').ok_or_else(malformed)?;
        check_kind(kind)?;
        return Ok(AnyName::Contract(ContractName {
            kind: kind.to_string(),
            key: unescape(key)?,
        }));
    }
    let Some((art, rest)) = s.split_once('#') else {
        return Ok(AnyName::Artifact(parse_artifact(s)?));
    };
    let artifact = parse_artifact(art)?;
    let (body, ordinal) = match rest.rsplit_once('~') {
        Some((body, n)) => {
            let n: u32 = n
                .parse()
                .map_err(|_| NameError::BadOrdinal(s.to_string()))?;
            if n < 2 || n.to_string() != n_str(rest) {
                return Err(NameError::BadOrdinal(s.to_string()));
            }
            (body, n)
        }
        None => (rest, 1),
    };
    if body.is_empty() {
        return Err(malformed());
    }
    let mut segments = Vec::new();
    for part in body.split('/') {
        let (kind, name) = part.split_once(':').ok_or_else(malformed)?;
        check_kind(kind)?;
        segments.push(Segment {
            kind: kind.to_string(),
            name: unescape(name)?,
        });
    }
    Ok(AnyName::Symbol(SymbolName {
        artifact,
        segments,
        ordinal,
    }))
}

fn n_str(rest: &str) -> &str {
    rest.rsplit_once('~').map_or("", |(_, n)| n)
}

// ---------- serde via canonical string ----------

macro_rules! serde_via_name {
    ($t:ty, $variant:ident) => {
        impl Serialize for $t {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.canonical())
            }
        }
        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                match parse_name(&s).map_err(serde::de::Error::custom)? {
                    AnyName::$variant(n) => Ok(n),
                    _ => Err(serde::de::Error::custom(format!("wrong name form: {s:?}"))),
                }
            }
        }
    };
}
serde_via_name!(ArtifactName, Artifact);
serde_via_name!(SymbolName, Symbol);
serde_via_name!(ContractName, Contract);

impl Serialize for AnyName {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.canonical())
    }
}
impl<'de> Deserialize<'de> for AnyName {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        parse_name(&s).map_err(serde::de::Error::custom)
    }
}

// ---------- ordering by canonical string ----------

impl Ord for ArtifactName {
    fn cmp(&self, o: &Self) -> Ordering {
        self.canonical().cmp(&o.canonical())
    }
}
impl PartialOrd for ArtifactName {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for SymbolName {
    fn cmp(&self, o: &Self) -> Ordering {
        self.canonical()
            .cmp(&o.canonical())
            .then(self.ordinal.cmp(&o.ordinal))
    }
}
impl PartialOrd for SymbolName {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for AnyName {
    fn cmp(&self, o: &Self) -> Ordering {
        self.canonical()
            .cmp(&o.canonical())
            .then_with(|| match (self, o) {
                (AnyName::Symbol(a), AnyName::Symbol(b)) => a.ordinal.cmp(&b.ordinal),
                _ => Ordering::Equal,
            })
    }
}
impl PartialOrd for AnyName {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn art(repo: &str, path: &str) -> ArtifactName {
        ArtifactName::new(repo, path).unwrap()
    }

    fn sym(ordinal: u32) -> SymbolName {
        SymbolName {
            artifact: art("core", "src/graph.rs"),
            segments: vec![
                Segment {
                    kind: "impl".into(),
                    name: "Graph".into(),
                },
                Segment {
                    kind: "fn".into(),
                    name: "add_node".into(),
                },
            ],
            ordinal,
        }
    }

    fn arb_artifact() -> impl Strategy<Value = ArtifactName> {
        (
            "[a-z0-9][a-z0-9_-]{0,6}".prop_filter("reserved", |r| r != "contract"),
            "[a-zA-Z0-9_.:#~% \t\u{a0}é-]{1,6}(/[a-zA-Z0-9_.:#~% \t\u{a0}é-]{1,6}){0,3}",
        )
            .prop_filter_map("valid path", |(repo, path)| {
                RelPath::new(&path)
                    .ok()
                    .map(|path| ArtifactName { repo, path })
            })
    }

    fn arb_text() -> impl Strategy<Value = String> {
        "[a-z:/#~% \t\n\u{a0}é]{0,6}"
    }

    fn arb_symbol() -> impl Strategy<Value = SymbolName> {
        (
            arb_artifact(),
            prop::collection::vec(("[a-z0-9_]{1,5}", arb_text()), 1..4),
            1u32..50,
        )
            .prop_map(|(artifact, segs, ordinal)| SymbolName {
                artifact,
                segments: segs
                    .into_iter()
                    .map(|(kind, name)| Segment { kind, name })
                    .collect(),
                ordinal,
            })
    }

    fn arb_contract() -> impl Strategy<Value = ContractName> {
        ("[a-z0-9_]{1,6}", arb_text()).prop_map(|(kind, key)| ContractName { kind, key })
    }

    proptest! {
        #[test]
        fn canonical_round_trip(a in arb_artifact(), s in arb_symbol(), c in arb_contract()) {
            prop_assert_eq!(parse_name(&a.canonical()).unwrap(), AnyName::Artifact(a));
            prop_assert_eq!(parse_name(&s.canonical()).unwrap(), AnyName::Symbol(s));
            prop_assert_eq!(parse_name(&c.canonical()).unwrap(), AnyName::Contract(c));
        }
    }

    #[test]
    fn bad_kind_rejected() {
        for k in ["", "Fn", "a-b", "a b", "a:b", "é"] {
            let mut s = sym(1);
            s.segments[0].kind = k.to_string();
            assert!(matches!(s.validate(), Err(NameError::BadKind(_))), "{k:?}");
            let c = ContractName {
                kind: k.to_string(),
                key: "x".into(),
            };
            assert!(matches!(c.validate(), Err(NameError::BadKind(_))), "{k:?}");
        }
        assert!(matches!(
            parse_name("core:a.rs#Fn:x"),
            Err(NameError::BadKind(_))
        ));
        assert!(matches!(
            parse_name("contract:Bad:k"),
            Err(NameError::BadKind(_))
        ));
        assert!(matches!(
            ArtifactName::new("Core", "a"),
            Err(NameError::BadRepo(_))
        ));
    }

    #[test]
    fn ordinal_suffix_only_from_two() {
        assert_eq!(
            sym(1).canonical(),
            "core:src/graph.rs#impl:Graph/fn:add_node"
        );
        assert_eq!(
            sym(2).canonical(),
            "core:src/graph.rs#impl:Graph/fn:add_node~2"
        );
        assert_eq!(
            sym(3).canonical(),
            "core:src/graph.rs#impl:Graph/fn:add_node~3"
        );
        assert!(matches!(
            parse_name("core:a.rs#fn:x~1"),
            Err(NameError::BadOrdinal(_))
        ));
        assert!(matches!(
            parse_name("core:a.rs#fn:x~02"),
            Err(NameError::BadOrdinal(_))
        ));
    }

    #[test]
    fn id_is_stable_and_hex() {
        let n = sym(1);
        assert_eq!(n.id(), n.id());
        let hex = n.id().to_hex();
        assert_eq!(hex.len(), 32);
        assert!(
            hex.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        );
        assert_eq!(Id::from_hex(&hex).unwrap(), n.id());
        assert_ne!(n.id(), sym(2).id());
        // SHA-256("abc") starts ba7816bf8f01cfea414140de5dae2223
        assert_eq!(Id::of("abc").to_hex(), "ba7816bf8f01cfea414140de5dae2223");
        assert!(Id::from_hex("zz").is_err());
        assert!(Id::from_hex(&hex.to_uppercase()).is_err());
        let j = serde_json::to_string(&n.id()).unwrap();
        assert_eq!(j, format!("\"{hex}\""));
    }

    #[test]
    fn escapes_use_uppercase_hex() {
        let c = ContractName {
            kind: "chan".into(),
            key: "a:b c".into(),
        };
        assert_eq!(c.canonical(), "contract:chan:a%3Ab%20c");
    }
}
