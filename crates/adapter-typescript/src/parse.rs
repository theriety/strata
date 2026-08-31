//! swc parsing of `.ts` / `.tsx` sources into serializable [`ParsedModule`]s.
//!
//! Parsing runs in parallel across files (one rayon task per file). A syntax
//! error is surfaced as [`AdapterError::Parse`] carrying the offending path and
//! a span-derived reason — files are never skipped silently.

use std::collections::BTreeSet;

use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize};
use smol_str::SmolStr;
use strata_ir::{AdapterError, SourceFile};
use swc_common::{BytePos, FileName, SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::{
    CallExpr, Callee, ClassDecl, ClassMember, Decl, DefaultDecl, Expr, FnDecl, Lit, Module,
    ModuleDecl, ModuleItem, NewExpr, Pat, PropName, TsEnumDecl, TsExprWithTypeArgs,
    TsInterfaceDecl, TsTypeAliasDecl, VarDecl,
};
use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
use swc_ecma_visit::{Visit, VisitWith};

use crate::sloc::production_sloc;

/// A top-level program entity extracted from a module, with its production SLOC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    /// Source-declared name, or a synthetic name for executable file-scope content.
    pub name: SmolStr,
    /// The program entity represented by this declaration.
    pub kind: DeclarationKind,
    /// Whether the declaration is exported from the module.
    pub exported: bool,
    /// Production SLOC attributed to this declaration.
    pub sloc: u32,
    /// Names this declaration extends or implements (drives inheritance edges).
    pub supertypes: Vec<SmolStr>,
    /// Identifiers referenced within this declaration's body.
    pub referenced: Vec<SmolStr>,
    /// Identifiers invoked as a call or `new` target within this declaration
    /// (drives call edges, resolved through the symbol tables).
    pub called: Vec<SmolStr>,
    /// Literal specifiers of `import('...')` calls inside this declaration.
    pub dynamic_imports: Vec<SmolStr>,
    /// Signature types whose names uniquely match this declaration as owner.
    #[serde(default)]
    pub signature_companions: Vec<SmolStr>,
}

/// The semantic role of a parsed top-level declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclarationKind {
    /// A value-level declaration.
    Symbol,
    /// A type-level declaration.
    Type,
    /// Executable file-scope content without an independent declaration.
    FileBody,
}

/// A static import (`import { x } from '...'`) with literal specifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaticImport {
    /// The module specifier string (e.g. `./util` or `@scope/pkg`).
    pub source: SmolStr,
    /// Locally bound names brought in by this import.
    pub names: Vec<SmolStr>,
    /// `true` for `import type` / type-only specifiers (erased at runtime).
    pub type_only: bool,
}

/// A re-export (`export { x } from '...'`) emitted verbatim for the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReExport {
    /// The module specifier the symbols are re-exported from.
    pub source: SmolStr,
    /// Named re-export bindings, empty for `export * from '...'`.
    #[serde(default)]
    pub names: Vec<ReExportBinding>,
    /// `true` for `export type { x } from '...'`.
    pub type_only: bool,
}

/// A source binding introduced by a re-export declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ReExportBinding {
    /// Legacy unaliased payload (`"Widget"`).
    Legacy(SmolStr),
    /// A named binding, including an explicitly represented unaliased name.
    Named {
        /// Name looked up in the source module.
        original: SmolStr,
        /// Name introduced in the re-exporting module.
        exported: SmolStr,
    },
    /// A namespace binding (`export * as namespace`).
    Namespace {
        /// Namespace symbol introduced in the re-exporting module.
        namespace: SmolStr,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedReExportBinding {
    original: SmolStr,
    exported: SmolStr,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespaceReExportBinding {
    namespace: SmolStr,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReExportBindingWire {
    Legacy(SmolStr),
    Named(NamedReExportBinding),
    Namespace(NamespaceReExportBinding),
}

impl<'de> Deserialize<'de> for ReExportBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match ReExportBindingWire::deserialize(deserializer)? {
            ReExportBindingWire::Legacy(name) => Self::Legacy(name),
            ReExportBindingWire::Named(binding) => Self::Named {
                original: binding.original,
                exported: binding.exported,
            },
            ReExportBindingWire::Namespace(binding) => Self::Namespace {
                namespace: binding.namespace,
            },
        })
    }
}

