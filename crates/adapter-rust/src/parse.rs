//! syn-based per-file parsing of Rust sources into serializable [`ParsedFile`]s.
//!
//! Parsing runs in parallel across files (one rayon task per file). A syntax
//! error is surfaced as [`AdapterError::Parse`] carrying the offending path and
//! a span-derived reason — files are never skipped silently.
//!
//! Each parsed file records its top-level declarations with the byte range each
//! spans, plus every reference of interest (use-path segments, call targets,
//! type positions, and trait-impl headers) with the byte offset at which it
//! occurs. [`crate::bind`] feeds those offsets to the `ra_ap` semantic database
//! to resolve them to definitions, then maps the resolved definition offset back
//! to the declaration whose byte range contains it.

use proc_macro2::Span;
use quote::ToTokens;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use strata_ir::{AdapterError, SourceFile};
use syn::spanned::Spanned as _;
use syn::visit::Visit;
use syn::{ImplItem, Item, ItemImpl};

use crate::sloc::production_sloc;

/// The category of a top-level declaration: a value symbol or a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclKind {
    /// A value-level item (fn, const, static).
    Symbol,
    /// A type-level item (struct, enum, union, trait, type alias).
    Type,
}

/// Source form of a Rust item's visibility.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibilityKind {
    /// Visibility from a payload produced before explicit metadata existed.
    #[default]
    Legacy,
    /// Private visibility inherited from the containing module.
    Inherited,
    /// Unrestricted public visibility.
    Public,
    /// Visibility restricted to the current crate.
    Crate,
    /// Visibility restricted to a module path.
    Restricted,
}

/// The category of a resolved reference, fixing the edge kind it produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RefKind {
    /// An identifier inside a `use` path (value-import).
    UsePath,
    /// A function or method call target (call).
    Call,
    /// An identifier in a type position (type-reference).
    TypeRef,
    /// The immediate qualifier of a multi-segment value path (`Type` in
    /// `Type::new()`). It becomes a type-reference only when it binds to a type
    /// declaration; a module qualifier (`render::report()`) never does.
    Qualifier,
    /// The trait named in an `impl Trait for T` header (inheritance).
    TraitImpl,
}

/// A reference whose target [`crate::bind`] resolves through the semantic
/// database at the recorded source byte offset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    /// The category of the reference (fixes the emitted edge kind).
    pub kind: RefKind,
    /// The referenced leaf identifier (e.g. `Foo` in `a::b::Foo`). Used as a
    /// name-based resolution fallback when `goto_definition` cannot pin the
    /// reference to an in-workspace declaration.
    pub name: SmolStr,
    /// 0-based byte offset of the reference within its source file. Resolution
    /// runs `goto_definition` at this offset.
    pub offset: u32,
    /// `true` when the reference textually originates inside a macro invocation,
    /// so its resolution carries reduced confidence (honest uncertainty).
    pub macro_expanded: bool,
}

/// A top-level declaration extracted from a file, with its byte range and SLOC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    /// Source-declared name of the symbol or type.
    pub name: SmolStr,
    /// Whether the declaration is a value symbol or a type.
    pub kind: DeclKind,
    /// `true` when the declaration is `pub` (or `pub(...)`), part of the export
    /// surface.
    pub exported: bool,
    /// Source form of the declaration's visibility.
    #[serde(default)]
    pub visibility_kind: VisibilityKind,
    /// Canonical `::`-joined path for restricted visibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_path: Option<SmolStr>,
    /// `true` when the declaration is gated behind `#[cfg(test)]`.
    pub cfg_test: bool,
    /// `true` when the declaration is itself a test case — a function carrying a
    /// `#[test]`-family attribute. Test cases are the entry points of a test;
    /// other `cfg(test)` items they reach are test *support*.
    pub test_case: bool,
    /// 0-based byte offset of the declaration's first byte.
    pub byte_start: u32,
    /// 0-based byte offset one past the declaration's last byte.
    pub byte_end: u32,
    /// Production SLOC attributed to this declaration.
    pub sloc: u32,
    /// References reachable from this declaration's body and header.
    pub references: Vec<Reference>,
}

/// A `pub use` re-export: the name surfaced in this file's export surface and the
/// byte offset of the imported leaf identifier, resolved by [`crate::bind`] to the
/// original declaration so a re-export edge can be emitted as-is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReExport {
    /// The name the re-export binds in this file (the imported leaf, or its
    /// `as`-rename when present).
    pub name: SmolStr,
    /// Source form of the re-export's visibility.
    #[serde(default = "public_visibility_kind")]
    pub visibility_kind: VisibilityKind,
    /// Canonical `::`-joined path for restricted visibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub visibility_path: Option<SmolStr>,
    /// 0-based byte offset of the re-exported leaf identifier; resolution runs
    /// `goto_definition` here to find the original declaration.
    pub offset: u32,
}

/// Supplies the compatibility default for re-exports from legacy payloads.
fn public_visibility_kind() -> VisibilityKind {
    VisibilityKind::Public
}

