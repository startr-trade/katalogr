//! `impl` blocks: references from the impl to its self type and its trait (FEAT-005 section 4,
//! "Impls").

use crate::bindings::segments_of;
use crate::extract::{Builder, normalise};
use katalogr_model::Confidence;
use ra_ap_syntax::{
    AstNode,
    ast::{self, HasAttrs, HasVisibility},
};

/// The path of a type that names a type: `Foo`, `a::Foo<T>`, `&Foo`.
fn type_path(ty: &ast::Type) -> Option<Vec<String>> {
    match ty {
        ast::Type::PathType(p) => segments_of(&p.path()?),
        ast::Type::RefType(r) => type_path(&r.ty()?),
        ast::Type::ParenType(p) => type_path(&p.ty()?),
        _ => None,
    }
}

impl Builder<'_> {
    pub(crate) fn imp(&mut self, i: &ast::Impl) {
        let Some(self_ty) = i.self_ty() else { return };
        let self_text = normalise(&self_ty.syntax().text().to_string());
        let trait_ty = i.trait_();
        let name = match &trait_ty {
            Some(t) => format!(
                "{} for {}",
                normalise(&t.syntax().text().to_string()),
                self_text
            ),
            None => self_text,
        };
        let mut facts = self.facts(i.visibility(), i.attrs(), i.syntax());
        if i.unsafe_token().is_some() {
            facts.insert("unsafe".to_string(), katalogr_model::Fact::Bool(true));
        }
        let id = self.add_symbol("impl", &name, i.syntax(), facts);

        let self_path = type_path(&self_ty);
        if let Some(path) = &self_path {
            let cands = self.candidates(path, true);
            let span = self.span_of(self_ty.syntax());
            self.add_ref(id, "implements-for", span, cands, Confidence::Extracted);
        }
        if let Some(t) = &trait_ty
            && let Some(path) = type_path(t)
        {
            let cands = self.candidates(&path, true);
            let span = self.span_of(t.syntax());
            self.add_ref(id, "implements", span, cands, Confidence::Extracted);
        }

        let owner = self_path.as_ref().and_then(|p| p.last().cloned());
        self.enter("impl", &name, id);
        let saved = std::mem::replace(&mut self.self_ident, owner.clone());
        if let Some(list) = i.assoc_item_list() {
            self.assoc_items(list, owner.as_deref().unwrap_or(""));
        }
        self.self_ident = saved;
        self.leave();
    }
}
