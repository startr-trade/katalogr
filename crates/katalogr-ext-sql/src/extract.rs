//! The per-file builder (§4, §6.2): statements to symbol drafts, bindings and facts, with the
//! lexical fallback and error reporting, assembled into a `Fragment`.

use crate::context::SqlContext;
use crate::dialect::{Dialect, DialectSource, resolve};
use crate::lines::Lines;
use crate::migration::parse_migration_name;
use crate::names::{normalise_ident, table_key};
use crate::refs::{as_strs, name_parts, parse_chain};
use crate::scan::{Ddl, ObjectKind, Refs, recognise_header, scan_body, tokens};
use crate::split::split_statements_in;
use crate::{CONTRACT_ROUTINE, CONTRACT_TABLE};
use katalogr_model::{
    ArtifactName, ArtifactNode, Binding, Confidence, ContractName, ContractRole, ExtractError,
    ExtractInput, Fact, Fragment, Id, Link, LinkKind, Name, Provenance, Segment, Span, SymbolName,
    SymbolNode,
};
use std::collections::BTreeMap;

/// Largest statement handed to the parser; larger ones are scanned only.
const MAX_PARSE_BYTES: usize = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Owner {
    Artifact,
    Sym(usize),
    Col(usize, usize),
}

pub(crate) struct ColDraft {
    pub name: String,
    pub span: Span,
    pub facts: BTreeMap<String, Fact>,
}

pub(crate) struct Draft {
    pub kind: String,
    pub key: String,
    pub span: Span,
    pub facts: BTreeMap<String, Fact>,
    pub ddl: Vec<String>,
    pub parsed: bool,
    pub dialect: Option<Dialect>,
    pub columns: Vec<ColDraft>,
}

struct BindDraft {
    contract: ContractName,
    role: ContractRole,
    owner: Owner,
    span: Span,
    confidence: Confidence,
}

pub(crate) struct Builder<'a> {
    pub text: &'a str,
    pub lines: Lines<'a>,
    pub schema: String,
    pub drafts: Vec<Draft>,
    index: BTreeMap<(String, String, String), usize>,
    bindings: BTreeMap<(String, ContractRole, Owner), BindDraft>,
    pub errors: Vec<ExtractError>,
    pub statements: i64,
    pub unparsed: i64,
    pub dml: i64,
    /// Start (line, column) of the statement being mapped, for column spans.
    pub origin: (u32, u32),
}

pub(crate) fn s(v: &str) -> Fact {
    Fact::Str(v.to_string())
}

