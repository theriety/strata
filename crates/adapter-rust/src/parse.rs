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

mod declaration;
mod items;
mod metadata;
mod references;
mod tokens;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use strata_ir::{AdapterError, SourceFile};

use self::items::push_item;

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