/// A serializable, language-agnostic summary of one parsed module.
///
/// This is the payload the adapter threads through [`strata_ir::ParseTree`]:
/// [`parse`] produces it, [`crate::bind::bind`] consumes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedModule {
    /// Repository-relative path of the source file.
    pub path: SmolStr,
    /// Declared entities in source order, followed by any aggregated file body.
    pub declarations: Vec<Declaration>,
    /// Static imports with literal specifiers.
    pub imports: Vec<StaticImport>,
    /// `export ... from '...'` re-exports, emitted as-is.
    pub re_exports: Vec<ReExport>,
}

/// Parses TypeScript / TSX sources into [`ParsedModule`]s, one rayon task per file.
///
/// # Errors
///
/// Returns [`AdapterError::Parse`] for the first file (in input order) that
/// contains a syntax error; the reason embeds the line and column of the failure.
pub fn parse(files: &[SourceFile]) -> Result<Vec<ParsedModule>, AdapterError> {
    files.par_iter().map(parse_one).collect()
}

/// Parses a single source file into a [`ParsedModule`].
fn parse_one(file: &SourceFile) -> Result<ParsedModule, AdapterError> {
    let source_map: Lrc<SourceMap> = Lrc::default();
    let source = source_map.new_source_file(
        Lrc::new(FileName::Custom(file.path.to_string())),
        file.contents.clone(),
    );
    let base = source.start_pos;

    let tsx = file.path.ends_with(".tsx");
    let lexer = Lexer::new(
        Syntax::Typescript(TsSyntax {
            tsx,
            ..TsSyntax::default()
        }),
        swc_ecma_ast::EsVersion::default(),
        StringInput::from(&*source),
        None,
    );
    let mut parser = Parser::new_from(lexer);

    let module = parser.parse_module().map_err(|error| AdapterError::Parse {
        path: file.path.clone(),
        reason: format_parse_error(&error, &source_map),
    })?;

    Ok(extract(&file.path, &module, &file.contents, base))
}

/// Renders a parser error as a `line L, column C: message` string.
fn format_parse_error(error: &swc_ecma_parser::error::Error, source_map: &SourceMap) -> String {
    let location = source_map.lookup_char_pos(error.span().lo);
    format!(
        "line {}, column {}: {}",
        location.line,
        location.col_display,
        error.kind().msg()
    )
}

/// Converts a string-literal specifier to a [`SmolStr`], lossily for the rare
/// non-UTF-8 case (module specifiers are UTF-8 in every real source).
fn specifier(value: &swc_ecma_ast::Str) -> SmolStr {
    SmolStr::new(value.value.as_str().unwrap_or_default())
}

