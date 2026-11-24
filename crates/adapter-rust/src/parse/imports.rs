//! File-import statements: out-of-line `mod x;` declarations, `use` items, and
//! qualified paths that reach another module's items.
//!
//! A `mod x;` resolves at parse time through the module tree (see
//! [`super::modules`]). A `use` is only recorded here, as the leaves it names
//! with the byte offset of each; [`crate::bind`] resolves every leaf through the
//! semantic database, the way it resolves every other reference.
//!
//! A qualified path outside a `use` tree (`child::run()`, `crate::a::f()`,
//! `field: super::Config`, `impl crate::t::Tr for`, a pattern or a generic
//! argument) is the other way one file uses another's items without a `use`.
//! It is recorded with the offset of every segment, and binding resolves them
//! through the same database to find its module prefix and the item it reaches.
//!
//! Left out, by syntax:
//! - Paths inside a macro invocation's token tree (`println!("{}", a::f())`):
//!   syn never parses them, so they cannot be resolved here. They are not
//!   import statements, and a `use` inside a macro is left out the same way.
//! - Paths in attributes (`#[derive(a::D)]`, `#[a::attr]`): a derive or
//!   attribute macro lives in its own proc-macro crate, so it never names a
//!   module of the crate that uses it.
//! - A `pub(in crate::a)` visibility: it names an ancestor and uses none of its
//!   items.
//! - `Self::..` paths, which always start at a type.
//! - Code only compiled for tests: an item gated by `cfg(test)` (an inline
//!   `mod tests { .. }` included), a `#[test]` function, and a file gated by its
//!   own `#![cfg(test)]`. A test-only load is not a load of production code, as
//!   a Python import under `TYPE_CHECKING` is not; a file reached through a
//!   `cfg(test)` `mod` declaration is cleared by [`super::modules`].

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use syn::visit::Visit;

use super::declaration::byte_offset;
use super::metadata::{has_cfg_test, has_test_attr};

/// An out-of-line `mod x;` declaration that resolves to an analysed file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModImport {
    /// Repository-relative path of the module file (`x.rs` or `x/mod.rs`, or
    /// the `#[path]` target).
    pub imported: SmolStr,
    /// The declared module name, as written.
    pub name: SmolStr,
    /// 1-based line of the `mod` keyword.
    pub line: u32,
}

/// One name a `use` statement brings in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseLeaf {
    /// The leaf's full path as written, `::`-joined (`crate::a::B`,
    /// `crate::a::*` for a glob).
    pub path: SmolStr,
    /// 0-based byte offset of the identifier whose definition names the file
    /// the leaf reaches: the imported name, or the last segment before a glob.
    pub offset: u32,
}

/// A `use` item with the leaves it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UseStatement {
    /// 1-based line of the `use` keyword.
    pub line: u32,
    /// The leaves of the statement's tree, in source order.
    pub leaves: Vec<UseLeaf>,
}

/// One segment of a qualified path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathSegment {
    /// The segment as written (`crate`, `super`, `child`, `run`).
    pub name: SmolStr,
    /// 0-based byte offset of the segment, where it resolves.
    pub offset: u32,
}

/// A path of two or more segments outside a `use` tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QualifiedPath {
    /// Whether the path starts with `::`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub absolute: bool,
    /// The segments in source order, the last naming what the path uses.
    pub segments: Vec<PathSegment>,
    /// 1-based line the path starts on.
    pub line: u32,
}

/// The `use` items and qualified paths of a file, in source order.
#[derive(Debug, Default)]
pub(super) struct FileUses {
    /// Every `use` item.
    pub(super) uses: Vec<UseStatement>,
    /// Every path of two or more segments outside a `use` tree.
    pub(super) paths: Vec<QualifiedPath>,
}

/// The 1-based line a span starts on, saturating into `u32`.
pub(super) fn line_of(span: proc_macro2::Span) -> u32 {
    u32::try_from(span.start().line).unwrap_or(u32::MAX)
}