impl<'a> Builder<'a> {
    pub(crate) fn new(text: &'a str, schema: &str) -> Builder<'a> {
        Builder {
            text,
            lines: Lines::new(text),
            schema: schema.to_string(),
            drafts: Vec::new(),
            index: BTreeMap::new(),
            bindings: BTreeMap::new(),
            errors: Vec::new(),
            statements: 0,
            unparsed: 0,
            dml: 0,
            origin: (1, 1),
        }
    }

    /// The draft of `(kind, key, sig)`, created at `span` when new.
    pub(crate) fn draft(&mut self, kind: &str, key: &str, sig: &str, span: Span) -> usize {
        let k = (kind.to_string(), key.to_string(), sig.to_string());
        if let Some(&i) = self.index.get(&k) {
            return i;
        }
        self.drafts.push(Draft {
            kind: kind.to_string(),
            key: key.to_string(),
            span,
            facts: BTreeMap::new(),
            ddl: Vec::new(),
            parsed: true,
            dialect: None,
            columns: Vec::new(),
        });
        let i = self.drafts.len() - 1;
        self.index.insert(k, i);
        i
    }

    /// Record one operation on a draft.
    pub(crate) fn touch(&mut self, i: usize, op: &str, parsed: bool, dialect: Option<Dialect>) {
        let d = &mut self.drafts[i];
        d.ddl.push(op.to_string());
        d.parsed &= parsed;
        if parsed && d.dialect.is_none() {
            d.dialect = dialect;
        }
    }

    pub(crate) fn fact(&mut self, i: usize, k: &str, v: Fact) {
        self.drafts[i].facts.insert(k.to_string(), v);
    }

    /// The column `name` (already normalised) of table draft `t`, created at `span`.
    pub(crate) fn column(&mut self, t: usize, name: &str, span: Span) -> usize {
        let d = &mut self.drafts[t];
        if let Some(p) = d.columns.iter().position(|c| c.name == name) {
            return p;
        }
        d.columns.push(ColDraft {
            name: name.to_string(),
            span,
            facts: BTreeMap::new(),
        });
        d.columns.len() - 1
    }

    pub(crate) fn col_fact(&mut self, t: usize, c: usize, k: &str, v: Fact) {
        self.drafts[t].columns[c].facts.insert(k.to_string(), v);
    }

    /// Add a binding; the first one for (contract, role, owner) wins.
    pub(crate) fn bind(
        &mut self,
        owner: Owner,
        kind: &str,
        key: &str,
        role: ContractRole,
        span: Span,
        confidence: Confidence,
    ) {
        let contract = ContractName {
            kind: kind.to_string(),
            key: key.to_string(),
        };
        self.bindings
            .entry((contract.canonical(), role, owner))
            .or_insert(BindDraft {
                contract,
                role,
                owner,
                span,
                confidence,
            });
    }

    pub(crate) fn consume_refs(&mut self, owner: Owner, refs: &Refs, span: Span, conf: Confidence) {
        for t in &refs.tables {
            self.bind(owner, CONTRACT_TABLE, t, ContractRole::Consumes, span, conf);
        }
        for r in &refs.routines {
            self.bind(
                owner,
                CONTRACT_ROUTINE,
                r,
                ContractRole::Consumes,
                span,
                conf,
            );
        }
    }

    pub(crate) fn tkey(&self, name: &sqlparser::ast::ObjectName) -> String {
        let parts = name_parts(name);
        table_key(&as_strs(&parts), &self.schema)
    }

    /// Map a column span relative to the statement being mapped.
    pub(crate) fn rel_span(&self, sp: sqlparser::tokenizer::Span, fallback: Span) -> Span {
        if sp.start.line == 0 || sp.end.line == 0 {
            return fallback;
        }
        let map = |l: u64, c: u64| -> (u32, u32) {
            let line = self.origin.0 + l as u32 - 1;
            let col = if l == 1 {
                self.origin.1 + c as u32 - 1
            } else {
                c as u32
            };
            (line, col)
        };
        let (sl, sc) = map(sp.start.line, sp.start.column);
        let (el, ec) = map(sp.end.line, sp.end.column);
        Span {
            start_line: sl,
            start_col: sc,
            end_line: el,
            end_col: ec.max(1),
        }
    }

    /// Process every statement of the file.
    pub(crate) fn run(&mut self, file_dialect: Option<Dialect>) {
        let raws = split_statements_in(self.text, file_dialect);
        for raw in &raws {
            self.statements += 1;
            let span = self.lines.span(raw.start as u32, raw.end as u32);
            let (l, c) = self.lines.position(raw.start);
            self.origin = (l, c);
            let text = &self.text[raw.start..raw.end];
            let parsed = if text.len() > MAX_PARSE_BYTES {
                Err("statement too large to parse".to_string())
            } else {
                parse_chain(text, file_dialect)
            };
            match parsed {
                Ok((stmts, d)) => {
                    for st in &stmts {
                        self.map_statement(st, span, d);
                    }
                }
                Err(e) => self.fallback(text, span, e),
            }
        }
    }

    /// Steps 4 of §6.2: the header recogniser, then a DML-shaped scan, else a parse error.
    fn fallback(&mut self, text: &str, span: Span, first_error: String) {
        if let Some(h) = recognise_header(text) {
            self.unparsed += 1;
            let refs = scan_body(&text[h.body_start.min(text.len())..], &self.schema);
            let owner;
            let op = match h.ddl {
                Ddl::Create => "create",
                Ddl::Alter => "alter",
                Ddl::Drop => "drop",
            };
            let parts: Vec<&str> = h.name.iter().map(String::as_str).collect();
            let inferred = Confidence::Inferred;
            match h.kind {
                ObjectKind::Anonymous => {
                    owner = Owner::Artifact;
                }
                ObjectKind::Table | ObjectKind::View | ObjectKind::MaterializedView => {
                    let kind = if h.kind == ObjectKind::Table {
                        "table"
                    } else {
                        "view"
                    };
                    let key = table_key(&parts, &self.schema);
                    let i = self.draft(kind, &key, "", span);
                    self.touch(i, op, false, None);
                    if h.kind == ObjectKind::MaterializedView {
                        self.fact(i, "materialized", Fact::Bool(true));
                    }
                    owner = Owner::Sym(i);
                    let role = if h.ddl == Ddl::Drop {
                        ContractRole::Declares
                    } else {
                        ContractRole::Provides
                    };
                    self.bind(owner, CONTRACT_TABLE, &key, role, span, inferred);
                }
                ObjectKind::Function | ObjectKind::Procedure => {
                    let kind = if h.kind == ObjectKind::Function {
                        "function"
                    } else {
                        "procedure"
                    };
                    let key = table_key(&parts, &self.schema);
                    let i = self.draft(kind, &key, "", span);
                    self.touch(i, op, false, None);
                    owner = Owner::Sym(i);
                    let role = if h.ddl == Ddl::Drop {
                        ContractRole::Declares
                    } else {
                        ContractRole::Provides
                    };
                    self.bind(owner, CONTRACT_ROUTINE, &key, role, span, inferred);
                }
                ObjectKind::Trigger | ObjectKind::Policy => {
                    let on = h.on_table.as_ref().map(|p| {
                        let q: Vec<&str> = p.iter().map(String::as_str).collect();
                        table_key(&q, &self.schema)
                    });
                    let name = normalise_ident(parts.last().copied().unwrap_or(""));
                    let (kind, key) = if h.kind == ObjectKind::Trigger {
                        let schema = on
                            .as_deref()
                            .and_then(|k| k.split_once('.').map(|(a, _)| a.to_string()))
                            .unwrap_or_else(|| self.schema.clone());
                        ("trigger", format!("{schema}.{name}"))
                    } else {
                        let base = on.clone().unwrap_or_else(|| format!("{}.", self.schema));
                        ("policy", format!("{base}.{name}"))
                    };
                    let i = self.draft(kind, &key, "", span);
                    self.touch(i, op, false, None);
                    if let Some(t) = &on {
                        self.fact(i, "table", s(t));
                        if h.ddl != Ddl::Drop {
                            self.bind(
                                Owner::Sym(i),
                                CONTRACT_TABLE,
                                t,
                                ContractRole::Consumes,
                                span,
                                inferred,
                            );
                        }
                    }
                    owner = Owner::Sym(i);
                }
            }
            if h.ddl != Ddl::Drop {
                self.consume_refs(owner, &refs, span, inferred);
            }
            return;
        }
        let refs = scan_body(text, &self.schema);
        let first = tokens(text)
            .first()
            .map(|t| t.text(text).to_ascii_lowercase())
            .unwrap_or_default();
        let dml_shape = matches!(
            first.as_str(),
            "insert" | "update" | "delete" | "select" | "replace" | "merge" | "with" | "call"
        );
        if dml_shape && (!refs.tables.is_empty() || !refs.routines.is_empty()) {
            self.unparsed += 1;
            self.consume_refs(Owner::Artifact, &refs, span, Confidence::Inferred);
        } else {
            self.errors.push(ExtractError::Parse {
                span: Some(span),
                message: first_error,
            });
        }
    }

    pub(crate) fn finish(mut self, input: &ExtractInput, ctx: &SqlContext) -> Fragment {
        let artifact = ArtifactName {
            repo: input.repo.to_string(),
            path: input.path.clone(),
        };
        let provenance = Provenance {
            repo: input.repo.to_string(),
            path: input.path.clone(),
            revision: input.revision.to_string(),
        };
        // Ordinals: overloads of one (kind, key) count in order of first appearance.
        let mut seen: BTreeMap<(String, String), u32> = BTreeMap::new();
        let mut ordinals = Vec::with_capacity(self.drafts.len());
        for d in &self.drafts {
            let n = seen.entry((d.kind.clone(), d.key.clone())).or_insert(0);
            *n += 1;
            ordinals.push(*n);
        }
        let sym_name = |d: &Draft, ord: u32| SymbolName {
            artifact: artifact.clone(),
            segments: vec![Segment {
                kind: d.kind.clone(),
                name: d.key.clone(),
            }],
            ordinal: ord,
        };
        let mut symbols = Vec::new();
        let mut links = Vec::new();
        let mut sym_ids: Vec<Id> = Vec::new();
        let mut col_ids: BTreeMap<(usize, usize), Id> = BTreeMap::new();
        for (i, d) in self.drafts.iter().enumerate() {
            let name = sym_name(d, ordinals[i]);
            let id = name.id();
            sym_ids.push(id);
            let mut facts = d.facts.clone();
            facts.insert(
                "ddl".into(),
                Fact::List(d.ddl.iter().map(|x| s(x)).collect()),
            );
            facts.insert("parsed".into(), Fact::Bool(d.parsed));
            if let (true, Some(dl)) = (d.parsed, d.dialect) {
                facts.insert("dialect".into(), s(dl.as_str()));
            }
            symbols.push(SymbolNode {
                name: name.clone(),
                kind: d.kind.clone(),
                span: d.span,
                facts,
            });
            for (ci, c) in d.columns.iter().enumerate() {
                let mut cname = name.clone();
                cname.segments.push(Segment {
                    kind: "column".into(),
                    name: c.name.clone(),
                });
                cname.ordinal = 1;
                let cid = cname.id();
                col_ids.insert((i, ci), cid);
                links.push(Link {
                    from: id,
                    to: cid,
                    kind: LinkKind("contains".into()),
                    confidence: Confidence::Extracted,
                    span: Some(c.span),
                    provenance: provenance.clone(),
                });
                symbols.push(SymbolNode {
                    name: cname,
                    kind: "column".into(),
                    span: c.span,
                    facts: c.facts.clone(),
                });
            }
        }
        let artifact_id = artifact.id();
        let resolve_owner = |o: Owner| match o {
            Owner::Artifact => artifact_id,
            Owner::Sym(i) => sym_ids[i],
            Owner::Col(i, c) => col_ids[&(i, c)],
        };
        // No self-bindings: a symbol never consumes a contract it provides.
        let provided: std::collections::BTreeSet<(String, Owner)> = self
            .bindings
            .iter()
            .filter(|((_, role, _), _)| *role == ContractRole::Provides)
            .map(|((c, _, o), _)| (c.clone(), *o))
            .collect();
        let mut bindings: Vec<Binding> = std::mem::take(&mut self.bindings)
            .into_iter()
            .filter(|((c, role, o), _)| {
                !(*role == ContractRole::Consumes && provided.contains(&(c.clone(), *o)))
            })
            .map(|(_, b)| Binding {
                symbol: resolve_owner(b.owner),
                contract: b.contract,
                role: b.role,
                span: Some(b.span),
                confidence: b.confidence,
                origin: None,
            })
            .collect();
        bindings.sort_by_cached_key(|b| (b.contract.canonical(), b.role, b.symbol));
        symbols.sort_by_cached_key(|s| s.name.canonical());
        links.sort_by(|a, b| (a.from, &a.kind, a.to).cmp(&(b.from, &b.kind, b.to)));

        let (fdialect, fsource) = resolve(input.path.as_str(), ctx);
        let mut facts: BTreeMap<String, Fact> = BTreeMap::new();
        facts.insert(
            "dialect".into(),
            s(fdialect.map_or("auto", Dialect::as_str)),
        );
        facts.insert(
            "dialect_source".into(),
            s(&match fsource {
                DialectSource::Suffix => "suffix".to_string(),
                DialectSource::Context(p) => p,
                DialectSource::Auto => "auto".to_string(),
            }),
        );
        facts.insert("statements".into(), Fact::Int(self.statements));
        facts.insert("unparsed_statements".into(), Fact::Int(self.unparsed));
        facts.insert("dml_statements".into(), Fact::Int(self.dml));
        facts.insert("default_schema".into(), s(ctx.default_schema()));
        facts.insert(
            "default_schema_source".into(),
            s(ctx.schema.as_ref().map_or("default", |c| c.source.as_str())),
        );
        if !ctx.schema_ignored.is_empty() {
            facts.insert(
                "default_schema_ignored".into(),
                Fact::List(ctx.schema_ignored.iter().map(|c| s(&c.source)).collect()),
            );
        }
        if let Some(m) = parse_migration_name(input.path.as_str()) {
            let mut map = BTreeMap::new();
            map.insert("scheme".to_string(), s(m.scheme.as_str()));
            if let Some(v) = &m.version {
                map.insert("version".to_string(), s(v));
            }
            map.insert("description".to_string(), s(&m.description));
            if let Some(o) = &m.order {
                map.insert("order".to_string(), s(o));
            }
            facts.insert("migration".into(), Fact::Map(map));
        }
        Fragment {
            provenance,
            extension: "sql".to_string(),
            artifact: ArtifactNode {
                name: artifact,
                facts,
            },
            symbols,
            links,
            refs: Vec::new(),
            exports: Vec::new(),
            bindings,
        }
    }
}

/// Extract one SQL file.
pub(crate) fn extract(input: &ExtractInput) -> (Fragment, Vec<ExtractError>) {
    let ctx = SqlContext::from_bytes(input.context);
    let text = String::from_utf8_lossy(input.content);
    let (dialect, _) = resolve(input.path.as_str(), &ctx);
    let mut b = Builder::new(&text, ctx.default_schema());
    b.run(dialect);
    let errors = std::mem::take(&mut b.errors);
    let frag = b.finish(input, &ctx);
    (frag, errors)
}