/// Walks `module` top-level items, building the [`ParsedModule`] summary.
fn extract(path: &SmolStr, module: &Module, contents: &str, base: BytePos) -> ParsedModule {
    let mut declarations = Vec::new();
    let mut imports = Vec::new();
    let mut re_exports = Vec::new();
    // Top-level executable statements that declare nothing still carry SLOC
    // and dependencies, so they aggregate into one synthetic `<module>` entity.
    let mut module_statements: Vec<&swc_ecma_ast::Stmt> = Vec::new();

    for item in &module.body {
        match item {
            ModuleItem::ModuleDecl(decl) => match decl {
                ModuleDecl::Import(import) => {
                    let names = import
                        .specifiers
                        .iter()
                        .map(|specifier| SmolStr::new(specifier.local().sym.as_str()))
                        .collect();
                    imports.push(StaticImport {
                        source: specifier(&import.src),
                        names,
                        type_only: import.type_only,
                    });
                }
                ModuleDecl::ExportNamed(named) => {
                    if let Some(src) = &named.src {
                        let names = named
                            .specifiers
                            .iter()
                            .filter_map(export_specifier_binding)
                            .collect();
                        re_exports.push(ReExport {
                            source: specifier(src),
                            names,
                            type_only: named.type_only,
                        });
                    }
                }
                ModuleDecl::ExportAll(all) => {
                    re_exports.push(ReExport {
                        source: specifier(&all.src),
                        names: Vec::new(),
                        type_only: all.type_only,
                    });
                }
                ModuleDecl::ExportDecl(export) => {
                    push_decl(&export.decl, true, contents, base, &mut declarations);
                }
                ModuleDecl::ExportDefaultDecl(default) => {
                    push_default_decl(default, contents, base, &mut declarations);
                }
                ModuleDecl::ExportDefaultExpr(_)
                | ModuleDecl::TsImportEquals(_)
                | ModuleDecl::TsExportAssignment(_)
                | ModuleDecl::TsNamespaceExport(_) => {}
            },
            ModuleItem::Stmt(stmt) => {
                if let swc_ecma_ast::Stmt::Decl(decl) = stmt {
                    push_decl(decl, false, contents, base, &mut declarations);
                } else if !matches!(stmt, swc_ecma_ast::Stmt::Empty(_)) {
                    module_statements.push(stmt);
                }
            }
        }
    }

    if !module_statements.is_empty() {
        let mut references = ReferenceCollector::default();
        let mut sloc: u32 = 0;
        for statement in &module_statements {
            statement.visit_with(&mut references);
            sloc = sloc.saturating_add(slice_sloc(statement.span(), contents, base));
        }
        declarations.push(Declaration {
            name: SmolStr::new("<module>"),
            kind: DeclarationKind::FileBody,
            exported: false,
            sloc,
            supertypes: Vec::new(),
            referenced: references.referenced,
            called: references.called,
            dynamic_imports: references.dynamic_imports,
            signature_companions: Vec::new(),
        });
    }

    ParsedModule {
        path: path.clone(),
        declarations,
        imports,
        re_exports,
    }
}

/// Preserves both sides of a named export (`export { a as b }` -> `(a, b)`).
fn export_specifier_binding(export: &swc_ecma_ast::ExportSpecifier) -> Option<ReExportBinding> {
    use swc_ecma_ast::ExportSpecifier;
    let (original, exported) = match export {
        ExportSpecifier::Named(named) => (
            module_export_name(&named.orig),
            module_export_name(named.exported.as_ref().unwrap_or(&named.orig)),
        ),
        ExportSpecifier::Namespace(ns) => {
            let name = module_export_name(&ns.name);
            return Some(ReExportBinding::Namespace { namespace: name });
        }
        ExportSpecifier::Default(_) => return None,
    };
    Some(ReExportBinding::Named { original, exported })
}

fn module_export_name(name: &swc_ecma_ast::ModuleExportName) -> SmolStr {
    use swc_ecma_ast::ModuleExportName;
    match name {
        ModuleExportName::Ident(ident) => SmolStr::new(ident.sym.as_str()),
        ModuleExportName::Str(string) => specifier(string),
    }
}

/// Appends the declarations introduced by a top-level `Decl` to `out`.
fn push_decl(
    decl: &Decl,
    exported: bool,
    contents: &str,
    base: BytePos,
    out: &mut Vec<Declaration>,
) {
    match decl {
        Decl::Class(class) => out.push(class_declaration(class, exported, contents, base)),
        Decl::Fn(function) => out.push(fn_declaration(function, exported, contents, base)),
        Decl::Var(var) => var_declarations(var, exported, contents, base, out),
        Decl::TsInterface(interface) => {
            out.push(interface_declaration(interface, exported, contents, base));
        }
        Decl::TsTypeAlias(alias) => {
            out.push(type_alias_declaration(alias, exported, contents, base));
        }
        Decl::TsEnum(ts_enum) => out.push(enum_declaration(ts_enum, exported, contents, base)),
        Decl::Using(_) | Decl::TsModule(_) => {}
    }
}

