//! Declaration and re-export collection from parsed items.
//!
//! Walks top-level (and nested module) items, turning each into the
//! declaration(s) it introduces: one per type or value item, one per inherent
//! method, and a single indivisible declaration per trait impl block.

use proc_macro2::Span;
use smol_str::SmolStr;
use syn::spanned::Spanned as _;
use syn::{ImplItem, Item, ItemImpl};

use super::declaration::{byte_offset, declaration};
use super::metadata::{has_cfg_test, has_test_attr, push_re_exports};
use super::references::collect_references;
use super::tokens::trait_impl_header;
use super::{DeclKind, Declaration, ReExport, RefKind, Reference};

/// Appends the declaration(s) introduced by one top-level item to `out`.
///
/// `cfg_test_ancestor` propagates `#[cfg(test)]` down from an enclosing module
/// so items nested in a test module inherit the test gate.
pub(super) fn push_item(
    item: &Item,
    contents: &str,
    cfg_test_ancestor: bool,
    out: &mut Vec<Declaration>,
    re_exports: &mut Vec<ReExport>,
) {
    match item {
        Item::Use(item_use) => push_re_exports(item_use, re_exports),
        Item::Fn(item_fn) => push_fn(item_fn, contents, cfg_test_ancestor, out),
        Item::Struct(item_struct) => push_type(
            &item_struct.ident.to_string(),
            &item_struct.vis,
            cfg_test_ancestor || has_cfg_test(&item_struct.attrs),
            item_struct.span(),
            contents,
            collect_references(item_struct),
            out,
        ),
        Item::Enum(item_enum) => push_type(
            &item_enum.ident.to_string(),
            &item_enum.vis,
            cfg_test_ancestor || has_cfg_test(&item_enum.attrs),
            item_enum.span(),
            contents,
            collect_references(item_enum),
            out,
        ),
        Item::Union(item_union) => push_type(
            &item_union.ident.to_string(),
            &item_union.vis,
            cfg_test_ancestor || has_cfg_test(&item_union.attrs),
            item_union.span(),
            contents,
            collect_references(item_union),
            out,
        ),
        Item::Trait(item_trait) => push_type(
            &item_trait.ident.to_string(),
            &item_trait.vis,
            cfg_test_ancestor || has_cfg_test(&item_trait.attrs),
            item_trait.span(),
            contents,
            collect_references(item_trait),
            out,
        ),
        Item::Type(item_type) => push_type(
            &item_type.ident.to_string(),
            &item_type.vis,
            cfg_test_ancestor || has_cfg_test(&item_type.attrs),
            item_type.span(),
            contents,
            collect_references(item_type),
            out,
        ),
        Item::Const(item_const) => out.push(declaration(
            &item_const.ident.to_string(),
            DeclKind::Symbol,
            &item_const.vis,
            cfg_test_ancestor || has_cfg_test(&item_const.attrs),
            item_const.span(),
            contents,
            collect_references(item_const),
        )),
        Item::Static(item_static) => out.push(declaration(
            &item_static.ident.to_string(),
            DeclKind::Symbol,
            &item_static.vis,
            cfg_test_ancestor || has_cfg_test(&item_static.attrs),
            item_static.span(),
            contents,
            collect_references(item_static),
        )),
        Item::Impl(item_impl) => {
            push_impl(item_impl, contents, cfg_test_ancestor, out);
        }
        Item::Mod(item_mod) => {
            let cfg_test = cfg_test_ancestor || has_cfg_test(&item_mod.attrs);
            if let Some((_, items)) = &item_mod.content {
                for nested in items {
                    push_item(nested, contents, cfg_test, out, re_exports);
                }
            }
        }
        _ => {}
    }
}