/// A serializable summary of one parsed Rust source file.
///
/// This is the payload the adapter threads through [`strata_ir::ParseTree`]:
/// [`parse`] produces it, [`crate::bind::bind`] consumes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedFile {
    /// Repository-relative path of the source file.
    pub path: SmolStr,
    /// Top-level declarations in source order.
    pub declarations: Vec<Declaration>,
    /// `pub use` re-exports surfaced at this file's module root, emitted as-is.
    pub re_exports: Vec<ReExport>,
}

/// Parses Rust sources into [`ParsedFile`]s, one rayon task per file.
///
/// # Errors
///
/// Returns [`AdapterError::Parse`] for the first file (in input order) that
/// contains a syntax error; the reason embeds the line and column of the failure.
pub fn parse(files: &[SourceFile]) -> Result<Vec<ParsedFile>, AdapterError> {
    files.par_iter().map(parse_one).collect()
}

/// Parses a single source file into a [`ParsedFile`].
fn parse_one(file: &SourceFile) -> Result<ParsedFile, AdapterError> {
    let ast = syn::parse_file(&file.contents).map_err(|error| AdapterError::Parse {
        path: file.path.clone(),
        reason: format_parse_error(&error),
    })?;

    let mut declarations = Vec::new();
    let mut re_exports = Vec::new();
    for item in &ast.items {
        push_item(
            item,
            &file.contents,
            false,
            &mut declarations,
            &mut re_exports,
        );
    }

    Ok(ParsedFile {
        path: file.path.clone(),
        declarations,
        re_exports,
    })
}

/// Renders a syn error as a `line L, column C: message` string.
fn format_parse_error(error: &syn::Error) -> String {
    let start = error.span().start();
    format!("line {}, column {}: {}", start.line, start.column, error)
}

/// Appends the declaration(s) introduced by one top-level item to `out`.
///
/// `cfg_test_ancestor` propagates `#[cfg(test)]` down from an enclosing module
/// so items nested in a test module inherit the test gate.
fn push_item(
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

/// Records the names a `pub use` re-exports through this module; a plain `use`
/// is a private import and contributes no export surface.
fn push_re_exports(item_use: &syn::ItemUse, re_exports: &mut Vec<ReExport>) {
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

/// Renders a trait impl's header without its own generic parameter list:
/// `impl<T> From<T> for Wrapper<T>` becomes `impl From<T> for Wrapper<T>`.
fn trait_impl_header(item_impl: &ItemImpl) -> String {
    let (negative, trait_path) = item_impl
        .trait_
        .as_ref()
        .map_or((false, None), |(bang, path, _)| {
            (bang.is_some(), Some(path))
        });
    format!(
        "impl {}{} for {}",
        if negative { "!" } else { "" },
        trait_path.map(compact_tokens).unwrap_or_default(),
        compact_tokens(&item_impl.self_ty)
    )
}

/// Prints `node` as source-like text: tokens are joined without spacing except
/// between two word-like tokens, after `,` and `;`, and around `+` and `->`
/// (`From < Vec < u8 > >` becomes `From<Vec<u8>>`, `* const T` becomes
/// `*const T`).
fn compact_tokens(node: &impl ToTokens) -> String {
    let mut text = String::new();
    write_tokens(node.to_token_stream(), &mut text);
    text
}

/// Appends `stream` to `text`, inserting a space only where two tokens would
/// otherwise fuse or where a separator reads better spaced.
fn write_tokens(stream: proc_macro2::TokenStream, text: &mut String) {
    use proc_macro2::{Delimiter, Spacing, TokenTree};

    let mut previous_is_word = false;
    let mut previous_is_lifetime = false;
    let mut after_tick = false;
    let mut pending_space = false;
    let mut tokens = stream.into_iter().peekable();
    while let Some(token) = tokens.next() {
        let is_word = matches!(token, TokenTree::Ident(_) | TokenTree::Literal(_));
        let lifetime_tick = matches!(&token, TokenTree::Punct(p) if p.as_char() == '\'');
        // `*mut [u8; 4]`: a bracket group after a qualifier is a type, not an index.
        let slice_type = matches!(&token, TokenTree::Group(g) if g.delimiter() == Delimiter::Bracket)
            && matches!(
                text.rsplit([' ', '*', '&']).next(),
                Some("mut" | "const" | "dyn")
            );
        // `for<'a> Fn(&'a u8)` and `&'a [u8]`: a closed higher-ranked binder
        // or a lifetime name must not fuse with the word or group after it.
        let after_binder =
            matches!(token, TokenTree::Ident(_)) && !after_tick && text.ends_with('>');
        let after_lifetime = previous_is_lifetime
            && matches!(&token, TokenTree::Group(g) if g.delimiter() != Delimiter::None);
        if (pending_space
            || slice_type
            || after_binder
            || after_lifetime
            || ((is_word || lifetime_tick) && previous_is_word))
            && !text.is_empty()
        {
            text.push(' ');
        }
        pending_space = false;
        previous_is_lifetime = after_tick && is_word;
        after_tick = lifetime_tick;
        previous_is_word = is_word;
        match token {
            TokenTree::Group(group) => {
                let (open, close) = match group.delimiter() {
                    Delimiter::Parenthesis => ("(", ")"),
                    Delimiter::Bracket => ("[", "]"),
                    Delimiter::Brace => ("{ ", " }"),
                    Delimiter::None => ("", ""),
                };
                if group.stream().is_empty() {
                    text.push_str(open.trim_end());
                    text.push_str(close.trim_start());
                } else {
                    text.push_str(open);
                    write_tokens(group.stream(), text);
                    text.push_str(close);
                }
            }
            TokenTree::Punct(punct) => {
                let ch = punct.as_char();
                let arrow = ch == '-'
                    && punct.spacing() == Spacing::Joint
                    && matches!(tokens.peek(), Some(TokenTree::Punct(next)) if next.as_char() == '>');
                if arrow {
                    tokens.next();
                    text.push_str(" ->");
                    pending_space = true;
                } else if ch == '+' {
                    text.push_str(" +");
                    pending_space = true;
                } else if ch == '='
                    && punct.spacing() == Spacing::Alone
                    && !text.ends_with(['<', '>', '=', '!'])
                {
                    text.push_str(" =");
                    pending_space = true;
                } else {
                    text.push(ch);
                    pending_space = matches!(ch, ',' | ';');
                }
            }
            TokenTree::Ident(ident) => text.push_str(&ident.to_string()),
            TokenTree::Literal(literal) => text.push_str(&literal.to_string()),
        }
    }
}

/// Builds a [`Declaration`] from its parts, computing byte range and SLOC.
fn declaration(
    name: &str,
    kind: DeclKind,
    source_visibility: &syn::Visibility,
    cfg_test: bool,
    span: Span,
    contents: &str,
    references: Vec<Reference>,
) -> Declaration {
    let (visibility_kind, visibility_path) = visibility(source_visibility);
    let byte_start = byte_start(span);
    let byte_end = byte_end(span);
    let sloc = if cfg_test {
        // `cfg(test)` regions are excluded from production SLOC.
        0
    } else {
        slice_sloc(byte_start, byte_end, contents)
    };
    Declaration {
        name: SmolStr::new(name),
        kind,
        exported: is_public(source_visibility),
        visibility_kind,
        visibility_path,
        cfg_test,
        test_case: false,
        byte_start,
        byte_end,
        sloc,
        references,
    }
}

/// Returns `true` when any attribute is a `#[test]`-family attribute (`#[test]`,
/// `#[tokio::test]`, `#[test_case]`, …): the marker of a test-case entry point.
fn has_test_attr(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "test")
    })
}