/// Appends a default-exported class, function, or interface declaration.
fn push_default_decl(
    default: &swc_ecma_ast::ExportDefaultDecl,
    contents: &str,
    base: BytePos,
    out: &mut Vec<Declaration>,
) {
    match &default.decl {
        DefaultDecl::Class(class) => {
            let name = class.ident.as_ref().map_or_else(
                || SmolStr::new("default"),
                |ident| SmolStr::new(ident.sym.as_str()),
            );
            let mut references = ReferenceCollector::default();
            class.class.visit_with(&mut references);
            out.push(Declaration {
                name: name.clone(),
                kind: DeclarationKind::Symbol,
                exported: true,
                sloc: slice_sloc(default.span(), contents, base),
                supertypes: supertypes_of_class(&class.class),
                referenced: references.referenced,
                called: references.called,
                dynamic_imports: references.dynamic_imports,
                signature_companions: class_signature_companions(&name, &class.class),
            });
        }
        DefaultDecl::Fn(function) => {
            let name = function.ident.as_ref().map_or_else(
                || SmolStr::new("default"),
                |ident| SmolStr::new(ident.sym.as_str()),
            );
            let mut references = ReferenceCollector::default();
            function.function.visit_with(&mut references);
            out.push(Declaration {
                name: name.clone(),
                kind: DeclarationKind::Symbol,
                exported: true,
                sloc: slice_sloc(default.span(), contents, base),
                supertypes: Vec::new(),
                referenced: references.referenced,
                called: references.called,
                dynamic_imports: references.dynamic_imports,
                signature_companions: function_signature_companions(&name, &function.function),
            });
        }
        DefaultDecl::TsInterfaceDecl(interface) => {
            out.push(interface_declaration(interface, true, contents, base));
        }
    }
}

/// Builds a [`Declaration`] for a class, capturing supertypes and references.
fn class_declaration(
    class: &ClassDecl,
    exported: bool,
    contents: &str,
    base: BytePos,
) -> Declaration {
    let mut references = ReferenceCollector::default();
    class.class.visit_with(&mut references);
    Declaration {
        name: SmolStr::new(class.ident.sym.as_str()),
        kind: DeclarationKind::Symbol,
        exported,
        sloc: slice_sloc(class.class.span(), contents, base),
        supertypes: supertypes_of_class(&class.class),
        referenced: references.referenced,
        called: references.called,
        dynamic_imports: references.dynamic_imports,
        signature_companions: class_signature_companions(class.ident.sym.as_str(), &class.class),
    }
}

/// Builds a [`Declaration`] for a function.
fn fn_declaration(function: &FnDecl, exported: bool, contents: &str, base: BytePos) -> Declaration {
    let mut references = ReferenceCollector::default();
    function.function.visit_with(&mut references);
    Declaration {
        name: SmolStr::new(function.ident.sym.as_str()),
        kind: DeclarationKind::Symbol,
        exported,
        sloc: slice_sloc(function.function.span(), contents, base),
        supertypes: Vec::new(),
        referenced: references.referenced,
        called: references.called,
        dynamic_imports: references.dynamic_imports,
        signature_companions: function_signature_companions(
            function.ident.sym.as_str(),
            &function.function,
        ),
    }
}

/// Appends a [`Declaration`] for each binding in a `const` / `let` / `var`.
fn var_declarations(
    var: &VarDecl,
    exported: bool,
    contents: &str,
    base: BytePos,
    out: &mut Vec<Declaration>,
) {
    for declarator in &var.decls {
        let Pat::Ident(binding) = &declarator.name else {
            continue;
        };
        let mut references = ReferenceCollector::default();
        if let Some(init) = &declarator.init {
            init.visit_with(&mut references);
        }
        out.push(Declaration {
            name: SmolStr::new(binding.id.sym.as_str()),
            kind: DeclarationKind::Symbol,
            exported,
            sloc: slice_sloc(declarator.span(), contents, base),
            supertypes: Vec::new(),
            referenced: references.referenced,
            called: references.called,
            dynamic_imports: references.dynamic_imports,
            signature_companions: Vec::new(),
        });
    }
}

