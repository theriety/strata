//! Advisory names for code the semantic loader cannot see.
//!
//! The loader runs with `cfg(test)` set and no feature, so it may drop any item
//! or file behind another `#[cfg(..)]` (`not(test)`, a feature, a platform) from
//! its module tree, and nothing inside resolves (`loader_may_drop`). Rule: every
//! identifier in such code counts as a use of every same-named declaration, from
//! the enclosing declaration, for visibility advice only (ADR-25). That is
//! conservative by construction: a name collision can hide a finding but never
//! creates a wrong one. These names never become graph edges.

use std::ops::Range;

use proc_macro2::{TokenStream, TokenTree};
use smol_str::SmolStr;
use syn::spanned::Spanned as _;
use syn::{ImplItem, Item};

use super::dropped::DroppedUses;
use super::metadata::loader_may_drop;
use super::{Declaration, ParsedFile};

/// Records the identifiers of every declaration of a file the loader may drop
/// whole, and every name its `use` items import.
pub(super) fn mark_all(file: &mut ParsedFile, source: &str) {
    for declaration in &mut file.declarations {
        record(declaration, source);
    }
    if let Ok(ast) = syn::parse_file(source) {
        let mut dropped = DroppedUses::default();
        dropped.collect(&ast.items, true);
        file.dropped_uses = dropped.imported;
        file.test_dropped_uses = dropped.test_imported;
        file.dropped_re_exports = dropped.re_exported;
        file.dropped_globs = dropped.globs;
    }
}

/// Records identifiers for declarations inside an inline item the loader may drop.
pub(super) fn mark_inline(declarations: &mut [Declaration], items: &[Item], source: &str) {
    let mut ranges = Vec::new();
    gated_ranges(items, &mut ranges);
    for declaration in declarations {
        let start = declaration.byte_start as usize;
        // an enclosing gated item, or any maybe-dropped cfg anywhere inside the
        // declaration (statement, field, variant, arm, trait item, ..).
        let span = declaration.byte_start as usize..declaration.byte_end as usize;
        let inside = source
            .get(span)
            .and_then(|text| text.parse::<TokenStream>().ok())
            .is_some_and(contains_dropped_cfg);
        if inside || ranges.iter().any(|range| range.contains(&start)) {
            record(declaration, source);
        }
    }
}

/// Whether any attribute among the tokens, at any depth, is a `cfg` the
/// loader may drop. One rule for every syntax position a cfg can sit in.
fn contains_dropped_cfg(stream: TokenStream) -> bool {
    let mut previous: Option<TokenTree> = None;
    for tree in stream {
        if let TokenTree::Group(group) = &tree {
            let after_hash =
                matches!(&previous, Some(TokenTree::Punct(punct)) if punct.as_char() == '#');
            if group.delimiter() == proc_macro2::Delimiter::Bracket && after_hash {
                let attribute: TokenStream = [
                    TokenTree::Punct(proc_macro2::Punct::new('#', proc_macro2::Spacing::Alone)),
                    tree.clone(),
                ]
                .into_iter()
                .collect();
                let parsed = syn::parse::Parser::parse2(syn::Attribute::parse_outer, attribute);
                if parsed.is_ok_and(|attributes| loader_may_drop(&attributes)) {
                    return true;
                }
            }
            if contains_dropped_cfg(group.stream()) {
                return true;
            }
        }
        previous = Some(tree);
    }
    false
}

/// Appends every identifier of `stream` to `out`, descending into groups.
pub(super) fn token_names(stream: TokenStream, out: &mut Vec<SmolStr>) {
    collect(stream, out);
}

/// Sets the declaration's advisory names from its own source text.
fn record(declaration: &mut Declaration, source: &str) {
    let range = declaration.byte_start as usize..declaration.byte_end as usize;
    if let Some(text) = source.get(range) {
        declaration.advisory_names = identifiers(text);
    }
}

/// Byte ranges of the items and impl methods the loader may drop.
fn gated_ranges(items: &[Item], out: &mut Vec<Range<usize>>) {
    for item in items {
        if loader_may_drop(item_attrs(item)) {
            out.push(item.span().byte_range());
        } else if let Item::Mod(item_mod) = item
            && let Some((_, nested)) = &item_mod.content
        {
            gated_ranges(nested, out);
        } else if let Item::Impl(item_impl) = item {
            for impl_item in &item_impl.items {
                if let ImplItem::Fn(method) = impl_item
                    && loader_may_drop(&method.attrs)
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
pub(super) fn identifiers(text: &str) -> Vec<SmolStr> {
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
    use crate::parse::metadata::loader_may_drop;

    fn dropped(attr: &str) -> bool {
        let source = format!("{attr}\nfn f() {{}}");
        syn::parse_str::<syn::ItemFn>(&source).is_ok_and(|item| loader_may_drop(&item.attrs))
    }

    #[test]
    fn should_treat_every_cfg_but_a_certain_or_test_implying_one_as_maybe_dropped() {
        for attr in [
            "#[cfg(not(test))]",
            "#[cfg(feature = \"x\")]",
            "#[cfg(windows)]",
            "#[cfg(all(windows, unix))]",
            "#[cfg(not(any(test, unix)))]",
            "#[cfg(all(test, feature = \"x\"))]",
        ] {
            assert!(dropped(attr), "{attr}");
        }
        for attr in [
            "",
            "#[cfg(test)]",
            "#[cfg(any(test, windows))]",
            "#[inline]",
        ] {
            assert!(!dropped(attr), "{attr}");
        }
    }

    #[test]
    fn should_collect_receiver_method_names_from_a_body() {
        let names = identifiers("fn drive(h: &Helper) { h.run(); }");

        assert!(names.iter().any(|name| name == "run"));
        assert!(names.iter().any(|name| name == "Helper"));
    }
}
