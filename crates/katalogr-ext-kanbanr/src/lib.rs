//! kanbanr board extension (FEAT-019, ADR-0016): the board repository is indexed like any
//! repository of the project. Items, requirements, tasks, goals, ADRs and specs become symbols
//! that code, commits and documents reference through the `board-item`, `board-req` and
//! `board-goal` export namespaces.

mod adr;
mod item;
mod lines;
mod markdown;
mod spec;

use katalogr_model::{
    ArtifactName, ArtifactNode, Confidence, Export, ExportKey, ExtractError, ExtractInput,
    Extractor, Fact, Fragment, Id, Link, LinkKind, Name, OutRef, Provenance, RelPath, Segment,
    Span, SymbolName, SymbolNode,
};
use std::collections::BTreeMap;

pub use katalogr_model::annotate::{GOAL_NS as NS_GOAL, ITEM_NS as NS_ITEM, REQ_NS as NS_REQ};

/// Which kind of board file a path is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// `projects/<p>/charter.yaml`
    Charter,
    /// `projects/<p>/features/**/<CODE>.yaml`
    Item,
    /// `projects/<p>/features/**/features-spec/<CODE>.md`
    Spec,
    /// `projects/<p>/docs/decisions/*.md`
    Adr,
}

fn is_item_file(name: &str) -> bool {
    let Some(code) = name.strip_suffix(".yaml") else {
        return false;
    };
    let mut it = code.splitn(2, '-');
    match (it.next(), it.next()) {
        (Some(a), Some(b)) => {
            a.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && a.chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
                && !b.is_empty()
                && b.chars().all(|c| c.is_ascii_digit())
        }
        _ => false,
    }
}

/// Classify a repo-relative path; `None` when it is not a board file this extension reads.
pub fn classify(path: &str) -> Option<FileKind> {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() < 3 || parts[0] != "projects" {
        return None;
    }
    let last = *parts.last()?;
    match parts[2] {
        "charter.yaml" if parts.len() == 3 => Some(FileKind::Charter),
        "features" if parts.len() >= 4 => {
            if is_item_file(last) {
                Some(FileKind::Item)
            } else if last.ends_with(".md") && parts[parts.len() - 2] == "features-spec" {
                Some(FileKind::Spec)
            } else {
                None
            }
        }
        "docs" if parts.len() == 5 && parts[3] == "decisions" && last.ends_with(".md") => {
            Some(FileKind::Adr)
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BoardExtractor;

impl Extractor for BoardExtractor {
    fn id(&self) -> &'static str {
        "kanbanr"
    }

    fn handles(&self, p: &RelPath) -> bool {
        classify(p.as_str()).is_some()
    }

    fn patterns(&self) -> &'static [&'static str] {
        &["items", "requirements", "tasks", "goals", "specs", "adrs"]
    }

    fn extract(&self, input: &ExtractInput) -> Result<Fragment, ExtractError> {
        let text = std::str::from_utf8(input.content).map_err(|e| ExtractError::Parse {
            span: None,
            message: format!("not utf-8: {e}"),
        })?;
        let kind = classify(input.path.as_str())
            .ok_or_else(|| ExtractError::Unsupported(format!("board file {}", input.path)))?;
        let mut b = Build::new(input, text);
        match kind {
            FileKind::Item => item::extract(&mut b)?,
            FileKind::Charter => item::charter(&mut b)?,
            FileKind::Spec => spec::extract(&mut b),
            FileKind::Adr => adr::extract(&mut b)?,
        }
        Ok(b.finish())
    }
}

/// Accumulates one file's fragment.
pub(crate) struct Build<'a> {
    pub repo: &'a str,
    pub path: &'a RelPath,
    pub revision: &'a str,
    pub text: &'a str,
    pub artifact: ArtifactName,
    pub aid: Id,
    pub facts: BTreeMap<String, Fact>,
    pub symbols: Vec<SymbolNode>,
    pub links: Vec<Link>,
    pub refs: Vec<OutRef>,
    pub exports: Vec<Export>,
    /// Line count, for whole-file spans.
    pub line_count: u32,
}