/// Appends a free function's declaration, marking `#[test]` functions as test
/// cases.
fn push_fn(
    item_fn: &syn::ItemFn,
    contents: &str,
    cfg_test_ancestor: bool,
    out: &mut Vec<Declaration>,
) {
    let cfg_test = cfg_test_ancestor || has_cfg_test(&item_fn.attrs);
    let mut decl = declaration(
        &item_fn.sig.ident.to_string(),
        DeclKind::Symbol,
        &item_fn.vis,
        cfg_test,
        item_fn.span(),
        contents,
        collect_references(item_fn),
    );
    decl.test_case = has_test_attr(&item_fn.attrs);
    out.push(decl);
}

/// Appends a type declaration to `out`.
fn push_type(
    name: &str,
    visibility: &syn::Visibility,
    cfg_test: bool,
    span: Span,
    contents: &str,
    references: Vec<Reference>,
    out: &mut Vec<Declaration>,
) {
    out.push(declaration(
        name,
        DeclKind::Type,
        visibility,
        cfg_test,
        span,
        contents,
        references,
    ));
}

/// Appends declarations for an `impl` block: one symbol per inherent method, or
/// — for a trait impl — a single declaration for the whole block (see
/// [`push_trait_impl`]).
fn push_impl(
    item_impl: &ItemImpl,
    contents: &str,
    cfg_test_ancestor: bool,
    out: &mut Vec<Declaration>,
) {
    let cfg_test = cfg_test_ancestor || has_cfg_test(&item_impl.attrs);

    // A trait impl is one indivisible Rust item: its methods cannot live in a
    // different file from the impl block, so the whole block is a single
    // declaration (ADR-19). It is named after its header and carries the
    // inheritance reference to the trait plus every item's references, so
    // calls into any method resolve to the block that must move as a unit.
    if item_impl.trait_.is_some() {
        push_trait_impl(item_impl, contents, cfg_test, out);

        return;
    }

    for impl_item in &item_impl.items {
        if let ImplItem::Fn(method) = impl_item {
            let mut decl = declaration(
                &method.sig.ident.to_string(),
                DeclKind::Symbol,
                &method.vis,
                cfg_test || has_cfg_test(&method.attrs),
                method.span(),
                contents,
                collect_references(method),
            );
            decl.test_case = has_test_attr(&method.attrs);
            out.push(decl);
        }
    }
}

/// Pushes an `impl Trait for T` block as one [`DeclKind::Type`] declaration.
///
/// The declaration is named from the block's header without the impl's own
/// generic parameter list (`impl From<Config> for Profile`), spans the whole
/// block, and carries the references of every method, associated type, and
/// constant, so its SLOC and dependencies travel together. A header already
/// used earlier in the same file — mutually exclusive `cfg` variants — is
/// disambiguated by source order: `impl Display for Report (2)`.
fn push_trait_impl(
    item_impl: &ItemImpl,
    contents: &str,
    cfg_test: bool,
    out: &mut Vec<Declaration>,
) {
    let Some((_, trait_path, _)) = &item_impl.trait_ else {
        return;
    };
    let Some(segment) = trait_path.segments.last() else {
        return;
    };
    let header = trait_impl_header(item_impl);
    let earlier = out
        .iter()
        .filter(|decl| {
            decl.kind == DeclKind::Type
                && decl.name.strip_prefix(header.as_str()).is_some_and(|rest| {
                    rest.is_empty() || (rest.starts_with(" (") && rest.ends_with(')'))
                })
        })
        .count();
    let name = if earlier == 0 {
        header
    } else {
        format!("{header} ({})", earlier + 1)
    };

    let mut references = vec![Reference {
        kind: RefKind::TraitImpl,
        name: SmolStr::new(segment.ident.to_string()),
        offset: byte_offset(segment.ident.span()),
        macro_expanded: false,
    }];
    for impl_item in &item_impl.items {
        references.extend(collect_references(impl_item));
    }

    out.push(declaration(
        &name,
        DeclKind::Type,
        &syn::Visibility::Inherited,
        cfg_test,
        item_impl.span(),
        contents,
        references,
    ));
}
