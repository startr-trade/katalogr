//! `use` trees, re-exports and path resolution to export keys (FEAT-005 section 4,
//! "Bindings"). Resolution is by module and import only; there is no type inference
//! (ADR-0015).

use crate::extract::{Builder, Target, attr_texts};
use katalogr_model::{Confidence, ExportKey, Fact};
use ra_ap_syntax::{
    AstNode,
    ast::{self, HasAttrs, HasName, HasVisibility, PathSegmentKind},
};

/// Names that start a path in another crate that is not in the repository.
const STD_ROOTS: [&str; 3] = ["std", "core", "alloc"];

/// The segments of a path as strings; `None` for a qualified path (`<T as Trait>::f`).
pub(crate) fn segments_of(path: &ast::Path) -> Option<Vec<String>> {
    path.segments()
        .map(|s| match s.kind()? {
            PathSegmentKind::Name(n) => Some(n.text().to_string()),
            PathSegmentKind::SelfTypeKw => Some("Self".to_string()),
            PathSegmentKind::SelfKw => Some("self".to_string()),
            PathSegmentKind::SuperKw => Some("super".to_string()),
            PathSegmentKind::CrateKw => Some("crate".to_string()),
            PathSegmentKind::Type { .. } => None,
        })
        .collect()
}

/// One imported name or glob of a `use` item.
enum Leaf {
    Name { alias: String, path: Vec<String> },
    Glob { path: Vec<String> },
}

fn leaves(tree: &ast::UseTree, prefix: &[String], out: &mut Vec<Leaf>) {
    let mut path = prefix.to_vec();
    if let Some(p) = tree.path() {
        match segments_of(&p) {
            Some(segs) => path.extend(segs),
            None => return,
        }
    }
    if let Some(list) = tree.use_tree_list() {
        for sub in list.use_trees() {
            leaves(&sub, &path, out);
        }
    } else if tree.star_token().is_some() {
        out.push(Leaf::Glob { path });
    } else if !path.is_empty() {
        // `use a::b::{self}` imports `b`.
        if path.len() > 1 && path.last().is_some_and(|s| s == "self") {
            path.pop();
        }
        let alias = match tree.rename() {
            Some(r) => match r.name() {
                Some(n) => n.text().to_string(),
                None => return, // `as _` binds nothing
            },
            None => path.last().cloned().unwrap_or_default(),
        };
        out.push(Leaf::Name { alias, path });
    }
}

