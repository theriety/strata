//! Visibility, test-attribute, and re-export metadata extraction.
//!
//! Reads the attributes and visibility qualifiers that classify a declaration
//! (public surface, `#[cfg(test)]` gating, test entry points) and records the
//! names a `pub use` re-exports.

use smol_str::SmolStr;

use super::declaration::byte_offset;
use super::{ReExport, VisibilityKind};

/// Records the names a `pub use` re-exports through this module; a plain `use`
/// is a private import and contributes no export surface.
///
/// `cfg_test_ancestor` carries an enclosing `cfg(test)` module's gate; the
/// `use`'s own `#[cfg(test)]` adds to it.
pub(super) fn push_re_exports(
    item_use: &syn::ItemUse,
    cfg_test_ancestor: bool,
    re_exports: &mut Vec<ReExport>,
) {
    if is_public(&item_use.vis) {
        let (visibility_kind, visibility_path) = visibility(&item_use.vis);
        let start = re_exports.len();
        collect_re_exports(
            &item_use.tree,
            visibility_kind,
            visibility_path.as_ref(),
            re_exports,
        );
        let cfg_test = cfg_test_ancestor || has_cfg_test(&item_use.attrs);
        for re_export in re_exports.iter_mut().skip(start) {
            re_export.cfg_test = cfg_test;
        }
    }
}

/// Returns `true` when any attribute is a `#[test]`-family attribute (`#[test]`,
/// `#[tokio::test]`, `#[test_case]`, …): the marker of a test-case entry point.
pub(super) fn has_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "test")
    })
}

/// Walks a `use` tree, recording one [`ReExport`] per re-exported leaf name.
///
/// `use a::b::C;` yields the leaf `C` (resolved through its byte offset);
/// `use a::b::{C, D};` yields both; `use a::b::C as E;` binds the renamed `E` but
/// resolves through the original `C` offset. Glob (`use a::*`) re-exports bind no
/// individual name and are skipped — the engine sees only named re-exports.
fn collect_re_exports(
    tree: &syn::UseTree,
    visibility_kind: VisibilityKind,
    visibility_path: Option<&SmolStr>,
    re_exports: &mut Vec<ReExport>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            collect_re_exports(&path.tree, visibility_kind, visibility_path, re_exports);
        }
        syn::UseTree::Name(name) => re_exports.push(ReExport {
            name: SmolStr::new(name.ident.to_string()),
            visibility_kind,
            visibility_path: visibility_path.cloned(),
            offset: byte_offset(name.ident.span()),
            cfg_test: false,
        }),
        syn::UseTree::Rename(rename) => re_exports.push(ReExport {
            name: SmolStr::new(rename.rename.to_string()),
            visibility_kind,
            visibility_path: visibility_path.cloned(),
            offset: byte_offset(rename.ident.span()),
            cfg_test: false,
        }),
        syn::UseTree::Group(group) => {
            for nested in &group.items {
                collect_re_exports(nested, visibility_kind, visibility_path, re_exports);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

/// Returns `true` when `vis` is any form of `pub`.
pub(super) fn is_public(vis: &syn::Visibility) -> bool {
    matches!(
        vis,
        syn::Visibility::Public(_) | syn::Visibility::Restricted(_)
    )
}

/// Returns explicit visibility metadata without deriving it from `exported`.
pub(super) fn visibility(vis: &syn::Visibility) -> (VisibilityKind, Option<SmolStr>) {
    match vis {
        syn::Visibility::Inherited => (VisibilityKind::Inherited, None),
        syn::Visibility::Public(_) => (VisibilityKind::Public, None),
        syn::Visibility::Restricted(restricted) => {
            let path = restricted
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            if path == "crate" {
                (VisibilityKind::Crate, None)
            } else {
                (VisibilityKind::Restricted, Some(SmolStr::new(path)))
            }
        }
    }
}

/// Returns `true` when any `#[cfg(..)]` attribute implies `test`: the item
/// exists only when `cfg(test)` holds. Doubt resolves to production.
pub(super) fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .filter_map(cfg_predicate)
        .any(|predicate| implies_test(&predicate))
}

/// The predicate inside a `#[cfg(..)]` attribute.
fn cfg_predicate(attr: &syn::Attribute) -> Option<syn::Meta> {
    if attr.path().is_ident("cfg") {
        attr.parse_args::<syn::Meta>().ok()
    } else {
        None
    }
}

/// The comma-separated arms of an `all`/`any`/`not` predicate.
fn arms(list: &syn::MetaList) -> Vec<syn::Meta> {
    list.parse_args_with(syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
        .map(|arms| arms.into_iter().collect())
        .unwrap_or_default()
}

/// Whether a predicate can only be true when `test` is set: `test` itself, an
/// `all` with such an arm, or an `any` whose every arm is such. `not` never is.
fn implies_test(predicate: &syn::Meta) -> bool {
    match predicate {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => arms(list).iter().any(implies_test),
        syn::Meta::List(list) if list.path.is_ident("any") => {
            let arms = arms(list);
            !arms.is_empty() && arms.iter().all(implies_test)
        }
        _ => false,
    }
}

/// Returns `true` when any `#[cfg(..)]` attribute is false whenever `test` is
/// set, as `cfg(not(test))` is: the semantic loader drops such an item. Doubt
/// (an unknown option decides the predicate) resolves to `false`.
pub(super) fn has_cfg_not_test(attrs: &[syn::Attribute]) -> bool {
    attrs
        .iter()
        .filter_map(cfg_predicate)
        .any(|predicate| holds_under_test(&predicate) == Some(false))
}

/// Three-valued evaluation with `test` set and every other option unknown.
fn holds_under_test(predicate: &syn::Meta) -> Option<bool> {
    let values = |list: &syn::MetaList| -> Vec<Option<bool>> {
        arms(list).iter().map(holds_under_test).collect()
    };
    match predicate {
        syn::Meta::Path(path) if path.is_ident("test") => Some(true),
        syn::Meta::List(list) if list.path.is_ident("not") => arms(list)
            .first()
            .and_then(holds_under_test)
            .map(|held| !held),
        syn::Meta::List(list) if list.path.is_ident("all") => {
            let values = values(list);
            if values.contains(&Some(false)) {
                Some(false)
            } else {
                values
                    .iter()
                    .all(|held| *held == Some(true))
                    .then_some(true)
            }
        }
        syn::Meta::List(list) if list.path.is_ident("any") => {
            let values = values(list);
            if values.contains(&Some(true)) {
                Some(true)
            } else {
                values
                    .iter()
                    .all(|held| *held == Some(false))
                    .then_some(false)
            }
        }
        _ => None,
    }
}