impl<'a> Build<'a> {
    fn new(input: &'a ExtractInput<'a>, text: &'a str) -> Build<'a> {
        let artifact = ArtifactName {
            repo: input.repo.to_string(),
            path: input.path.clone(),
        };
        Build {
            repo: input.repo,
            path: input.path,
            revision: input.revision,
            text,
            aid: artifact.id(),
            artifact,
            facts: BTreeMap::new(),
            symbols: Vec::new(),
            links: Vec::new(),
            refs: Vec::new(),
            exports: Vec::new(),
            line_count: text.lines().count().max(1) as u32,
        }
    }

    /// The whole file.
    pub fn whole(&self) -> Span {
        let last = self
            .text
            .lines()
            .last()
            .map_or(1, |l| l.chars().count().max(1));
        Span {
            start_line: 1,
            start_col: 1,
            end_line: self.line_count,
            end_col: last as u32,
        }
    }

    /// A one-line span at 1-based `line`.
    pub fn line_span(&self, line: u32) -> Span {
        let text = self
            .text
            .lines()
            .nth(line.saturating_sub(1) as usize)
            .unwrap_or("");
        Span {
            start_line: line,
            start_col: 1,
            end_line: line,
            end_col: text.chars().count().max(1) as u32,
        }
    }

    pub fn provenance(&self) -> Provenance {
        Provenance {
            repo: self.repo.to_string(),
            path: self.path.clone(),
            revision: self.revision.to_string(),
        }
    }

    /// Add a symbol of one nesting level below `parent` (a symbol path), linked from it.
    pub fn symbol(
        &mut self,
        parent: &[Segment],
        kind: &str,
        name: &str,
        span: Span,
        facts: BTreeMap<String, Fact>,
    ) -> (Id, Vec<Segment>) {
        let mut segments = parent.to_vec();
        segments.push(Segment {
            kind: kind.to_string(),
            name: name.to_string(),
        });
        let sym = SymbolName {
            artifact: self.artifact.clone(),
            segments: segments.clone(),
            ordinal: 1,
        };
        let id = sym.id();
        if !parent.is_empty() {
            let from = SymbolName {
                artifact: self.artifact.clone(),
                segments: parent.to_vec(),
                ordinal: 1,
            }
            .id();
            self.links.push(Link {
                from,
                to: id,
                kind: LinkKind("contains".to_string()),
                confidence: Confidence::Extracted,
                span: Some(span),
                provenance: self.provenance(),
            });
        }
        // The board has one symbol per name; a repeated name keeps the first.
        if !self.symbols.iter().any(|s| s.name == sym) {
            self.symbols.push(SymbolNode {
                name: sym,
                kind: kind.to_string(),
                span,
                facts,
            });
        }
        (id, segments)
    }

    pub fn export(&mut self, namespace: &str, key: String, target: Id) {
        self.exports.push(Export {
            key: ExportKey {
                namespace: namespace.to_string(),
                key,
            },
            target,
        });
    }

    pub fn add_ref(&mut self, from: Id, kind: &str, span: Option<Span>, keys: Vec<ExportKey>) {
        self.refs.push(OutRef {
            from,
            kind: LinkKind(kind.to_string()),
            span,
            candidates: keys,
            confidence: Confidence::Extracted,
            origin: None,
            binds: None,
        });
    }

    /// Fold the Markdown text of the file into the fragment (sections, links, annotations).
    pub fn markdown(&mut self, text: &str) -> markdown::Parsed {
        let mut p = markdown::parse(self.repo, self.path, self.revision, text);
        self.symbols.append(&mut p.symbols);
        self.links.append(&mut p.links);
        self.refs.append(&mut p.refs);
        self.exports.append(&mut p.exports);
        if let Some(t) = &p.title {
            self.facts.insert("title".to_string(), Fact::Str(t.clone()));
        }
        if !p.external.is_empty() {
            self.facts.insert(
                "urls".to_string(),
                Fact::List(p.external.iter().cloned().map(Fact::Str).collect()),
            );
        }
        p
    }

    fn finish(mut self) -> Fragment {
        self.symbols.sort_by_key(|s| s.name.canonical());
        Fragment {
            provenance: self.provenance(),
            extension: "kanbanr".to_string(),
            artifact: ArtifactNode {
                name: self.artifact,
                facts: self.facts,
            },
            symbols: self.symbols,
            links: self.links,
            refs: self.refs,
            exports: self.exports,
            bindings: Vec::new(),
        }
    }
}

/// The 1-based line of the first line at or after `from_line` (1-based) whose trimmed text,
/// without a leading `- `, starts with `needle`.
pub(crate) fn find_line(text: &str, from_line: u32, needle: &str) -> Option<u32> {
    text.lines()
        .enumerate()
        .skip(from_line.saturating_sub(1) as usize)
        .find(|(_, l)| {
            let t = l.trim_start();
            let t = t.strip_prefix("- ").unwrap_or(t).trim_start();
            t.starts_with(needle)
        })
        .map(|(i, _)| i as u32 + 1)
}

pub(crate) fn str_fact(v: &serde_yaml_ng::Value, key: &str) -> Option<Fact> {
    match v.get(key)? {
        serde_yaml_ng::Value::String(s) => Some(Fact::Str(s.clone())),
        serde_yaml_ng::Value::Number(n) => Some(Fact::Str(n.to_string())),
        _ => None,
    }
}
