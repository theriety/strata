//! `use` items the loader may drop (ADR-25 rule 7).
//!
//! A dropped `use` has no declaration to carry advisory names, so what it
//! imports is kept per file. The re-exporting ones (any visibility above
//! private) also set a floor the loader cannot see, and a glob imports names
//! nobody can list; binding turns both into advice exemptions.

use smol_str::SmolStr;
use syn::Item;

use super::advisory::token_names;
use super::metadata::loader_may_drop;

/// Names and globs of the `use` items the loader may drop.
#[derive(Default)]
pub(super) struct DroppedUses {
    /// Leaf names of every dropped `use`.
    pub(super) imported: Vec<SmolStr>,
    /// Leaf names of the dropped `use` items that are not private.
    pub(super) re_exported: Vec<SmolStr>,
    /// The `::`-joined path before each dropped glob (any visibility).
    pub(super) globs: Vec<SmolStr>,
}

impl DroppedUses {
    /// Collects the dropped `use` items of `items`; `inherited` says an
    /// enclosing module or file is already dropped.
    pub(super) fn collect(&mut self, items: &[Item], inherited: bool) {
        for item in items {
            match item {
                Item::Use(item_use) if inherited || loader_may_drop(&item_use.attrs) => {
                    let mut names = Vec::new();
                    leaf_names(&item_use.tree, &[], &mut names, &mut self.globs);
                    if !matches!(item_use.vis, syn::Visibility::Inherited) {
                        self.re_exported.extend(names.iter().cloned());
                    }
                    self.imported.extend(names);
                }
                Item::Mod(item_mod) => {
                    if let Some((_, nested)) = &item_mod.content {
                        self.collect(nested, inherited || loader_may_drop(&item_mod.attrs));
                    }
                }
                // an item-position macro (`cfg_if!`, ..) expands to code the
                // loader may show only in part: its names count as uses.
                Item::Macro(item_macro) => {
                    token_names(item_macro.mac.tokens.clone(), &mut self.imported);
                }
                Item::Impl(item_impl) => {
                    for nested in &item_impl.items {
                        if let syn::ImplItem::Macro(found) = nested {
                            token_names(found.mac.tokens.clone(), &mut self.imported);
                        }
                    }
                }
                Item::Trait(item_trait) => {
                    for nested in &item_trait.items {
                        if let syn::TraitItem::Macro(found) = nested {
                            token_names(found.mac.tokens.clone(), &mut self.imported);
                        }
                    }
                }
                _ => {}
            }
        }
        for list in [&mut self.imported, &mut self.re_exported, &mut self.globs] {
            list.sort();
            list.dedup();
        }
    }
}

/// The original and renamed leaf identifiers of a `use` tree; a glob is
/// recorded as the path before it.
fn leaf_names(
    tree: &syn::UseTree,
    prefix: &[String],
    out: &mut Vec<SmolStr>,
    globs: &mut Vec<SmolStr>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            let mut inner = prefix.to_vec();
            inner.push(path.ident.to_string());
            leaf_names(&path.tree, &inner, out, globs);
        }
        syn::UseTree::Name(name) => out.push(SmolStr::new(name.ident.to_string())),
        syn::UseTree::Rename(rename) => {
            out.push(SmolStr::new(rename.ident.to_string()));
            out.push(SmolStr::new(rename.rename.to_string()));
        }
        syn::UseTree::Group(group) => group
            .items
            .iter()
            .for_each(|nested| leaf_names(nested, prefix, out, globs)),
        syn::UseTree::Glob(_) => globs.push(SmolStr::new(prefix.join("::"))),
    }
}

/// Whether a `use` item that is not private sits anywhere among `items`
/// (named or glob): names may flow onward through it.
pub(super) fn has_pub_use(items: &[Item]) -> bool {
    items.iter().any(|item| match item {
        Item::Use(item_use) => !matches!(item_use.vis, syn::Visibility::Inherited),
        Item::Mod(item_mod) => item_mod
            .content
            .as_ref()
            .is_some_and(|(_, nested)| has_pub_use(nested)),
        _ => false,
    })
}
