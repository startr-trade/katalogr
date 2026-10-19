//! The profile file format (ADR-0017, ADR-0020): YAML with `profile`, `version`, `select`
//! (version 2) and `rules`. Parsing
//! validates everything it can, so a refused file names a line and a reason. Errors raised
//! inside `Deserialize` impls carry the position of the node being read (serde_yaml_ng adds it).

use globset::{Glob, GlobMatcher};
use katalogr_model::{ContractName, ContractRole};
use regex::Regex;
use serde::Deserialize;
use serde::de::{self, Deserializer};
use std::collections::BTreeMap;

/// The latest `version`, the one new files are written in.
pub const FORMAT_VERSION: u32 = 2;
/// The versions this build reads.
pub const FORMAT_VERSIONS: &[u32] = &[1, 2];

/// A parsed, validated profile file.
#[derive(Debug, Clone)]
pub struct ProfileDoc {
    pub profile: String,
    pub version: u32,
    pub select: Vec<Select>,
    pub rules: Vec<Rule>,
}

/// `select` (version 2): route the files a glob matches to a registered extension.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Select {
    /// An extractor id.
    pub extension: String,
    pub path: PathGlob,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDoc {
    profile: String,
    version: u32,
    #[serde(default)]
    select: Vec<Select>,
    #[serde(default)]
    rules: Vec<Rule>,
}

impl<'de> Deserialize<'de> for ProfileDoc {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<ProfileDoc, D::Error> {
        let raw = RawDoc::deserialize(d)?;
        if !is_word(&raw.profile) {
            return Err(de::Error::custom(format!(
                "profile name {:?} must be letters, digits, '_', '-' or '.'",
                raw.profile
            )));
        }
        if !FORMAT_VERSIONS.contains(&raw.version) {
            return Err(de::Error::custom(format!(
                "unsupported version {} (this build reads versions {})",
                raw.version,
                FORMAT_VERSIONS
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" and ")
            )));
        }
        for s in &raw.select {
            if !is_word(&s.extension) {
                return Err(de::Error::custom(format!(
                    "select extension {:?} must be letters, digits, '_', '-' or '.'",
                    s.extension
                )));
            }
        }
        if raw.version < 2 {
            if !raw.select.is_empty() {
                return Err(de::Error::custom(NEEDS_SELECT));
            }
            if raw.rules.iter().any(|r| r.uses_fact()) {
                return Err(de::Error::custom(NEEDS_FACT));
            }
            if raw.rules.iter().any(|r| r.uses_scope()) {
                return Err(de::Error::custom(NEEDS_SCOPE));
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        for r in &raw.rules {
            if !seen.insert(r.id.as_str()) {
                return Err(de::Error::custom(format!("duplicate rule id {:?}", r.id)));
            }
        }
        Ok(ProfileDoc {
            profile: raw.profile,
            version: raw.version,
            select: raw.select,
            rules: raw.rules,
        })
    }
}

/// Messages of the refusal of a version 2 feature in a version 1 file; `document_line` finds
/// their line by these texts.
pub(crate) const NEEDS_SELECT: &str = "`select` needs version: 2";
pub(crate) const NEEDS_FACT: &str = "`${fact:}` needs version: 2";
pub(crate) const NEEDS_SCOPE: &str = "`${scope:}` needs version: 2";

fn is_word(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
}

/// A regular expression with its source text.
#[derive(Debug, Clone)]
pub struct Pattern {
    pub source: String,
    pub re: Regex,
}

impl Pattern {
    /// Groups the pattern captures, not counting the whole match.
    pub fn groups(&self) -> usize {
        self.re.captures_len() - 1
    }
}

impl<'de> Deserialize<'de> for Pattern {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Pattern, D::Error> {
        let source = String::deserialize(d)?;
        let re = Regex::new(&source).map_err(|e| {
            let text = e.to_string();
            let reason = text
                .lines()
                .find_map(|l| l.strip_prefix("error: "))
                .unwrap_or_else(|| text.lines().next().unwrap_or(""));
            de::Error::custom(format!("regex {source:?} does not compile: {reason}"))
        })?;
        Ok(Pattern { source, re })
    }
}

/// A path glob with its source text.
#[derive(Debug, Clone)]
pub struct PathGlob {
    pub source: String,
    pub matcher: GlobMatcher,
}

impl<'de> Deserialize<'de> for PathGlob {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<PathGlob, D::Error> {
        let source = String::deserialize(d)?;
        let glob = Glob::new(&source)
            .map_err(|e| de::Error::custom(format!("glob {source:?} is invalid: {e}")))?;
        Ok(PathGlob {
            source,
            matcher: glob.compile_matcher(),
        })
    }
}

/// A variable a template can name besides the captures.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Var {
    Name,
    Repo,
    Path,
    Dir,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    Lit(String),
    /// `$1` to `$9`.
    Capture(usize),
    Var(Var),
    /// `${fact:<name>}` (version 2): the matched symbol's fact, when it is a string.
    Fact(String),
    /// `${scope:<a>/<b>}` (version 2): the `value` fact of the nearest enclosing symbol path.
    Scope(Vec<String>),
}