/// Computes production SLOC for the byte slice `[start, end)` of `contents`.
fn slice_sloc(start: u32, end: u32, contents: &str) -> u32 {
    contents
        .get(start as usize..end as usize)
        .map_or(0, production_sloc)
}

/// 0-based byte offset of a span's start, saturating into `u32`.
fn byte_start(span: Span) -> u32 {
    byte_offset(span)
}

/// 0-based byte offset of a span's start, saturating into `u32`.
fn byte_offset(span: Span) -> u32 {
    u32::try_from(span.byte_range().start).unwrap_or(u32::MAX)
}

/// 0-based byte offset one past a span's end, saturating into `u32`.
fn byte_end(span: Span) -> u32 {
    u32::try_from(span.byte_range().end).unwrap_or(u32::MAX)
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
fn is_public(vis: &syn::Visibility) -> bool {
    matches!(
        vis,
        syn::Visibility::Public(_) | syn::Visibility::Restricted(_)
    )
}

/// Returns explicit visibility metadata without deriving it from `exported`.
fn visibility(vis: &syn::Visibility) -> (VisibilityKind, Option<SmolStr>) {
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
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
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

/// Visits a declaration subtree, collecting every reference of interest with its
/// source byte offset.
fn collect_references<'ast, V>(node: &'ast V) -> Vec<Reference>
where
    ReferenceCollector: Visit<'ast>,
    V: VisitNode,
{
    let mut collector = ReferenceCollector::default();
    node.accept(&mut collector);
    collector.references
}

/// A node a [`ReferenceCollector`] can be driven over.
trait VisitNode {
    /// Drives `collector` over `self`.
    fn accept<'ast>(&'ast self, collector: &mut ReferenceCollector)
    where
        ReferenceCollector: Visit<'ast>;
}

/// Generates the [`VisitNode`] impl for each visited syn item type.
macro_rules! impl_visit_node {
    ($($ty:ty => $method:ident),+ $(,)?) => {
        $(
            impl VisitNode for $ty {
                fn accept<'ast>(&'ast self, collector: &mut ReferenceCollector)
                where
                    ReferenceCollector: Visit<'ast>,
                {
                    collector.$method(self);
                }
            }
        )+
    };
}

