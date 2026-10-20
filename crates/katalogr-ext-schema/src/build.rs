//! The fragment builder shared by the XSD and JSON Schema walkers, and public so that
//! API-document extensions can reuse it.

use katalogr_model::{
    ArtifactName, ArtifactNode, Binding, Confidence, ContractName, ContractRole, Export, ExportKey,
    ExtractInput, Fact, Fragment, Id, Link, LinkKind, Name, OutRef, Provenance, Segment, Span,
    SymbolName, SymbolNode,
};
use std::collections::{BTreeMap, HashMap};

pub struct Builder {
    artifact: ArtifactName,
    provenance: Provenance,
    extension: &'static str,
    facts: BTreeMap<String, Fact>,
    symbols: Vec<SymbolNode>,
    /// Id of each symbol, by index.
    ids: Vec<Id>,
    /// Segments of each symbol, by index.
    segments: Vec<Vec<Segment>>,
    links: Vec<Link>,
    refs: Vec<OutRef>,
    exports: Vec<Export>,
    bindings: Vec<Binding>,
    /// Occurrences of each canonical name (without ordinal) so far.
    ordinals: HashMap<String, u32>,
    /// Canonical name (without ordinal) of each symbol, by index.
    bases: Vec<String>,
    /// Canonical name of each symbol, by index.
    canon: Vec<String>,
    artifact_canon: String,
}

impl Builder {
    /// A builder for the file of `input`. `text` is the file's text, for extensions that need
    /// it; this builder keeps no reference to it.
    pub fn new(input: &ExtractInput, extension: &'static str, text: &str) -> Builder {
        let _ = text;
        let artifact = ArtifactName {
            repo: input.repo.to_string(),
            path: input.path.clone(),
        };
        Builder {
            artifact_canon: artifact.canonical(),
            artifact: ArtifactName {
                repo: input.repo.to_string(),
                path: input.path.clone(),
            },
            provenance: Provenance {
                repo: input.repo.to_string(),
                path: input.path.clone(),
                revision: input.revision.to_string(),
            },
            extension,
            facts: BTreeMap::new(),
            symbols: Vec::new(),
            ids: Vec::new(),
            segments: Vec::new(),
            links: Vec::new(),
            refs: Vec::new(),
            exports: Vec::new(),
            bindings: Vec::new(),
            ordinals: HashMap::new(),
            bases: Vec::new(),
            canon: Vec::new(),
        }
    }

    /// Add a symbol under `parent` (None = top level); assigns `~n` for a repeated (kind, name)
    /// in the same parent; adds a `contains` link (Extracted) from the parent symbol. Returns
    /// its index.
    pub fn symbol(
        &mut self,
        parent: Option<usize>,
        kind: &str,
        name: &str,
        span: Span,
        facts: BTreeMap<String, Fact>,
    ) -> usize {
        let mut segments = parent.map_or_else(Vec::new, |p| self.segments[p].clone());
        segments.push(Segment {
            kind: kind.to_string(),
            name: name.to_string(),
        });
        let mut base = match parent {
            Some(p) => format!("{}/", self.bases[p]),
            None => format!("{}#", self.artifact_canon),
        };
        base.push_str(kind);
        base.push(':');
        katalogr_ext_data::keytree::escape_segment(name, &mut base);
        let mut canon = base.clone();
        let n = match self.ordinals.get_mut(&canon) {
            Some(n) => {
                *n += 1;
                *n
            }
            None => {
                self.ordinals.insert(canon.clone(), 1);
                1
            }
        };
        let sym = SymbolName {
            artifact: self.artifact.clone(),
            segments: segments.clone(),
            ordinal: n,
        };
        if n >= 2 {
            canon.push_str(&format!("~{n}"));
        }
        let id = Id::of(&canon);
        if let Some(p) = parent {
            self.links.push(Link {
                from: self.ids[p],
                to: id,
                kind: LinkKind("contains".to_string()),
                confidence: Confidence::Extracted,
                span: Some(span),
                provenance: self.provenance.clone(),
            });
        }
        self.symbols.push(SymbolNode {
            name: sym,
            kind: kind.to_string(),
            span,
            facts,
        });
        self.ids.push(id);
        self.bases.push(base);
        self.canon.push(canon);
        self.segments.push(segments);
        self.symbols.len() - 1
    }