/// Every `use` item and qualified path of `file`, at any depth (inline modules
/// and function bodies included), in source order, leaving out what the module
/// documentation lists: macro token trees, attributes, restricted visibility,
/// `Self` paths, and test-only code.
pub(super) fn collect_uses(file: &syn::File) -> FileUses {
    let mut collector = UseCollector::default();
    if !is_test_only(&file.attrs) {
        collector.visit_file(file);
    }
    collector.found
}

/// Whether `attrs` confine their item to test builds: a `cfg` that implies
/// `test`, or a `#[test]`-family marker.
fn is_test_only(attrs: &[syn::Attribute]) -> bool {
    has_cfg_test(attrs) || has_test_attr(attrs)
}

/// The outer attributes of `item`; none for a kind syn gives none.
fn item_attrs(item: &syn::Item) -> &[syn::Attribute] {
    match item {
        syn::Item::Const(item) => &item.attrs,
        syn::Item::Enum(item) => &item.attrs,
        syn::Item::ExternCrate(item) => &item.attrs,
        syn::Item::Fn(item) => &item.attrs,
        syn::Item::ForeignMod(item) => &item.attrs,
        syn::Item::Impl(item) => &item.attrs,
        syn::Item::Macro(item) => &item.attrs,
        syn::Item::Mod(item) => &item.attrs,
        syn::Item::Static(item) => &item.attrs,
        syn::Item::Struct(item) => &item.attrs,
        syn::Item::Trait(item) => &item.attrs,
        syn::Item::TraitAlias(item) => &item.attrs,
        syn::Item::Type(item) => &item.attrs,
        syn::Item::Union(item) => &item.attrs,
        syn::Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

/// A syn visitor that gathers `use` items and qualified paths.
#[derive(Default)]
struct UseCollector {
    found: FileUses,
}

impl<'ast> Visit<'ast> for UseCollector {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        if !is_test_only(item_attrs(item)) {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        if !is_test_only(&item.attrs) {
            syn::visit::visit_impl_item_fn(self, item);
        }
    }

    fn visit_impl_item(&mut self, item: &'ast syn::ImplItem) {
        let attrs: &[syn::Attribute] = match item {
            syn::ImplItem::Const(item) => &item.attrs,
            syn::ImplItem::Fn(item) => &item.attrs,
            syn::ImplItem::Type(item) => &item.attrs,
            syn::ImplItem::Macro(item) => &item.attrs,
            _ => &[],
        };
        if !is_test_only(attrs) {
            syn::visit::visit_impl_item(self, item);
        }
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if !is_test_only(&field.attrs) {
            syn::visit::visit_field(self, field);
        }
    }

    fn visit_variant(&mut self, variant: &'ast syn::Variant) {
        if !is_test_only(&variant.attrs) {
            syn::visit::visit_variant(self, variant);
        }
    }

    fn visit_stmt(&mut self, stmt: &'ast syn::Stmt) {
        let attrs: &[syn::Attribute] = match stmt {
            syn::Stmt::Local(local) => &local.attrs,
            syn::Stmt::Macro(mac) => &mac.attrs,
            syn::Stmt::Item(item) => item_attrs(item),
            syn::Stmt::Expr(..) => &[],
        };
        if !is_test_only(attrs) {
            syn::visit::visit_stmt(self, stmt);
        }
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        if !is_test_only(&arm.attrs) {
            syn::visit::visit_arm(self, arm);
        }
    }

    fn visit_trait_item_fn(&mut self, item: &'ast syn::TraitItemFn) {
        if !is_test_only(&item.attrs) {
            syn::visit::visit_trait_item_fn(self, item);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let mut leaves = Vec::new();
        let prefix = if item.leading_colon.is_some() {
            vec![String::new()]
        } else {
            Vec::new()
        };
        collect_leaves(&item.tree, &prefix, None, &mut leaves);
        self.found.uses.push(UseStatement {
            line: line_of(item.use_token.span),
            leaves,
        });
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let starts_at_self = path
            .segments
            .first()
            .is_some_and(|segment| segment.ident == "Self");
        if path.segments.len() > 1 && !starts_at_self {
            let line = match (&path.leading_colon, path.segments.first()) {
                (Some(colon), _) => line_of(colon.spans[0]),
                (None, Some(first)) => line_of(first.ident.span()),
                (None, None) => 0,
            };
            self.found.paths.push(QualifiedPath {
                absolute: path.leading_colon.is_some(),
                segments: path
                    .segments
                    .iter()
                    .map(|segment| PathSegment {
                        name: SmolStr::new(segment.ident.to_string()),
                        offset: byte_offset(segment.ident.span()),
                    })
                    .collect(),
                line,
            });
        }
        // generic arguments hold paths of their own (`Vec<crate::a::T>`).
        syn::visit::visit_path(self, path);
    }

    fn visit_attribute(&mut self, _attribute: &'ast syn::Attribute) {}

    fn visit_vis_restricted(&mut self, _restricted: &'ast syn::VisRestricted) {}
}

/// Appends the leaves of `tree`. `prefix` holds the path segments so far and
/// `last` the offset of the most recent one, which a glob resolves through.
fn collect_leaves(
    tree: &syn::UseTree,
    prefix: &[String],
    last: Option<u32>,
    out: &mut Vec<UseLeaf>,
) {
    match tree {
        syn::UseTree::Path(path) => {
            let mut inner = prefix.to_vec();
            inner.push(path.ident.to_string());
            collect_leaves(
                &path.tree,
                &inner,
                Some(byte_offset(path.ident.span())),
                out,
            );
        }
        syn::UseTree::Name(name) => out.push(leaf(prefix, &name.ident, &name.ident)),
        syn::UseTree::Rename(rename) => out.push(leaf(prefix, &rename.ident, &rename.ident)),
        syn::UseTree::Glob(_) => {
            if let Some(offset) = last {
                let mut inner = prefix.to_vec();
                inner.push("*".to_owned());
                out.push(UseLeaf {
                    path: SmolStr::new(inner.join("::")),
                    offset,
                });
            }
        }
        syn::UseTree::Group(group) => {
            for nested in &group.items {
                collect_leaves(nested, prefix, last, out);
            }
        }
    }
}

/// The leaf `prefix::ident`, resolved at `at`'s offset.
fn leaf(prefix: &[String], ident: &syn::Ident, at: &syn::Ident) -> UseLeaf {
    let mut path = prefix.to_vec();
    path.push(ident.to_string());
    UseLeaf {
        path: SmolStr::new(path.join("::")),
        offset: byte_offset(at.span()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uses(text: &str) -> Vec<UseStatement> {
        syn::parse_file(text)
            .map(|file| collect_uses(&file).uses)
            .unwrap_or_default()
    }

    /// `(line, segments joined)` of every qualified path.
    fn paths(text: &str) -> Vec<(u32, String)> {
        syn::parse_file(text)
            .map(|file| collect_uses(&file).paths)
            .unwrap_or_default()
            .into_iter()
            .map(|path| {
                let names: Vec<&str> = path.segments.iter().map(|s| s.name.as_str()).collect();
                let lead = if path.absolute { "::" } else { "" };
                (path.line, format!("{lead}{}", names.join("::")))
            })
            .collect()
    }

    #[test]
    fn should_collect_expression_type_pattern_and_generic_paths_with_their_lines() {
        let found = paths(
            "struct S {\n    f: super::Config,\n}\nimpl crate::t::Tr for S {}\nfn g(v: Vec<crate::a::T>) {\n    child::run();\n    if let crate::e::E::V = x {}\n    let _ = ::ext::m::f();\n}\n",
        );

        assert_eq!(
            found,
            vec![
                (2, "super::Config".to_owned()),
                (4, "crate::t::Tr".to_owned()),
                (5, "crate::a::T".to_owned()),
                (6, "child::run".to_owned()),
                (7, "crate::e::E::V".to_owned()),
                (8, "::ext::m::f".to_owned()),
            ]
        );
    }

    #[test]
    fn should_skip_single_segment_self_attribute_visibility_and_macro_body_paths() {
        let found = paths(
            "#[derive(crate::d::D)]\npub(in crate::a) struct S;\nimpl S {\n    fn n() -> Self { Self::make() }\n}\nfn g() {\n    run();\n    println!(\"{}\", crate::a::f());\n}\n",
        );

        assert_eq!(found, Vec::<(u32, String)>::new());
    }

    #[test]
    fn should_skip_uses_and_paths_in_test_only_code() {
        let text = "#[cfg(test)]\nmod tests {\n    use super::x;\n    fn t() { crate::a::f(); }\n}\n#[test]\nfn case() { crate::b::f(); }\nstruct S;\nimpl S {\n    #[cfg(test)]\n    fn only() { crate::c::f(); }\n}\nfn live() { crate::d::f(); }\n";

        assert_eq!(uses(text), Vec::new());
        assert_eq!(paths(text), vec![(13, "crate::d::f".to_owned())]);
        assert_eq!(
            paths("#![cfg(test)]\nfn t() { crate::a::f(); }\n"),
            Vec::new()
        );
    }

    #[test]
    fn should_skip_paths_under_cfg_test_fields_statements_arms_and_impl_items() {
        let text = "struct S {\n    #[cfg(test)]\n    mock: crate::fake::M,\n    live: crate::real::R,\n}\nimpl S {\n    #[cfg(test)]\n    const C: crate::c::K = crate::c::K;\n}\nfn g(x: u8) {\n    #[cfg(test)]\n    let _m = crate::m::f();\n    match x {\n        #[cfg(test)]\n        0 => crate::arm::f(),\n        _ => {}\n    }\n}\n";

        assert_eq!(paths(text), vec![(4, "crate::real::R".to_owned())]);
    }

    /// `(line, leaf paths)` of every statement.
    fn summary(text: &str) -> Vec<(u32, Vec<String>)> {
        uses(text)
            .into_iter()
            .map(|statement| {
                let paths = statement
                    .leaves
                    .iter()
                    .map(|leaf| leaf.path.to_string())
                    .collect();
                (statement.line, paths)
            })
            .collect()
    }

    #[test]
    fn should_flatten_a_grouped_use_into_one_leaf_per_name() {
        let found = summary("\nuse crate::a::{b, c::D as E, self};\n");

        assert_eq!(
            found,
            vec![(
                2,
                vec![
                    "crate::a::b".to_owned(),
                    "crate::a::c::D".to_owned(),
                    "crate::a::self".to_owned()
                ]
            )]
        );
    }

    #[test]
    fn should_resolve_a_glob_through_the_segment_before_it() {
        let offsets: Vec<u32> = uses("use crate::a::*;\n")
            .iter()
            .flat_map(|statement| statement.leaves.iter().map(|leaf| leaf.offset))
            .collect();

        assert_eq!(
            summary("use crate::a::*;\n"),
            [(1, vec!["crate::a::*".to_owned()])]
        );
        assert_eq!(offsets, [11]);
    }

    #[test]
    fn should_collect_a_use_inside_an_inline_module_and_a_function_body() {
        let found = summary("mod m {\n    use super::x;\n}\nfn f() {\n    use crate::y;\n}\n");

        let lines: Vec<u32> = found.iter().map(|(line, _)| *line).collect();
        assert_eq!(lines, [2, 5]);
    }

    #[test]
    fn should_keep_a_leading_colon_of_an_absolute_path() {
        assert_eq!(
            summary("use ::std::fmt;\n"),
            [(1, vec!["::std::fmt".to_owned()])]
        );
    }
}
