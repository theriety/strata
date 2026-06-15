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
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use strata_ir::{AdapterError, SourceFile};
use syn::spanned::Spanned as _;
use syn::visit::Visit;
use syn::{ImplItem, Item, ItemImpl, Type};

use crate::sloc::production_sloc;

/// The category of a top-level declaration: a value symbol or a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclKind {
    /// A value-level item (fn, const, static).
    Symbol,
    /// A type-level item (struct, enum, union, trait, type alias).
    Type,
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
    /// 0-based byte offset of the re-exported leaf identifier; resolution runs
    /// `goto_definition` here to find the original declaration.
    pub offset: u32,
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
        Item::Use(item_use) => {
            // A `pub use` re-exports the imported names through this module; a
            // plain `use` is a private import and contributes no export surface.
            if is_public(&item_use.vis) {
                collect_re_exports(&item_use.tree, re_exports);
            }
        }
        Item::Fn(item_fn) => {
            let cfg_test = cfg_test_ancestor || has_cfg_test(&item_fn.attrs);
            let mut decl = declaration(
                &item_fn.sig.ident.to_string(),
                DeclKind::Symbol,
                is_public(&item_fn.vis),
                cfg_test,
                item_fn.span(),
                contents,
                collect_references(item_fn),
            );
            decl.test_case = has_test_attr(&item_fn.attrs);
            out.push(decl);
        }
        Item::Struct(item_struct) => push_type(
            &item_struct.ident.to_string(),
            is_public(&item_struct.vis),
            cfg_test_ancestor || has_cfg_test(&item_struct.attrs),
            item_struct.span(),
            contents,
            collect_references(item_struct),
            out,
        ),
        Item::Enum(item_enum) => push_type(
            &item_enum.ident.to_string(),
            is_public(&item_enum.vis),
            cfg_test_ancestor || has_cfg_test(&item_enum.attrs),
            item_enum.span(),
            contents,
            collect_references(item_enum),
            out,
        ),
        Item::Union(item_union) => push_type(
            &item_union.ident.to_string(),
            is_public(&item_union.vis),
            cfg_test_ancestor || has_cfg_test(&item_union.attrs),
            item_union.span(),
            contents,
            collect_references(item_union),
            out,
        ),
        Item::Trait(item_trait) => push_type(
            &item_trait.ident.to_string(),
            is_public(&item_trait.vis),
            cfg_test_ancestor || has_cfg_test(&item_trait.attrs),
            item_trait.span(),
            contents,
            collect_references(item_trait),
            out,
        ),
        Item::Type(item_type) => push_type(
            &item_type.ident.to_string(),
            is_public(&item_type.vis),
            cfg_test_ancestor || has_cfg_test(&item_type.attrs),
            item_type.span(),
            contents,
            collect_references(item_type),
            out,
        ),
        Item::Const(item_const) => out.push(declaration(
            &item_const.ident.to_string(),
            DeclKind::Symbol,
            is_public(&item_const.vis),
            cfg_test_ancestor || has_cfg_test(&item_const.attrs),
            item_const.span(),
            contents,
            collect_references(item_const),
        )),
        Item::Static(item_static) => out.push(declaration(
            &item_static.ident.to_string(),
            DeclKind::Symbol,
            is_public(&item_static.vis),
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

/// Appends a type declaration to `out`.
fn push_type(
    name: &str,
    exported: bool,
    cfg_test: bool,
    span: Span,
    contents: &str,
    references: Vec<Reference>,
    out: &mut Vec<Declaration>,
) {
    out.push(declaration(
        name,
        DeclKind::Type,
        exported,
        cfg_test,
        span,
        contents,
        references,
    ));
}

/// Appends declarations for an `impl` block: its inherent and trait methods, and
/// — for a trait impl — a synthetic declaration on the self type that carries the
/// inheritance reference to the implemented trait.
fn push_impl(
    item_impl: &ItemImpl,
    contents: &str,
    cfg_test_ancestor: bool,
    out: &mut Vec<Declaration>,
) {
    let cfg_test = cfg_test_ancestor || has_cfg_test(&item_impl.attrs);
    let self_name = type_leaf_name(&item_impl.self_ty);

    // A trait impl contributes an inheritance reference from the self type to
    // the trait, recorded on a declaration named after the self type so the
    // edge originates at the implementing type.
    if let Some((_, trait_path, _)) = &item_impl.trait_
        && let (Some(self_name), Some(segment)) = (self_name.as_ref(), trait_path.segments.last())
    {
        let offset = byte_offset(segment.ident.span());
        out.push(Declaration {
            name: SmolStr::new(self_name),
            kind: DeclKind::Type,
            exported: false,
            cfg_test,
            test_case: false,
            byte_start: byte_start(item_impl.span()),
            byte_end: byte_end(item_impl.span()),
            sloc: 0,
            references: vec![Reference {
                kind: RefKind::TraitImpl,
                name: SmolStr::new(segment.ident.to_string()),
                offset,
                macro_expanded: false,
            }],
        });
    }

    for impl_item in &item_impl.items {
        if let ImplItem::Fn(method) = impl_item {
            let mut decl = declaration(
                &method.sig.ident.to_string(),
                DeclKind::Symbol,
                is_public(&method.vis),
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

/// Builds a [`Declaration`] from its parts, computing byte range and SLOC.
fn declaration(
    name: &str,
    kind: DeclKind,
    exported: bool,
    cfg_test: bool,
    span: Span,
    contents: &str,
    references: Vec<Reference>,
) -> Declaration {
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
        exported,
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
fn collect_re_exports(tree: &syn::UseTree, re_exports: &mut Vec<ReExport>) {
    match tree {
        syn::UseTree::Path(path) => collect_re_exports(&path.tree, re_exports),
        syn::UseTree::Name(name) => re_exports.push(ReExport {
            name: SmolStr::new(name.ident.to_string()),
            offset: byte_offset(name.ident.span()),
        }),
        syn::UseTree::Rename(rename) => re_exports.push(ReExport {
            name: SmolStr::new(rename.rename.to_string()),
            offset: byte_offset(rename.ident.span()),
        }),
        syn::UseTree::Group(group) => {
            for nested in &group.items {
                collect_re_exports(nested, re_exports);
            }
        }
        syn::UseTree::Glob(_) => {}
    }
}

/// Returns the trailing identifier of a type path (`a::b::Foo` -> `Foo`).
fn type_leaf_name(ty: &Type) -> Option<String> {
    match ty {
        Type::Path(path) => path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string()),
        _ => None,
    }
}

/// Returns `true` when `vis` is any form of `pub`.
fn is_public(vis: &syn::Visibility) -> bool {
    matches!(
        vis,
        syn::Visibility::Public(_) | syn::Visibility::Restricted(_)
    )
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
}

impl ReferenceCollector {
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
        if expr_path.path.segments.len() > 1
            && let Some(segment) = expr_path.path.segments.last()
        {
            self.record(RefKind::Call, &segment.ident);
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
            Some(SmolStr::new("R"))
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
    fn should_not_record_a_bare_local_path_as_a_reference() {
        let declarations = parse_text("fn build() -> i32 {\n    let total = 1;\n    total\n}\n");

        // `total` is a bare single-segment path expression (a local), so it must
        // not be recorded as a reference — only qualified paths are.
        let bare = declarations
            .iter()
            .flat_map(|decl| &decl.references)
            .any(|r| r.name == "total");
        assert!(!bare, "a bare local path is not recorded as a reference");
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