impl_visit_node! {
    syn::ItemFn => visit_item_fn,
    syn::ItemStruct => visit_item_struct,
    syn::ItemEnum => visit_item_enum,
    syn::ItemUnion => visit_item_union,
    syn::ItemTrait => visit_item_trait,
    syn::ItemType => visit_item_type,
    syn::ItemConst => visit_item_const,
    syn::ItemStatic => visit_item_static,
    syn::ImplItemFn => visit_impl_item_fn,
    syn::ImplItem => visit_impl_item,
}

/// A syn visitor that gathers references (use paths, calls, type positions) with
/// the byte offset at which each occurs, tracking whether the cursor is inside a
/// macro invocation so resolved edges can be marked lower-confidence.
#[derive(Default)]
struct ReferenceCollector {
    /// The references gathered so far, in source order.
    references: Vec<Reference>,
    /// Macro-invocation nesting depth; non-zero marks macro-expanded context.
    macro_depth: u32,
    /// Names bound in the lexical scopes enclosing the cursor (parameters, let,
    /// closure parameters, patterns); bare uses of them are locals, not items.
    /// Blocks, closures, match arms, and `if`/`while`/`for` truncate it on exit.
    bound: Vec<String>,
}

impl ReferenceCollector {
    /// Runs `visit`, then forgets every name it bound, so a binding does not
    /// outlive the block, closure, arm, or loop that introduced it.
    fn scoped(&mut self, visit: impl FnOnce(&mut Self)) {
        let depth = self.bound.len();
        visit(self);
        self.bound.truncate(depth);
    }

    /// Records a reference of `kind` to `ident`, stamping the current macro
    /// context onto it. The identifier's text is retained so binding can fall
    /// back to name-based resolution when `goto_definition` comes up empty.
    fn record(&mut self, kind: RefKind, ident: &syn::Ident) {
        self.references.push(Reference {
            kind,
            name: SmolStr::new(ident.to_string()),
            offset: byte_offset(ident.span()),
            macro_expanded: self.macro_depth > 0,
        });
    }
}

impl<'ast> Visit<'ast> for ReferenceCollector {
    fn visit_pat_ident(&mut self, pat: &'ast syn::PatIdent) {
        self.bound.push(pat.ident.to_string());
        syn::visit::visit_pat_ident(self, pat);
    }

    fn visit_local(&mut self, local: &'ast syn::Local) {
        // The initializer (and let-else block) is evaluated before the pattern
        // binds, so `let helper = helper;` still reads the outer `helper`.
        for attr in &local.attrs {
            self.visit_attribute(attr);
        }
        if let Some(init) = &local.init {
            self.visit_expr(&init.expr);
            if let Some((_, diverge)) = &init.diverge {
                self.visit_expr(diverge);
            }
        }
        self.visit_pat(&local.pat);
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        self.scoped(|this| syn::visit::visit_block(this, block));
    }

    fn visit_expr_closure(&mut self, closure: &'ast syn::ExprClosure) {
        self.scoped(|this| syn::visit::visit_expr_closure(this, closure));
    }

    fn visit_arm(&mut self, arm: &'ast syn::Arm) {
        self.scoped(|this| syn::visit::visit_arm(this, arm));
    }

    fn visit_expr_if(&mut self, expr: &'ast syn::ExprIf) {
        self.scoped(|this| syn::visit::visit_expr_if(this, expr));
    }

    fn visit_expr_while(&mut self, expr: &'ast syn::ExprWhile) {
        self.scoped(|this| syn::visit::visit_expr_while(this, expr));
    }

    fn visit_expr_for_loop(&mut self, expr: &'ast syn::ExprForLoop) {
        self.scoped(|this| syn::visit::visit_expr_for_loop(this, expr));
    }

    fn visit_use_path(&mut self, use_path: &'ast syn::UsePath) {
        self.record(RefKind::UsePath, &use_path.ident);
        syn::visit::visit_use_path(self, use_path);
    }