/// Builds a type-level [`Declaration`] for an interface, capturing `extends`.
fn interface_declaration(
    interface: &TsInterfaceDecl,
    exported: bool,
    contents: &str,
    base: BytePos,
) -> Declaration {
    let mut references = ReferenceCollector::for_declaration_members();
    references.visit_type_parameter_scope(interface.type_params.as_deref(), |references| {
        interface.body.visit_with(references);
    });
    Declaration {
        name: SmolStr::new(interface.id.sym.as_str()),
        kind: DeclarationKind::Type,
        exported,
        sloc: slice_sloc(interface.span, contents, base),
        supertypes: interface.extends.iter().filter_map(type_ref_name).collect(),
        referenced: references.referenced,
        called: references.called,
        dynamic_imports: references.dynamic_imports,
        signature_companions: Vec::new(),
    }
}

/// Builds a type-level [`Declaration`] for a `type` alias.
fn type_alias_declaration(
    alias: &TsTypeAliasDecl,
    exported: bool,
    contents: &str,
    base: BytePos,
) -> Declaration {
    let mut references = ReferenceCollector::for_declaration_members();
    references.visit_type_parameter_scope(alias.type_params.as_deref(), |references| {
        alias.type_ann.visit_with(references);
    });
    Declaration {
        name: SmolStr::new(alias.id.sym.as_str()),
        kind: DeclarationKind::Type,
        exported,
        sloc: slice_sloc(alias.span, contents, base),
        supertypes: Vec::new(),
        referenced: references.referenced,
        called: references.called,
        dynamic_imports: references.dynamic_imports,
        signature_companions: Vec::new(),
    }
}

/// Builds a type-level [`Declaration`] for an `enum`.
fn enum_declaration(
    ts_enum: &TsEnumDecl,
    exported: bool,
    contents: &str,
    base: BytePos,
) -> Declaration {
    Declaration {
        name: SmolStr::new(ts_enum.id.sym.as_str()),
        kind: DeclarationKind::Type,
        exported,
        sloc: slice_sloc(ts_enum.span, contents, base),
        supertypes: Vec::new(),
        referenced: Vec::new(),
        called: Vec::new(),
        dynamic_imports: Vec::new(),
        signature_companions: Vec::new(),
    }
}

fn function_signature_companions(
    owner_name: &str,
    function: &swc_ecma_ast::Function,
) -> Vec<SmolStr> {
    signature_type_names(function)
        .into_iter()
        .filter(|type_name| companion_name_matches(type_name, owner_name))
        .collect()
}

fn class_signature_companions(class_name: &str, class: &swc_ecma_ast::Class) -> Vec<SmolStr> {
    let mut companions = Vec::new();
    for member in &class.body {
        let ClassMember::Method(method) = member else {
            continue;
        };
        let PropName::Ident(method_name) = &method.key else {
            continue;
        };
        let owner_name = format!("{class_name}_{}", method_name.sym);
        companions.extend(
            signature_type_names(&method.function)
                .into_iter()
                .filter(|type_name| companion_name_matches(type_name, &owner_name)),
        );
    }
    companions
}

fn signature_type_names(function: &swc_ecma_ast::Function) -> BTreeSet<SmolStr> {
    let mut collector = ReferenceCollector::for_declaration_members();
    for parameter in &function.params {
        if let Some(type_ann) = pattern_type_annotation(&parameter.pat) {
            type_ann.type_ann.visit_with(&mut collector);
        }
    }
    if let Some(return_type) = &function.return_type {
        return_type.type_ann.visit_with(&mut collector);
    }
    collector.referenced.into_iter().collect()
}

/// Returns only the annotation attached to a parameter pattern.
///
/// Assignment defaults and destructuring bodies are deliberately not visited:
/// companion evidence comes from the declared signature, never expressions.
fn pattern_type_annotation(pattern: &Pat) -> Option<&swc_ecma_ast::TsTypeAnn> {
    match pattern {
        Pat::Ident(binding) => binding.type_ann.as_deref(),
        Pat::Array(array) => array.type_ann.as_deref(),
        Pat::Object(object) => object.type_ann.as_deref(),
        Pat::Rest(rest) => rest.type_ann.as_deref(),
        Pat::Assign(assign) => pattern_type_annotation(&assign.left),
        Pat::Invalid(_) | Pat::Expr(_) => None,
    }
}

