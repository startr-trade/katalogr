//! Symbols, spans and attributes: the items of a module in source order (FEAT-005 section 4,
//! "Items").

use crate::extract::{Builder, NS_FILE, Scope, normalise};
use katalogr_model::{Confidence, ExportKey, Fact, Id};
use ra_ap_syntax::{
    AstNode,
    ast::{self, HasAttrs, HasModuleItem, HasName, HasVisibility},
};
use std::collections::BTreeMap;

impl Builder<'_> {
    /// Walk the items of one module (the file, or an inline `mod`). Imports are read first, so
    /// a call before its `use` still resolves.
    pub(crate) fn items(&mut self, items: Vec<ast::Item>) {
        for it in &items {
            if let ast::Item::Use(u) = it {
                self.bind_uses(u);
            }
        }
        for it in items {
            match it {
                ast::Item::Use(u) => self.reexports(&u),
                ast::Item::Module(m) => self.module(&m),
                ast::Item::Fn(f) => {
                    self.func(&f, None);
                }
                ast::Item::Struct(s) => {
                    if let Some(name) = s.name() {
                        let facts = self.facts(s.visibility(), s.attrs(), s.syntax());
                        self.simple("struct", name.text(), s.syntax(), facts);
                    }
                }
                ast::Item::Union(u) => {
                    if let Some(name) = u.name() {
                        let facts = self.facts(u.visibility(), u.attrs(), u.syntax());
                        self.simple("union", name.text(), u.syntax(), facts);
                    }
                }
                ast::Item::Const(c) => {
                    if let Some(name) = c.name() {
                        let facts = self.facts(c.visibility(), c.attrs(), c.syntax());
                        let id = self.simple("const", name.text(), c.syntax(), facts);
                        self.collect_calls(c.syntax(), id);
                    }
                }
                ast::Item::Static(s) => {
                    if let Some(name) = s.name() {
                        let mut facts = self.facts(s.visibility(), s.attrs(), s.syntax());
                        if s.unsafe_token().is_some() {
                            facts.insert("unsafe".to_string(), Fact::Bool(true));
                        }
                        let id = self.simple("static", name.text(), s.syntax(), facts);
                        self.collect_calls(s.syntax(), id);
                    }
                }
                ast::Item::TypeAlias(t) => {
                    if let Some(name) = t.name() {
                        let facts = self.facts(t.visibility(), t.attrs(), t.syntax());
                        self.simple("type", name.text(), t.syntax(), facts);
                    }
                }
                ast::Item::MacroRules(m) => {
                    if let Some(name) = m.name() {
                        let facts = self.facts(m.visibility(), m.attrs(), m.syntax());
                        let id = self.simple("macro", name.text(), m.syntax(), facts);
                        self.export(
                            ExportKey {
                                namespace: crate::extract::NS_MACRO.to_string(),
                                key: name.text().to_string(),
                            },
                            id,
                        );
                    }
                }
                ast::Item::Enum(e) => self.enumeration(&e),
                ast::Item::Trait(t) => self.tr(&t),
                ast::Item::Impl(i) => self.imp(&i),
                ast::Item::ExternBlock(b) => self.extern_block(&b),
                // Macro invocations and `extern crate` define no symbol.
                _ => {}
            }
        }
    }

    /// `extern "<abi>" { ... }` (FEAT-016): each foreign `fn`, `static` and `type` becomes a
    /// symbol in the current module, exported as `("rs-item", ...)` like any item, with the
    /// usual facts plus `abi` (`"C"` when omitted) and `extern_attrs` (the block's attribute
    /// texts, when any). Foreign items have no body, so no calls are collected.
    fn extern_block(&mut self, b: &ast::ExternBlock) {
        let abi = b.abi().and_then(|a| a.string_token()).map_or_else(
            || "C".to_string(),
            |t| t.text().trim_matches('"').to_string(),
        );
        let block_attrs = crate::extract::attr_texts(b.attrs());
        let Some(list) = b.extern_item_list() else {
            return;
        };
        for item in list.extern_items() {
            let before = self.symbols.len();
            match item {
                ast::ExternItem::Fn(f) => self.func(&f, None),
                ast::ExternItem::Static(s) => {
                    if let Some(name) = s.name() {
                        let mut facts = self.facts(s.visibility(), s.attrs(), s.syntax());
                        if s.unsafe_token().is_some() {
                            facts.insert("unsafe".to_string(), Fact::Bool(true));
                        }
                        self.simple("static", name.text(), s.syntax(), facts);
                    }
                }
                ast::ExternItem::TypeAlias(t) => {
                    if let Some(name) = t.name() {
                        let facts = self.facts(t.visibility(), t.attrs(), t.syntax());
                        self.simple("type", name.text(), t.syntax(), facts);
                    }
                }
                ast::ExternItem::MacroCall(_) => {}
            }
            if let Some(sym) = self.symbols.get_mut(before) {
                sym.facts.insert("abi".to_string(), Fact::Str(abi.clone()));
                if !block_attrs.is_empty() {
                    sym.facts
                        .insert("extern_attrs".to_string(), Fact::List(block_attrs.clone()));
                }
            }
        }
    }

    /// A symbol that exports `("rs-item", ...)` and has no children.
    fn simple(
        &mut self,
        kind: &str,
        name: &str,
        node: &ra_ap_syntax::SyntaxNode,
        facts: BTreeMap<String, Fact>,
    ) -> Id {
        let id = self.add_symbol(kind, name, node, facts);
        self.export_item(&[name], id);
        id
    }

    /// A function or method. `owner` is the impl's self type or the trait name for members.
    pub(crate) fn func(&mut self, f: &ast::Fn, owner: Option<&str>) {
        let Some(name) = f.name() else { return };
        let name = name.text();
        let mut facts = self.facts(f.visibility(), f.attrs(), f.syntax());
        if f.async_token().is_some() {
            facts.insert("async".to_string(), Fact::Bool(true));
        }
        if f.unsafe_token().is_some() {
            facts.insert("unsafe".to_string(), Fact::Bool(true));
        }
        let id = self.add_symbol("fn", name, f.syntax(), facts);
        match owner {
            None => self.export_item(&[name], id),
            Some(o) => {
                self.export(
                    ExportKey {
                        namespace: crate::extract::NS_METHOD.to_string(),
                        key: name.to_string(),
                    },
                    id,
                );
                self.export_item(&[o, name], id);
            }
        }
        self.collect_calls(f.syntax(), id);
    }

    fn module(&mut self, m: &ast::Module) {
        let Some(name) = m.name() else { return };
        let name = name.text().to_string();
        let facts = self.facts(m.visibility(), m.attrs(), m.syntax());
        let id = self.simple("mod", &name, m.syntax(), facts);
        if let Some(list) = m.item_list() {
            let mut path = self.module_path();
            path.push(name.clone());
            let mut fs = self.scope().fs.clone();
            fs.push(name.clone());
            self.scopes.push(Scope {
                path,
                fs,
                bindings: BTreeMap::new(),
                globs: Vec::new(),
            });
            self.enter("mod", &name, id);
            self.items(list.items().collect());
            self.leave();
            self.scopes.pop();
        } else {
            self.declares(m, &name, id);
        }
    }

    /// `mod x;`: the file that holds the module, as `declares` candidates (`#[path]` first).
    fn declares(&mut self, m: &ast::Module, name: &str, id: Id) {
        let mut base = self.child_dir.clone();
        for seg in &self.scope().fs {
            base = crate::context::join_norm(&base, seg).unwrap_or_default();
        }
        let path_attr = m.attrs().find_map(|a| {
            let text = normalise(&a.meta()?.syntax().text().to_string());
            let rest = text.strip_prefix("path")?.trim_start().strip_prefix('=')?;
            let rest = rest.trim();
            Some(rest.trim_matches('"').to_string())
        });
        let paths: Vec<String> = match path_attr {
            Some(p) => crate::context::join_norm(&self.child_dir, &p)
                .into_iter()
                .collect(),
            None => [format!("{name}.rs"), format!("{name}/mod.rs")]
                .iter()
                .filter_map(|p| crate::context::join_norm(&base, p))
                .collect(),
        };
        let span = self.span_of(m.syntax());
        let cands = paths
            .into_iter()
            .map(|p| ExportKey {
                namespace: NS_FILE.to_string(),
                key: p,
            })
            .collect();
        self.add_ref(id, "declares", span, cands, Confidence::Extracted);
    }

    fn enumeration(&mut self, e: &ast::Enum) {
        let Some(name) = e.name() else { return };
        let name = name.text().to_string();
        let facts = self.facts(e.visibility(), e.attrs(), e.syntax());
        let id = self.simple("enum", &name, e.syntax(), facts);
        self.enter("enum", &name, id);
        if let Some(list) = e.variant_list() {
            for v in list.variants() {
                let Some(vname) = v.name() else { continue };
                let mut facts = BTreeMap::new();
                let attrs = crate::extract::attr_texts(v.attrs());
                if !attrs.is_empty() {
                    facts.insert("attrs".to_string(), Fact::List(attrs));
                }
                facts.insert(
                    "signature".to_string(),
                    Fact::Str(crate::extract::signature(v.syntax())),
                );
                let vid = self.add_symbol("variant", vname.text(), v.syntax(), facts);
                self.export_item(&[&name, vname.text()], vid);
            }
        }
        self.leave();
    }

    fn tr(&mut self, t: &ast::Trait) {
        let Some(name) = t.name() else { return };
        let name = name.text().to_string();
        let mut facts = self.facts(t.visibility(), t.attrs(), t.syntax());
        if t.unsafe_token().is_some() {
            facts.insert("unsafe".to_string(), Fact::Bool(true));
        }
        let id = self.simple("trait", &name, t.syntax(), facts);
        self.enter("trait", &name, id);
        let saved = self.self_ident.replace(name.clone());
        if let Some(list) = t.assoc_item_list() {
            self.assoc_items(list, &name);
        }
        self.self_ident = saved;
        self.leave();
    }

    /// Members of an impl or trait.
    pub(crate) fn assoc_items(&mut self, list: ast::AssocItemList, owner: &str) {
        for item in list.assoc_items() {
            match item {
                ast::AssocItem::Fn(f) => self.func(&f, Some(owner)),
                ast::AssocItem::Const(c) => {
                    if let Some(name) = c.name() {
                        let facts = self.facts(c.visibility(), c.attrs(), c.syntax());
                        let id = self.add_symbol("const", name.text(), c.syntax(), facts);
                        self.export_item(&[owner, name.text()], id);
                        self.collect_calls(c.syntax(), id);
                    }
                }
                ast::AssocItem::TypeAlias(t) => {
                    if let Some(name) = t.name() {
                        let facts = self.facts(t.visibility(), t.attrs(), t.syntax());
                        let id = self.add_symbol("type", name.text(), t.syntax(), facts);
                        self.export_item(&[owner, name.text()], id);
                    }
                }
                ast::AssocItem::MacroCall(_) => {}
            }
        }
    }
}
