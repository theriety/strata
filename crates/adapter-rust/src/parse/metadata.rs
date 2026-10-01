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
pub(super) fn push_re_exports(item_use: &syn::ItemUse, re_exports: &mut Vec<ReExport>) {
    if is_public(&item_use.vis) {
        let (visibility_kind, visibility_path) = visibility(&item_use.vis);
        collect_re_exports(
            &item_use.tree,
            visibility_kind,
            visibility_path.as_ref(),
            re_exports,
        );
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
        }),
        syn::UseTree::Rename(rename) => re_exports.push(ReExport {
            name: SmolStr::new(rename.rename.to_string()),
            visibility_kind,
            visibility_path: visibility_path.cloned(),
            offset: byte_offset(rename.ident.span()),
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

/// Returns `true` when any attribute is `#[cfg(test)]`.
pub(super) fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(is_cfg_test_attr)
}

/// Returns `true` when a single attribute is exactly `#[cfg(test)]`.
fn is_cfg_test_attr(attr: &syn::Attribute) -> bool {
    if !attr.path().is_ident("cfg") {
        return false;
    }
    let mut is_test = false;
    let _ = attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("test") {
            is_test = true;
        }
        Ok(())
    });
    is_test
}
