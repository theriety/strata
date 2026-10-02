//! Advisory names for code the semantic loader cannot see.
//!
//! The loader runs with `cfg(test)` set, so it drops `#[cfg(not(test))]` items
//! and files from its module tree and nothing inside them resolves. Rule: every
//! identifier in such code counts as a use of every same-named declaration, from
//! the enclosing declaration, for visibility advice only (ADR-25). That is
//! conservative by construction: a name collision can hide a finding but never
//! creates a wrong one. These names never become graph edges.

use std::ops::Range;

use proc_macro2::{TokenStream, TokenTree};
use smol_str::SmolStr;
use syn::spanned::Spanned as _;
use syn::{ImplItem, Item};

use super::Declaration;
use super::metadata::has_cfg_not_test;

/// Records the identifiers of every declaration in `declarations`.
pub(super) fn mark_all(declarations: &mut [Declaration], source: &str) {
    for declaration in declarations {
        record(declaration, source);
    }
}

/// Records identifiers for declarations inside an inline `cfg(not(test))` item.
pub(super) fn mark_inline(declarations: &mut [Declaration], items: &[Item], source: &str) {
    let mut ranges = Vec::new();
    gated_ranges(items, &mut ranges);
    for declaration in declarations {
        let start = declaration.byte_start as usize;
        if ranges.iter().any(|range| range.contains(&start)) {
            record(declaration, source);
        }
    }
}

/// Sets the declaration's advisory names from its own source text.
fn record(declaration: &mut Declaration, source: &str) {
    let range = declaration.byte_start as usize..declaration.byte_end as usize;
    if let Some(text) = source.get(range) {
        declaration.advisory_names = identifiers(text);
    }
}

/// Byte ranges of the items and impl methods gated by `cfg(not(test))`.
fn gated_ranges(items: &[Item], out: &mut Vec<Range<usize>>) {
    for item in items {
        if has_cfg_not_test(item_attrs(item)) {
            out.push(item.span().byte_range());
        } else if let Item::Mod(item_mod) = item
            && let Some((_, nested)) = &item_mod.content
        {
            gated_ranges(nested, out);
        } else if let Item::Impl(item_impl) = item {
            for impl_item in &item_impl.items {
                if let ImplItem::Fn(method) = impl_item
                    && has_cfg_not_test(&method.attrs)
                {
                    out.push(method.span().byte_range());
                }
            }
        }
    }
}

/// The outer attributes of the item kinds that produce declarations.
fn item_attrs(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Fn(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::Union(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Const(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        _ => &[],
    }
}

/// The distinct identifiers of a source slice, sorted.
fn identifiers(text: &str) -> Vec<SmolStr> {
    let mut found = Vec::new();
    if let Ok(stream) = text.parse::<TokenStream>() {
        collect(stream, &mut found);
    }
    found.sort();
    found.dedup();
    found
}

/// Appends every identifier of `stream`, descending into groups.
fn collect(stream: TokenStream, out: &mut Vec<SmolStr>) {
    for tree in stream {
        match tree {
            TokenTree::Ident(ident) => out.push(SmolStr::new(ident.to_string())),
            TokenTree::Group(group) => collect(group.stream(), out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::identifiers;

    #[test]
    fn should_collect_receiver_method_names_from_a_body() {
        let names = identifiers("fn drive(h: &Helper) { h.run(); }");

        assert!(names.iter().any(|name| name == "run"));
        assert!(names.iter().any(|name| name == "Helper"));
    }
}