    /// Set a fact on a symbol (replacing one of the same name).
    pub fn fact(&mut self, sym: usize, key: &str, value: Fact) {
        self.symbols[sym].facts.insert(key.to_string(), value);
    }

    /// The fact `key` of a symbol.
    pub fn get_fact(&self, sym: usize, key: &str) -> Option<&Fact> {
        self.symbols[sym].facts.get(key)
    }

    /// Export a symbol under `namespace` and `key`.
    pub fn export(&mut self, sym: usize, namespace: &str, key: String) {
        self.exports.push(Export {
            key: ExportKey {
                namespace: namespace.to_string(),
                key,
            },
            target: self.ids[sym],
        });
    }

    /// A reference from a symbol, resolved by export key (most specific candidate first).
    pub fn reference(
        &mut self,
        sym: usize,
        kind: &str,
        candidates: Vec<ExportKey>,
        span: Span,
        confidence: Confidence,
    ) {
        self.refs.push(OutRef {
            from: self.ids[sym],
            kind: LinkKind(kind.to_string()),
            span: Some(span),
            candidates,
            confidence,
            origin: None,
            binds: None,
        });
    }

    /// Bind a symbol to a contract. Precondition: `kind` matches `[a-z0-9_]+` (callers pass
    /// constants).
    pub fn bind(
        &mut self,
        sym: usize,
        kind: &str,
        key: String,
        role: ContractRole,
        span: Span,
        confidence: Confidence,
    ) {
        self.bindings.push(Binding {
            symbol: self.ids[sym],
            contract: ContractName {
                kind: kind.to_string(),
                key,
            },
            role,
            span: Some(span),
            confidence,
            origin: None,
        });
    }

    /// A fact of the file itself.
    pub fn artifact_fact(&mut self, key: &str, value: Fact) {
        self.facts.insert(key.to_string(), value);
    }

    /// The fragment, with symbols, links, refs, exports and bindings sorted as the model
    /// requires.
    pub fn finish(mut self) -> Fragment {
        let mut pairs: Vec<(String, SymbolNode)> = std::mem::take(&mut self.canon)
            .into_iter()
            .zip(std::mem::take(&mut self.symbols))
            .collect();
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        self.symbols = pairs.into_iter().map(|(_, s)| s).collect();
        self.links
            .sort_by(|a, b| (a.from, &a.kind, a.to).cmp(&(b.from, &b.kind, b.to)));
        self.links
            .dedup_by(|a, b| (a.from, &a.kind, a.to) == (b.from, &b.kind, b.to));
        self.refs.sort_by(|a, b| {
            (a.from, &a.kind, &a.candidates).cmp(&(b.from, &b.kind, &b.candidates))
        });
        self.refs
            .dedup_by(|a, b| (a.from, &a.kind, &a.candidates) == (b.from, &b.kind, &b.candidates));
        self.exports
            .sort_by(|a, b| (&a.key, a.target).cmp(&(&b.key, b.target)));
        self.exports.dedup();
        self.bindings
            .sort_by_cached_key(|b| (b.contract.canonical(), b.role, b.symbol));
        self.bindings
            .dedup_by(|a, b| (&a.contract, a.role, a.symbol) == (&b.contract, b.role, b.symbol));
        Fragment {
            provenance: self.provenance,
            extension: self.extension.to_string(),
            artifact: ArtifactNode {
                name: self.artifact,
                facts: self.facts,
            },
            symbols: self.symbols,
            links: self.links,
            refs: self.refs,
            exports: self.exports,
            bindings: self.bindings,
        }
    }
}