impl Builder<'_> {
    /// Resolve the path of a `use` (no local bindings are consulted).
    pub(crate) fn abs_use(&self, path: &[String]) -> Option<Target> {
        let own = self.own.as_ref()?;
        let first = path.first()?;
        let rest = &path[1..];
        let mp = self.module_path();
        match first.as_str() {
            "crate" => Some(Target::Crate(own.clone(), rest.to_vec())),
            "self" => Some(Target::Crate(
                own.clone(),
                mp.iter().chain(rest).cloned().collect(),
            )),
            "super" => {
                let mut up = mp.len().saturating_sub(1);
                let mut rest = rest;
                while rest.first().is_some_and(|s| s == "super") {
                    up = up.saturating_sub(1);
                    rest = &rest[1..];
                }
                Some(Target::Crate(
                    own.clone(),
                    mp[..up].iter().chain(rest).cloned().collect(),
                ))
            }
            f if STD_ROOTS.contains(&f) => None,
            f => {
                if let Some(dep) = self.krate.and_then(|c| c.deps.iter().find(|d| d.key == f)) {
                    return Some(match &dep.path {
                        Some(p) => Target::Crate(crate::context::key_dir(p), rest.to_vec()),
                        None => Target::Package(dep.package.clone()),
                    });
                }
                // The crate's own library, named from a binary or a test.
                if self.krate.is_some_and(|c| c.lib_name == f) {
                    return Some(Target::Crate(own.clone(), rest.to_vec()));
                }
                Some(Target::Crate(
                    own.clone(),
                    mp.iter().chain(path).cloned().collect(),
                ))
            }
        }
    }

    /// Candidate export keys for a path used in an expression or a type, most specific first:
    /// the module's own bindings, then the current module (and, for types, the crate root),
    /// then the glob imports.
    pub(crate) fn candidates(&self, path: &[String], for_type: bool) -> Vec<ExportKey> {
        let Some(first) = path.first() else {
            return Vec::new();
        };
        let rest = &path[1..];
        let mut out: Vec<Target> = Vec::new();
        let own = self.own.clone();
        let mp = self.module_path();
        if first == "Self" {
            if let (Some(own), Some(s)) = (&own, &self.self_ident) {
                let segs = mp
                    .iter()
                    .cloned()
                    .chain(std::iter::once(s.clone()))
                    .chain(rest.iter().cloned())
                    .collect();
                out.push(Target::Crate(own.clone(), segs));
            }
        } else if let Some(b) = self.scope().bindings.get(first) {
            out.push(b.extended(rest));
        } else {
            let is_dep = self
                .krate
                .is_some_and(|c| c.deps.iter().any(|d| &d.key == first) || &c.lib_name == first);
            if matches!(first.as_str(), "crate" | "self" | "super")
                || is_dep
                || STD_ROOTS.contains(&first.as_str())
            {
                out.extend(self.abs_use(path));
            } else if let Some(own) = &own {
                out.push(Target::Crate(
                    own.clone(),
                    mp.iter().chain(path).cloned().collect(),
                ));
                if for_type && !mp.is_empty() {
                    out.push(Target::Crate(own.clone(), path.to_vec()));
                }
                for g in &self.scope().globs {
                    out.push(g.extended(path));
                }
            }
        }
        out.iter().map(Target::key).collect()
    }

    /// Read every `use` of a module into its scope.
    pub(crate) fn bind_uses(&mut self, u: &ast::Use) {
        let Some(tree) = u.use_tree() else { return };
        let mut ls = Vec::new();
        leaves(&tree, &[], &mut ls);
        for l in ls {
            match l {
                Leaf::Name { alias, path } => {
                    if let Some(t) = self.abs_use(&path) {
                        self.scopes
                            .last_mut()
                            .expect("scope")
                            .bindings
                            .entry(alias)
                            .or_insert(t);
                    }
                }
                Leaf::Glob { path } => {
                    if let Some(t) = self.abs_use(&path) {
                        self.scopes.last_mut().expect("scope").globs.push(t);
                    }
                }
            }
        }
    }

    /// A `use` with a visibility re-exports: one symbol of kind `use` per imported name, which
    /// exports that name and references the original with `reexports`.
    pub(crate) fn reexports(&mut self, u: &ast::Use) {
        let Some(vis) = u.visibility() else { return };
        let Some(tree) = u.use_tree() else { return };
        let mut ls = Vec::new();
        leaves(&tree, &[], &mut ls);
        let vis_text: String = vis.syntax().text().to_string().split_whitespace().collect();
        for l in ls {
            let (name, path, glob) = match l {
                Leaf::Name { alias, path } => (alias, path, false),
                Leaf::Glob { path } => {
                    let mut n = path.join("::");
                    n.push_str("::*");
                    (n, path, true)
                }
            };
            let mut facts = std::collections::BTreeMap::new();
            facts.insert("vis".to_string(), Fact::Str(vis_text.clone()));
            let attrs = attr_texts(u.attrs());
            if !attrs.is_empty() {
                facts.insert("attrs".to_string(), Fact::List(attrs));
            }
            facts.insert(
                "signature".to_string(),
                Fact::Str(crate::extract::signature(u.syntax())),
            );
            let id = self.add_symbol("use", &name, u.syntax(), facts);
            if !glob {
                self.export_item(&[&name], id);
            }
            let cands: Vec<ExportKey> = self.abs_use(&path).iter().map(Target::key).collect();
            let span = self.span_of(u.syntax());
            self.add_ref(id, "reexports", span, cands, Confidence::Extracted);
        }
    }
}