    fn visit_use_name(&mut self, use_name: &'ast syn::UseName) {
        self.record(RefKind::UsePath, &use_name.ident);
        syn::visit::visit_use_name(self, use_name);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref()
            && let Some(segment) = path.path.segments.last()
        {
            self.record(RefKind::Call, &segment.ident);
        }
        syn::visit::visit_expr_call(self, call);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        self.record(RefKind::Call, &call.method);
        syn::visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_struct(&mut self, expr_struct: &'ast syn::ExprStruct) {
        // A struct literal `Foo { .. }` depends on the constructed type.
        if let Some(segment) = expr_struct.path.segments.last() {
            self.record(RefKind::TypeRef, &segment.ident);
        }
        syn::visit::visit_expr_struct(self, expr_struct);
    }

    fn visit_expr_path(&mut self, expr_path: &'ast syn::ExprPath) {
        // Qualified value paths (`Type::assoc`, `module::ITEM`, `Enum::Variant`)
        // name a cross-item dependency; bare single-segment paths are usually
        // locals, so only multi-segment paths are recorded. Direct call callees
        // are already handled by `visit_expr_call`; duplicates collapse in bind.
        // The qualifier is a dependency too when it names a type: `Type::new()`
        // resolves its leaf to the associated function, so the qualifier is
        // recorded separately and bind keeps it only if it lands on a type.
        let segments = &expr_path.path.segments;
        if segments.len() == 1
            && expr_path.qself.is_none()
            && expr_path.path.leading_colon.is_none()
            && let Some(segment) = segments.first()
        {
            // A bare name is a function used as a value (`.map_or(0, f)`) unless
            // the declaration binds it (let, parameter, closure, pattern) or it
            // is a path keyword. Bind resolves it only through the semantic
            // database, never by name.
            let name = segment.ident.to_string();
            if !matches!(name.as_str(), "self" | "Self" | "crate" | "super")
                && !self.bound.contains(&name)
            {
                self.record(RefKind::Call, &segment.ident);
            }
        }
        if segments.len() > 1
            && let Some(segment) = segments.last()
        {
            self.record(RefKind::Call, &segment.ident);
            if let Some(qualifier) = segments.iter().nth_back(1) {
                self.record(RefKind::Qualifier, &qualifier.ident);
            }
        }
        syn::visit::visit_expr_path(self, expr_path);
    }

    fn visit_type_path(&mut self, type_path: &'ast syn::TypePath) {
        if let Some(segment) = type_path.path.segments.last() {
            self.record(RefKind::TypeRef, &segment.ident);
        }
        syn::visit::visit_type_path(self, type_path);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        // A macro's token stream is not part of the typed AST, so the default
        // visit never descends into it. Best-effort: re-parse the tokens as a
        // comma-separated expression list (the shape of `println!`, `vec!`,
        // `assert!`, …) and visit any recovered expressions with the macro flag
        // raised, so references that originate inside a macro resolve at reduced
        // confidence rather than being silently dropped.
        self.macro_depth += 1;
        if let Ok(args) = mac.parse_body_with(
            syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated,
        ) {
            for expr in &args {
                self.visit_expr(expr);
            }
        }
        self.macro_depth = self.macro_depth.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses `text`, returning the declarations; an unparseable fixture yields
    /// an empty list so assertions fail loudly without panicking the test.
    fn parse_text(text: &str) -> Vec<Declaration> {
        parse_one(&SourceFile {
            path: SmolStr::new("src/lib.rs"),
            contents: text.to_string(),
        })
        .map(|file| file.declarations)
        .unwrap_or_default()
    }

    /// Parses `text`, returning the recorded re-exports (empty on parse failure).
    fn parse_re_exports(text: &str) -> Vec<ReExport> {
        parse_one(&SourceFile {
            path: SmolStr::new("src/lib.rs"),
            contents: text.to_string(),
        })
        .map(|file| file.re_exports)
        .unwrap_or_default()
    }

    /// The first declaration of a parsed fixture, or a default placeholder whose
    /// fields fail the asserting test loudly when parsing produced nothing.
    fn first(declarations: &[Declaration]) -> &Declaration {
        declarations.first().unwrap_or(&MISSING)
    }

    /// Placeholder returned when a fixture yields no declarations; its sentinel
    /// fields never satisfy a real assertion.
    static MISSING: Declaration = Declaration {
        name: SmolStr::new_inline("<<missing>>"),
        kind: DeclKind::Symbol,
        exported: false,
        visibility_kind: VisibilityKind::Inherited,
        visibility_path: None,
        cfg_test: false,
        test_case: false,
        byte_start: u32::MAX,
        byte_end: u32::MAX,
        sloc: u32::MAX,
        references: Vec::new(),
    };

    #[test]
    fn should_extract_a_public_function_as_a_symbol() {
        let declarations = parse_text("pub fn run() {}\n");

        let decl = first(&declarations);
        assert_eq!(decl.name, SmolStr::new("run"));
        assert_eq!(decl.kind, DeclKind::Symbol);
        assert!(decl.exported);
        assert!(!decl.cfg_test);
    }

    #[test]
    fn should_classify_a_struct_as_a_type() {
        let declarations = parse_text("pub struct Report { label: String }\n");

        assert_eq!(first(&declarations).kind, DeclKind::Type);
    }

    #[test]
    fn should_mark_cfg_test_items_and_zero_their_sloc() {
        let declarations = parse_text("#[cfg(test)]\nfn helper() {\n    let x = 1;\n}\n");

        let decl = first(&declarations);
        assert!(decl.cfg_test);
        assert_eq!(decl.sloc, 0);
    }

    #[test]
    fn should_propagate_cfg_test_into_nested_modules() {
        let declarations = parse_text("#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n");

        assert!(first(&declarations).cfg_test);
    }

    #[test]
    fn should_record_a_trait_impl_inheritance_reference() {
        let declarations = parse_text("struct R;\nimpl Summarize for R {\n    fn s(&self) {}\n}\n");

        let trait_decl = declarations
            .iter()
            .find(|decl| decl.references.iter().any(|r| r.kind == RefKind::TraitImpl));
        assert_eq!(
            trait_decl.map(|decl| decl.name.clone()),
            Some(SmolStr::new("impl Summarize for R"))
        );
    }

    #[test]
    fn should_fold_trait_impl_items_into_one_block_named_by_its_header() {
        let declarations = parse_text(
            "struct R;\nimpl Summarize for R {\n    type Out = Unit;\n    fn s(&self) -> u32 {\n        helper()\n    }\n}\n",
        );

        let names = declarations
            .iter()
            .map(|decl| decl.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["R", "impl Summarize for R"],
            "a trait impl's items are not their own declarations"
        );
        let block = declarations.get(1);
        assert_eq!(block.map(|block| block.kind), Some(DeclKind::Type));
        assert!(
            block.is_some_and(|block| block.sloc > 0),
            "the block carries its items' sloc"
        );
        let carries = |kind: RefKind, name: &str| {
            block.is_some_and(|block| {
                block
                    .references
                    .iter()
                    .any(|r| r.kind == kind && r.name == name)
            })
        };
        assert!(
            carries(RefKind::Call, "helper"),
            "the block carries its methods' references"
        );
        assert!(
            carries(RefKind::TypeRef, "Unit"),
            "the block carries its associated types' references"
        );
    }

    #[test]
    fn should_name_a_trait_impl_without_the_impls_own_generic_parameters() {
        let declarations = parse_text(
            "impl<T: Clone> From<Vec<T>> for Wrapper<T> {\n    fn from(v: Vec<T>) -> Self {\n        Wrapper(v)\n    }\n}\nimpl std::fmt::Display for &'static Report {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        Ok(())\n    }\n}\n",
        );

        let names = declarations
            .iter()
            .map(|decl| decl.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "impl From<Vec<T>> for Wrapper<T>",
                "impl std::fmt::Display for &'static Report",
            ]
        );
    }

    #[test]
    fn should_declare_a_trait_impl_block_with_the_type_kind() {
        // ADR-19: the block is a `Type`; the downstream type-only pricing it
        // implies is pinned by the engine, not here.
        let declarations = parse_text(
            "impl Display for Report {\n    fn fmt(&self, f: &mut Formatter) -> Result {\n        Ok(())\n    }\n}\n",
        );

        assert_eq!(
            declarations
                .iter()
                .map(|decl| decl.kind)
                .collect::<Vec<_>>(),
            [DeclKind::Type]
        );
    }

    #[test]
    fn should_render_fn_pointer_and_const_generic_self_types_compactly() {
        let declarations = parse_text(
            "impl Tr for Box<dyn Fn(u8) -> u8> {}\nimpl Tr for *const Item {}\nimpl Tr for *mut [u8; 4] {}\nimpl Tr for Buf<{ N > 1 }> {}\nimpl Tr for Box<dyn A + Send + 'a> {}\n",
        );

        let names = declarations
            .iter()
            .map(|decl| decl.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "impl Tr for Box<dyn Fn(u8) -> u8>",
                "impl Tr for *const Item",
                "impl Tr for *mut [u8; 4]",
                "impl Tr for Buf<{ N>1 }>",
                "impl Tr for Box<dyn A + Send + 'a>",
            ]
        );
    }