/// A string with `$1`..`$9`, `$name`, `$repo`, `$path`, `$dir`, and (version 2) `${fact:name}`
/// and `${scope:a/b}`; `$$` is a literal dollar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template(pub Vec<Piece>);

impl Template {
    pub fn parse(s: &str) -> Result<Template, String> {
        let mut pieces = Vec::new();
        let mut lit = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '$' {
                lit.push(c);
                continue;
            }
            let flush = |lit: &mut String, p: Piece, pieces: &mut Vec<Piece>| {
                if !lit.is_empty() {
                    pieces.push(Piece::Lit(std::mem::take(lit)));
                }
                pieces.push(p);
            };
            match chars.peek().copied() {
                Some('$') => {
                    chars.next();
                    lit.push('$');
                }
                Some('{') => {
                    chars.next();
                    let mut body = String::new();
                    loop {
                        match chars.next() {
                            Some('}') => break,
                            Some(c) => body.push(c),
                            None => return Err(format!("{s:?}: a '${{' is not closed by '}}'")),
                        }
                    }
                    let (kind, name) = body.split_once(':').unwrap_or((body.as_str(), ""));
                    let name_ok = |n: &str| {
                        !n.is_empty()
                            && n.chars().all(|c| {
                                c.is_ascii_alphanumeric()
                                    || matches!(c, '_' | '@' | '#' | '.' | '-')
                            })
                    };
                    let piece = match kind {
                        "fact" if name_ok(name) => Piece::Fact(name.to_string()),
                        "scope" if !name.is_empty() && name.split('/').all(name_ok) => {
                            Piece::Scope(name.split('/').map(str::to_string).collect())
                        }
                        "fact" | "scope" => {
                            return Err(format!(
                                "{s:?}: ${{{kind}:..}} needs a name of letters, digits, '_', '@', '#', '.' or '-'{}",
                                if kind == "scope" {
                                    " (parts joined by '/')"
                                } else {
                                    ""
                                }
                            ));
                        }
                        other => {
                            return Err(format!(
                                "{s:?}: unknown template variable ${{{other}:..}} (use ${{fact:..}} or ${{scope:..}})"
                            ));
                        }
                    };
                    flush(&mut lit, piece, &mut pieces);
                }
                Some(d) if d.is_ascii_digit() => {
                    chars.next();
                    let n = d as usize - '0' as usize;
                    if n == 0 {
                        return Err(format!("{s:?}: $0 is not a capture; use $1 to $9"));
                    }
                    flush(&mut lit, Piece::Capture(n), &mut pieces);
                }
                Some(a) if a.is_ascii_alphabetic() || a == '_' => {
                    let mut word = String::new();
                    while let Some(&w) = chars.peek() {
                        if w.is_ascii_alphanumeric() || w == '_' {
                            word.push(w);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    let var = match word.as_str() {
                        "name" => Var::Name,
                        "repo" => Var::Repo,
                        "path" => Var::Path,
                        "dir" => Var::Dir,
                        other => {
                            return Err(format!(
                                "{s:?}: unknown template variable ${other} \
                                 (use $1..$9, $name, $repo, $path, $dir)"
                            ));
                        }
                    };
                    flush(&mut lit, Piece::Var(var), &mut pieces);
                }
                _ => {
                    return Err(format!(
                        "{s:?}: a '$' must start $1..$9, $name, $repo, $path, $dir, or be written $$"
                    ));
                }
            }
        }
        if !lit.is_empty() {
            pieces.push(Piece::Lit(lit));
        }
        Ok(Template(pieces))
    }

    /// The highest capture number used (0 when none).
    pub fn max_capture(&self) -> usize {
        self.0
            .iter()
            .filter_map(|p| match p {
                Piece::Capture(n) => Some(*n),
                _ => None,
            })
            .max()
            .unwrap_or(0)
    }

    /// True when a piece needs profile format version 2.
    pub fn uses_v2(&self) -> bool {
        self.uses_fact() || self.uses_scope()
    }

    fn uses_fact(&self) -> bool {
        self.0.iter().any(|p| matches!(p, Piece::Fact(_)))
    }

    fn uses_scope(&self) -> bool {
        self.0.iter().any(|p| matches!(p, Piece::Scope(_)))
    }

    /// The text when the template has no variables or captures.
    pub fn literal(&self) -> Option<String> {
        let mut out = String::new();
        for p in &self.0 {
            match p {
                Piece::Lit(s) => out.push_str(s),
                _ => return None,
            }
        }
        Some(out)
    }
}

impl<'de> Deserialize<'de> for Template {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Template, D::Error> {
        let s = String::deserialize(d)?;
        Template::parse(&s).map_err(de::Error::custom)
    }
}

/// The matchers of a rule, combined with AND.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    /// Extractor id.
    pub extension: Option<String>,
    /// Symbol kind, exact.
    pub kind: Option<String>,
    /// Regex over the symbol's last name segment.
    pub name: Option<Pattern>,
    /// A fact that must exist.
    pub fact: Option<String>,
    /// Regex over the string value of `fact`; its captures feed `$1..$9`.
    pub matches: Option<Pattern>,
    /// Glob over the artifact's path.
    pub path: Option<PathGlob>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RoleName {
    Provides,
    Consumes,
    Declares,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContractSpec {
    kind: Template,
    key: Template,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBind {
    contract: ContractSpec,
    role: RoleName,
}

/// `bind`: attach the matched symbol to a contract.
#[derive(Debug, Clone)]
pub struct BindEmit {
    pub kind: Template,
    pub key: Template,
    pub role: ContractRole,
}

impl<'de> Deserialize<'de> for BindEmit {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<BindEmit, D::Error> {
        let raw = RawBind::deserialize(d)?;
        if let Some(kind) = raw.contract.kind.literal() {
            ContractName {
                kind: kind.clone(),
                key: String::new(),
            }
            .validate()
            .map_err(|_| {
                de::Error::custom(format!(
                    "contract kind {kind:?} must be lowercase letters, digits or '_'"
                ))
            })?;
        }
        Ok(BindEmit {
            kind: raw.contract.kind,
            key: raw.contract.key,
            role: match raw.role {
                RoleName::Provides => ContractRole::Provides,
                RoleName::Consumes => ContractRole::Consumes,
                RoleName::Declares => ContractRole::Declares,
            },
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateSpec {
    pub ns: Template,
    pub key: Template,
}

/// `ref`: a reference from the matched symbol, resolved by export key.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RefEmit {
    pub kind: String,
    /// Ordered: most specific first.
    pub candidates: Vec<CandidateSpec>,
}

/// A fact value: scalars with templated strings, lists and maps. No floats.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactSpec {
    Str(Template),
    Int(i64),
    Bool(bool),
    List(Vec<FactSpec>),
    Map(BTreeMap<String, FactSpec>),
}

impl FactSpec {
    fn from_value(v: serde_yaml_ng::Value) -> Result<FactSpec, String> {
        use serde_yaml_ng::Value;
        Ok(match v {
            Value::Bool(b) => FactSpec::Bool(b),
            Value::Number(n) => {
                FactSpec::Int(n.as_i64().ok_or("a fact number must be an integer")?)
            }
            Value::String(s) => FactSpec::Str(Template::parse(&s)?),
            Value::Sequence(items) => FactSpec::List(
                items
                    .into_iter()
                    .map(FactSpec::from_value)
                    .collect::<Result<_, _>>()?,
            ),
            Value::Mapping(m) => {
                let mut out = BTreeMap::new();
                for (k, v) in m {
                    let Value::String(k) = k else {
                        return Err("a fact map key must be a string".to_string());
                    };
                    out.insert(k, FactSpec::from_value(v)?);
                }
                FactSpec::Map(out)
            }
            Value::Null => return Err("a fact value must not be null".to_string()),
            Value::Tagged(_) => return Err("a fact value must not be tagged".to_string()),
        })
    }

    fn max_capture(&self) -> usize {
        match self {
            FactSpec::Str(t) => t.max_capture(),
            FactSpec::List(l) => l.iter().map(FactSpec::max_capture).max().unwrap_or(0),
            FactSpec::Map(m) => m.values().map(FactSpec::max_capture).max().unwrap_or(0),
            _ => 0,
        }
    }
}

impl<'de> Deserialize<'de> for FactSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<FactSpec, D::Error> {
        let v = serde_yaml_ng::Value::deserialize(d)?;
        FactSpec::from_value(v).map_err(de::Error::custom)
    }
}

/// What a matching rule adds. At least one action.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Emit {
    pub bind: Option<BindEmit>,
    #[serde(rename = "ref")]
    pub reference: Option<RefEmit>,
    pub fact: Option<BTreeMap<String, FactSpec>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRule {
    id: String,
    #[serde(default)]
    when: When,
    emit: Emit,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub when: When,
    pub emit: Emit,
}

impl Rule {
    /// Every template of the emit part.
    fn templates(&self) -> Vec<&Template> {
        let mut out = Vec::new();
        if let Some(b) = &self.emit.bind {
            out.push(&b.kind);
            out.push(&b.key);
        }
        if let Some(r) = &self.emit.reference {
            for c in &r.candidates {
                out.push(&c.ns);
                out.push(&c.key);
            }
        }
        if let Some(f) = &self.emit.fact {
            fn walk<'a>(spec: &'a FactSpec, out: &mut Vec<&'a Template>) {
                match spec {
                    FactSpec::Str(t) => out.push(t),
                    FactSpec::List(l) => l.iter().for_each(|s| walk(s, out)),
                    FactSpec::Map(m) => m.values().for_each(|s| walk(s, out)),
                    _ => {}
                }
            }
            f.values().for_each(|s| walk(s, &mut out));
        }
        out
    }

    /// True when a template reads a `${scope:..}`.
    pub fn uses_scope(&self) -> bool {
        self.templates().iter().any(|t| t.uses_scope())
    }

    fn uses_fact(&self) -> bool {
        self.templates().iter().any(|t| t.uses_fact())
    }

    /// True when the rule needs profile format version 2.
    pub fn uses_v2(&self) -> bool {
        self.templates().iter().any(|t| t.uses_v2())
    }

    /// How many capture groups `$1..$9` can name: those of `matches`, else those of `name`.
    pub fn groups(&self) -> usize {
        match (&self.when.matches, &self.when.name) {
            (Some(m), _) => m.groups(),
            (None, Some(n)) => n.groups(),
            (None, None) => 0,
        }
    }
}

impl<'de> Deserialize<'de> for Rule {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Rule, D::Error> {
        let raw = RawRule::deserialize(d)?;
        let err = |m: String| de::Error::custom(format!("rule {:?}: {m}", raw.id));
        if !is_word(&raw.id) {
            return Err(err(
                "the id must be letters, digits, '_', '-' or '.'".to_string()
            ));
        }
        let e = &raw.emit;
        if e.bind.is_none() && e.reference.is_none() && e.fact.as_ref().is_none_or(|f| f.is_empty())
        {
            return Err(err("emit is empty: give bind, ref or fact".to_string()));
        }
        if raw.when.matches.is_some() && raw.when.fact.is_none() {
            return Err(err("'matches' needs the 'fact' it matches".to_string()));
        }
        if let Some(r) = &e.reference
            && r.candidates.is_empty()
        {
            return Err(err("ref needs at least one candidate".to_string()));
        }
        let rule = Rule {
            id: raw.id.clone(),
            when: raw.when,
            emit: raw.emit,
        };
        let groups = rule.groups();
        let mut used = 0;
        if let Some(b) = &rule.emit.bind {
            used = used.max(b.kind.max_capture()).max(b.key.max_capture());
        }
        if let Some(r) = &rule.emit.reference {
            for c in &r.candidates {
                used = used.max(c.ns.max_capture()).max(c.key.max_capture());
            }
        }
        if let Some(f) = &rule.emit.fact {
            used = used.max(f.values().map(FactSpec::max_capture).max().unwrap_or(0));
        }
        if used > groups {
            return Err(de::Error::custom(format!(
                "rule {:?}: ${used} is out of range: the rule's regex captures {groups} group(s)",
                rule.id
            )));
        }
        Ok(rule)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_template_parses() {
        let t = Template::parse("a/${scope:schema/type}-${fact:value}-$1-$$").unwrap();
        assert_eq!(
            t.0,
            vec![
                Piece::Lit("a/".into()),
                Piece::Scope(vec!["schema".into(), "type".into()]),
                Piece::Lit("-".into()),
                Piece::Fact("value".into()),
                Piece::Lit("-".into()),
                Piece::Capture(1),
                Piece::Lit("-$".into()),
            ]
        );
        assert!(t.uses_v2());
        assert!(!Template::parse("$repo:$dir").unwrap().uses_v2());
        // Names may carry '@', '#' and '.' (XML attributes, text, dotted keys).
        assert!(Template::parse("${scope:@id/#text/a.b-c_d}").is_ok());
        for bad in [
            "${}",
            "${fact:}",
            "${fact:a b}",
            "${scope:a//b}",
            "${nope:x}",
            "${fact:x",
            "${x}",
        ] {
            assert!(Template::parse(bad).is_err(), "{bad}");
        }
    }
}