fn companion_name_matches(type_name: &str, owner_name: &str) -> bool {
    const SUFFIXES: [&str; 7] = [
        "Params", "Options", "Input", "Output", "Result", "Context", "State",
    ];
    let Some(stem) = SUFFIXES
        .iter()
        .find_map(|suffix| type_name.strip_suffix(suffix))
    else {
        return false;
    };
    let companion = semantic_tokens(stem);
    companion.len() >= 2 && companion == semantic_tokens(owner_name)
}

fn semantic_tokens(name: &str) -> BTreeSet<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();
    for (index, ch) in chars.iter().copied().enumerate() {
        let boundary = !current.is_empty()
            && (ch == '_'
                || ch == '-'
                || (ch.is_uppercase()
                    && chars
                        .get(index.wrapping_sub(1))
                        .is_some_and(|previous| previous.is_lowercase())));
        if boundary {
            words.push(std::mem::take(&mut current));
        }
        if ch != '_' && ch != '-' {
            current.push(ch.to_ascii_lowercase());
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
        .into_iter()
        .filter(|word| word != "adapter" && word != "to")
        .map(|word| normalize_ing(&word))
        .collect()
}

fn normalize_ing(word: &str) -> String {
    let Some(stem) = word.strip_suffix("ing") else {
        return word.to_owned();
    };
    let mut normalized = stem.to_owned();
    if matches!(normalized.as_bytes(), [.., penultimate, last] if penultimate == last) {
        normalized.pop();
    }
    normalized
}

/// Collects the names a class extends and implements.
fn supertypes_of_class(class: &swc_ecma_ast::Class) -> Vec<SmolStr> {
    let mut names = Vec::new();
    if let Some(super_class) = &class.super_class
        && let Expr::Ident(ident) = super_class.as_ref()
    {
        names.push(SmolStr::new(ident.sym.as_str()));
    }
    names.extend(class.implements.iter().filter_map(type_ref_name));
    names
}

/// Extracts the leading identifier name of a `extends` / `implements` clause.
fn type_ref_name(reference: &TsExprWithTypeArgs) -> Option<SmolStr> {
    match reference.expr.as_ref() {
        Expr::Ident(ident) => Some(SmolStr::new(ident.sym.as_str())),
        _ => None,
    }
}

/// Computes production SLOC for the source slice a span covers.
fn slice_sloc(span: swc_common::Span, contents: &str, base: BytePos) -> u32 {
    let lo = (span.lo.0 - base.0) as usize;
    let hi = (span.hi.0 - base.0) as usize;
    contents.get(lo..hi).map_or(0, production_sloc)
}

/// A [`Visit`] that gathers referenced identifiers, invoked identifiers, and
/// dynamic-import literals reachable from a single top-level declaration's body.
#[derive(Default)]
struct ReferenceCollector {
    /// Whether declaration-member keys and binders should be excluded.
    is_declaration_member_scope: bool,
    /// Type-level names bound by the declaration scopes currently being visited.
    bound_type_names: Vec<SmolStr>,
    /// Identifiers referenced within the visited subtree.
    referenced: Vec<SmolStr>,
    /// Identifiers invoked as a call or `new` target within the subtree.
    called: Vec<SmolStr>,
    /// Literal specifiers of `import('...')` calls within the subtree.
    dynamic_imports: Vec<SmolStr>,
}

impl ReferenceCollector {
    fn for_declaration_members() -> Self {
        Self {
            is_declaration_member_scope: true,
            ..Self::default()
        }
    }

    fn visit_type_parameter_scope(
        &mut self,
        params: Option<&swc_ecma_ast::TsTypeParamDecl>,
        visit: impl FnOnce(&mut Self),
    ) {
        let Some(params) = params else {
            visit(self);
            return;
        };
        let names = params
            .params
            .iter()
            .map(|param| SmolStr::new(param.name.sym.as_str()));
        self.visit_bound_type_scope(names, |references| {
            for param in &params.params {
                param.constraint.visit_with(references);
                param.default.visit_with(references);
            }
            visit(references);
        });
    }

    fn visit_bound_type_scope(
        &mut self,
        names: impl IntoIterator<Item = SmolStr>,
        visit: impl FnOnce(&mut Self),
    ) {
        let mut scoped = Self {
            is_declaration_member_scope: self.is_declaration_member_scope,
            bound_type_names: self.bound_type_names.clone(),
            ..Self::default()
        };
        scoped.bound_type_names.extend(names);
        visit(&mut scoped);
        self.referenced.extend(scoped.referenced);
        self.called.extend(scoped.called);
        self.dynamic_imports.extend(scoped.dynamic_imports);
    }
}

impl Visit for ReferenceCollector {
    fn visit_ident(&mut self, ident: &swc_ecma_ast::Ident) {
        if self.is_declaration_member_scope
            && self
                .bound_type_names
                .iter()
                .any(|name| name == ident.sym.as_str())
        {
            return;
        }
        self.referenced.push(SmolStr::new(ident.sym.as_str()));
    }

    fn visit_binding_ident(&mut self, binding: &swc_ecma_ast::BindingIdent) {
        if self.is_declaration_member_scope {
            binding.type_ann.visit_with(self);
        } else {
            binding.visit_children_with(self);
        }
    }

    fn visit_ts_type_param(&mut self, param: &swc_ecma_ast::TsTypeParam) {
        if self.is_declaration_member_scope {
            param.constraint.visit_with(self);
            param.default.visit_with(self);
        } else {
            param.visit_children_with(self);
        }
    }

    fn visit_ts_property_signature(&mut self, property: &swc_ecma_ast::TsPropertySignature) {
        if property.computed || !self.is_declaration_member_scope {
            property.key.visit_with(self);
        }
        property.type_ann.visit_with(self);
    }

    fn visit_ts_getter_signature(&mut self, getter: &swc_ecma_ast::TsGetterSignature) {
        if getter.computed || !self.is_declaration_member_scope {
            getter.key.visit_with(self);
        }
        getter.type_ann.visit_with(self);
    }

    fn visit_ts_setter_signature(&mut self, setter: &swc_ecma_ast::TsSetterSignature) {
        if setter.computed || !self.is_declaration_member_scope {
            setter.key.visit_with(self);
        }
        setter.param.visit_with(self);
    }

    fn visit_ts_method_signature(&mut self, method: &swc_ecma_ast::TsMethodSignature) {
        if !self.is_declaration_member_scope {
            method.visit_children_with(self);
            return;
        }
        if method.computed {
            method.key.visit_with(self);
        }
        self.visit_type_parameter_scope(method.type_params.as_deref(), |references| {
            method.params.visit_with(references);
            method.type_ann.visit_with(references);
        });
    }

    fn visit_ts_call_signature_decl(&mut self, call: &swc_ecma_ast::TsCallSignatureDecl) {
        if !self.is_declaration_member_scope {
            call.visit_children_with(self);
            return;
        }
        self.visit_type_parameter_scope(call.type_params.as_deref(), |references| {
            call.params.visit_with(references);
            call.type_ann.visit_with(references);
        });
    }

    fn visit_ts_construct_signature_decl(
        &mut self,
        constructor: &swc_ecma_ast::TsConstructSignatureDecl,
    ) {
        if !self.is_declaration_member_scope {
            constructor.visit_children_with(self);
            return;
        }
        self.visit_type_parameter_scope(constructor.type_params.as_deref(), |references| {
            constructor.params.visit_with(references);
            constructor.type_ann.visit_with(references);
        });
    }

    fn visit_ts_fn_type(&mut self, function: &swc_ecma_ast::TsFnType) {
        if !self.is_declaration_member_scope {
            function.visit_children_with(self);
            return;
        }
        self.visit_type_parameter_scope(function.type_params.as_deref(), |references| {
            function.params.visit_with(references);
            function.type_ann.visit_with(references);
        });
    }

    fn visit_ts_constructor_type(&mut self, constructor: &swc_ecma_ast::TsConstructorType) {
        if !self.is_declaration_member_scope {
            constructor.visit_children_with(self);
            return;
        }
        self.visit_type_parameter_scope(constructor.type_params.as_deref(), |references| {
            constructor.params.visit_with(references);
            constructor.type_ann.visit_with(references);
        });
    }

    fn visit_ts_tuple_element(&mut self, element: &swc_ecma_ast::TsTupleElement) {
        if self.is_declaration_member_scope {
            element.ty.visit_with(self);
        } else {
            element.visit_children_with(self);
        }
    }

    fn visit_ts_type_predicate(&mut self, predicate: &swc_ecma_ast::TsTypePredicate) {
        if self.is_declaration_member_scope {
            predicate.type_ann.visit_with(self);
        } else {
            predicate.visit_children_with(self);
        }
    }

    fn visit_ts_mapped_type(&mut self, mapped: &swc_ecma_ast::TsMappedType) {
        if !self.is_declaration_member_scope {
            mapped.visit_children_with(self);
            return;
        }
        mapped.type_param.constraint.visit_with(self);
        mapped.type_param.default.visit_with(self);
        let name = SmolStr::new(mapped.type_param.name.sym.as_str());
        self.visit_bound_type_scope([name], |references| {
            mapped.name_type.visit_with(references);
            mapped.type_ann.visit_with(references);
        });
    }

    fn visit_ts_conditional_type(&mut self, conditional: &swc_ecma_ast::TsConditionalType) {
        if !self.is_declaration_member_scope {
            conditional.visit_children_with(self);
            return;
        }
        conditional.check_type.visit_with(self);
        let mut bindings = InferBindingCollector::default();
        conditional.extends_type.visit_with(&mut bindings);
        self.visit_bound_type_scope(bindings.names, |references| {
            conditional.extends_type.visit_with(references);
            conditional.true_type.visit_with(references);
        });
        conditional.false_type.visit_with(self);
    }

    fn visit_ts_qualified_name(&mut self, name: &swc_ecma_ast::TsQualifiedName) {
        if self.is_declaration_member_scope {
            name.left.visit_with(self);
        } else {
            name.visit_children_with(self);
        }
    }

    fn visit_object_pat_prop(&mut self, property: &swc_ecma_ast::ObjectPatProp) {
        if !self.is_declaration_member_scope {
            property.visit_children_with(self);
            return;
        }
        match property {
            swc_ecma_ast::ObjectPatProp::KeyValue(property) => {
                if let swc_ecma_ast::PropName::Computed(key) = &property.key {
                    key.expr.visit_with(self);
                }
                property.value.visit_with(self);
            }
            swc_ecma_ast::ObjectPatProp::Assign(property) => {
                property.key.type_ann.visit_with(self);
                property.value.visit_with(self);
            }
            swc_ecma_ast::ObjectPatProp::Rest(rest) => rest.visit_with(self),
        }
    }

    fn visit_call_expr(&mut self, call: &CallExpr) {
        match &call.callee {
            Callee::Import(_) => {
                if let Some(first) = call.args.first()
                    && let Expr::Lit(Lit::Str(literal)) = first.expr.as_ref()
                {
                    self.dynamic_imports.push(specifier(literal));
                }
            }
            Callee::Expr(expr) => {
                if let Expr::Ident(ident) = expr.as_ref() {
                    self.called.push(SmolStr::new(ident.sym.as_str()));
                }
            }
            Callee::Super(_) => {}
        }
        call.visit_children_with(self);
    }

    fn visit_new_expr(&mut self, new: &NewExpr) {
        if let Expr::Ident(ident) = new.callee.as_ref() {
            self.called.push(SmolStr::new(ident.sym.as_str()));
        }
        new.visit_children_with(self);
    }
}

#[derive(Default)]
struct InferBindingCollector {
    names: Vec<SmolStr>,
}

impl Visit for InferBindingCollector {
    fn visit_ts_infer_type(&mut self, infer: &swc_ecma_ast::TsInferType) {
        self.names
            .push(SmolStr::new(infer.type_param.name.sym.as_str()));
        infer.type_param.constraint.visit_with(self);
        infer.type_param.default.visit_with(self);
    }
}