    #[test]
    fn should_render_binders_lifetimes_bindings_and_empty_braces_compactly() {
        let declarations = parse_text(
            "impl Tr for Box<dyn for<'a> Fn(&'a u8)> {}\nimpl Tr for &'a [u8] {}\nimpl Tr for &'a (A, B) {}\nimpl Tr for Box<dyn Iterator<Item = u8>> {}\nimpl Tr for Buf<{}> {}\nimpl Tr for Buf<{ N >= 1 }> {}\n",
        );

        let names = declarations
            .iter()
            .map(|decl| decl.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "impl Tr for Box<dyn for<'a> Fn(&'a u8)>",
                "impl Tr for &'a [u8]",
                "impl Tr for &'a (A, B)",
                "impl Tr for Box<dyn Iterator<Item = u8>>",
                "impl Tr for Buf<{}>",
                "impl Tr for Buf<{ N>=1 }>",
            ]
        );
    }

    #[test]
    fn should_disambiguate_cfg_variant_trait_impls_by_source_order() {
        let declarations = parse_text(
            "#[cfg(unix)]\nimpl Display for Report {\n    fn fmt(&self) {}\n}\n#[cfg(windows)]\nimpl Display for Report {\n    fn fmt(&self) {}\n}\nimpl Debug for Report {\n    fn fmt(&self) {}\n}\n",
        );

        let names = declarations
            .iter()
            .map(|decl| decl.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "impl Display for Report",
                "impl Display for Report (2)",
                "impl Debug for Report",
            ]
        );
    }

    #[test]
    fn should_keep_inherent_methods_as_separate_symbols() {
        let declarations = parse_text("struct R;\nimpl R {\n    fn s(&self) {}\n}\n");

        assert!(
            declarations
                .iter()
                .any(|decl| decl.name == "s" && decl.kind == DeclKind::Symbol)
        );
    }

    #[test]
    fn should_record_a_struct_literal_as_a_type_reference() {
        let declarations = parse_text("fn build() -> () {\n    let _ = Report { label: 1 };\n}\n");

        let type_ref = declarations
            .iter()
            .flat_map(|decl| &decl.references)
            .find(|r| r.kind == RefKind::TypeRef && r.name == "Report");
        assert!(
            type_ref.is_some(),
            "a struct-literal type reference is recorded"
        );
    }

    #[test]
    fn should_record_a_qualified_value_path_as_a_call_reference() {
        let declarations = parse_text("fn build() -> () {\n    let _ = Report::new();\n}\n");

        let value_ref = declarations
            .iter()
            .flat_map(|decl| &decl.references)
            .find(|r| r.kind == RefKind::Call && r.name == "new");
        assert!(
            value_ref.is_some(),
            "a qualified value-path reference is recorded"
        );
    }

    #[test]
    fn should_record_the_qualifier_of_an_associated_call_as_a_qualifier_reference() {
        let declarations =
            parse_text("fn build() -> () {\n    let _ = adapters::Report::new();\n}\n");

        let references = declarations
            .iter()
            .flat_map(|decl| &decl.references)
            .collect::<Vec<_>>();
        assert!(
            references
                .iter()
                .any(|r| r.kind == RefKind::Qualifier && r.name == "Report"),
            "`Type::new()` depends on `Type`, not only on its constructor; got {references:?}"
        );
        assert!(
            !references.iter().any(|r| r.name == "adapters"),
            "only the immediate qualifier is recorded; got {references:?}"
        );
    }

    #[test]
    fn should_record_a_bare_fn_used_as_a_value_as_a_call() {
        let declarations =
            parse_text("fn build(x: Option<u32>) -> Option<u32> {\n    x.map(double)\n}\n");

        let value = declarations
            .iter()
            .flat_map(|decl| &decl.references)
            .find(|r| r.name == "double");
        assert_eq!(value.map(|r| r.kind), Some(RefKind::Call));
    }

    /// Whether any reference in `text` carries `name`.
    fn records_name(text: &str, name: &str) -> bool {
        parse_text(text)
            .iter()
            .flat_map(|decl| &decl.references)
            .any(|r| r.name == name)
    }

    #[test]
    fn should_not_record_a_let_bound_name_as_a_reference() {
        assert!(!records_name(
            "fn build() -> i32 {\n    let total = 1;\n    total\n}\n",
            "total"
        ));
    }

    #[test]
    fn should_record_the_initializer_use_of_a_name_a_let_then_shadows() {
        for text in [
            "fn helper() {}\nfn build() {\n    let helper = helper;\n    let _ = helper;\n}\n",
            "fn helper() {}\nfn build() {\n    let helper: fn() = helper;\n    let _ = helper;\n}\n",
        ] {
            let hits = parse_text(text)
                .iter()
                .flat_map(|decl| &decl.references)
                .filter(|r| r.name == "helper" && r.kind == RefKind::Call)
                .count();
            assert_eq!(hits, 1, "only the initializer is the outer fn: {text}");
        }
    }

    #[test]
    fn should_keep_a_later_use_of_a_shadowing_let_local() {
        assert!(!records_name(
            "fn build() -> i32 {\n    let helper = 1;\n    helper\n}\n",
            "helper"
        ));
    }

    #[test]
    fn should_not_record_a_param_bound_name_as_a_reference() {
        assert!(!records_name(
            "fn build(total: i32) -> i32 {\n    total\n}\n",
            "total"
        ));
    }

    #[test]
    fn should_not_record_a_closure_param_as_a_reference() {
        assert!(!records_name(
            "fn build(v: Vec<i32>) -> Vec<i32> {\n    v.into_iter().map(|item| item).collect()\n}\n",
            "item"
        ));
    }

    #[test]
    fn should_not_record_a_pattern_bound_name_as_a_reference() {
        assert!(!records_name(
            "fn build(o: Option<i32>) -> i32 {\n    match o {\n        Some(inner) => inner,\n        None => 0,\n    }\n}\n",
            "inner"
        ));
    }

    #[test]
    fn should_skip_bare_self_and_path_root_keywords() {
        for keyword in ["self", "Self", "crate", "super"] {
            let text = format!(
                "struct S;\nimpl S {{\n    fn f(&self) {{\n        let _x = {keyword};\n        let _y = sentinel;\n    }}\n}}\n"
            );
            let references: Vec<_> = parse_text(&text)
                .into_iter()
                .flat_map(|decl| decl.references)
                .collect();

            assert!(
                references.iter().any(|r| r.name == "sentinel"),
                "{keyword}: the fixture parsed and records a real bare value"
            );
            assert!(
                !references
                    .iter()
                    .any(|r| r.name == keyword && r.kind == RefKind::Call),
                "{keyword} is not a value reference: {references:?}"
            );
        }
    }

    /// How many references named `name` the first declaration of `text` records.
    fn count_named(text: &str, name: &str) -> usize {
        parse_text(text)
            .iter()
            .flat_map(|decl| &decl.references)
            .filter(|r| r.name == name)
            .count()
    }

    #[test]
    fn should_scope_a_closure_param_to_its_closure() {
        let text = "fn f(v: Vec<u32>) {\n    let _a = v.iter().map(|double| double);\n    let _b = v.iter().map(double);\n}\n";

        assert_eq!(
            count_named(text, "double"),
            1,
            "only the use after the closure"
        );
    }

    #[test]
    fn should_scope_a_match_arm_binding_to_its_arm() {
        let text = "fn f(o: Option<u32>) {\n    let _a = match o { Some(double) => double, None => 0 };\n    let _b = o.map(double);\n}\n";

        assert_eq!(
            count_named(text, "double"),
            1,
            "only the use after the match"
        );
    }

    #[test]
    fn should_scope_a_block_let_to_its_block() {
        let text = "fn f(o: Option<u32>) {\n    {\n        let double = 1;\n        let _a = double;\n    }\n    let _b = o.map(double);\n}\n";

        assert_eq!(
            count_named(text, "double"),
            1,
            "only the use after the block"
        );
    }

    #[test]
    fn should_carry_the_leaf_name_on_a_recorded_reference() {
        let declarations = parse_text("use other::Thing;\nfn f(x: Thing) {}\n");

        let named = declarations
            .iter()
            .flat_map(|decl| &decl.references)
            .find(|r| r.kind == RefKind::TypeRef && r.name == "Thing");
        assert!(named.is_some(), "the referenced leaf name is retained");
    }

    #[test]
    fn should_flag_references_inside_a_macro_as_macro_expanded() {
        let declarations = parse_text("fn f() {\n    println!(\"{}\", run());\n}\n");

        let macro_ref = declarations
            .iter()
            .flat_map(|decl| &decl.references)
            .find(|r| r.macro_expanded);
        assert!(
            macro_ref.is_some(),
            "a macro-expanded reference is recorded"
        );
    }

    #[test]
    fn should_count_production_sloc_for_a_function_body() {
        let declarations = parse_text("pub fn run() {\n    let x = 1;\n    x\n}\n");

        assert_eq!(first(&declarations).sloc, 4);
    }

    #[test]
    fn should_record_a_pub_use_as_a_re_export() {
        let re_exports = parse_re_exports("pub use other::Thing;\n");

        assert_eq!(re_exports.len(), 1);
        assert_eq!(
            re_exports.first().map(|re| re.name.clone()),
            Some(SmolStr::new("Thing"))
        );
    }

    #[test]
    fn should_record_each_name_of_a_grouped_pub_use() {
        let re_exports = parse_re_exports("pub use other::{A, B};\n");

        let names: Vec<SmolStr> = re_exports.iter().map(|re| re.name.clone()).collect();
        assert_eq!(names, vec![SmolStr::new("A"), SmolStr::new("B")]);
    }

    #[test]
    fn should_bind_the_renamed_name_of_a_pub_use_rename() {
        let re_exports = parse_re_exports("pub use other::A as Renamed;\n");

        assert_eq!(
            re_exports.first().map(|re| re.name.clone()),
            Some(SmolStr::new("Renamed"))
        );
    }

    #[test]
    fn should_record_a_crate_root_alias_as_a_re_export() {
        // `pub use strata_ir as ir;` names a crate root, which is never a node;
        // it is still a re-export declaration (ADR-20).
        let re_exports = parse_re_exports("pub use strata_ir as ir;\n");

        assert_eq!(
            re_exports.first().map(|re| re.name.clone()),
            Some(SmolStr::new("ir"))
        );
    }

    #[test]
    fn should_not_record_a_private_use_as_a_re_export() {
        let re_exports = parse_re_exports("use other::Thing;\n");

        assert!(re_exports.is_empty());
    }

    #[test]
    fn should_flag_a_test_attributed_function_as_a_test_case() {
        let declarations =
            parse_text("#[cfg(test)]\nmod tests {\n    #[test]\n    fn it_works() {}\n}\n");

        let it_works = declarations.iter().find(|decl| decl.name == "it_works");
        assert_eq!(it_works.map(|decl| decl.test_case), Some(true));
    }

    #[test]
    fn should_not_flag_a_cfg_test_helper_as_a_test_case() {
        let declarations = parse_text("#[cfg(test)]\nmod tests {\n    fn helper() {}\n}\n");

        let helper = declarations.iter().find(|decl| decl.name == "helper");
        assert_eq!(helper.map(|decl| decl.test_case), Some(false));
        assert_eq!(helper.map(|decl| decl.cfg_test), Some(true));
    }
}
