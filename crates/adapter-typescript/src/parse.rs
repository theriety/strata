//! swc parsing of `.ts` / `.tsx` sources into serializable [`ParsedModule`]s.
//!
//! Parsing runs in parallel across files (one rayon task per file). A syntax
//! error is surfaced as [`AdapterError::Parse`] carrying the offending path and
//! a span-derived reason — files are never skipped silently.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use strata_ir::{AdapterError, SourceFile};
use swc_common::{BytePos, FileName, SourceMap, Spanned, sync::Lrc};
use swc_ecma_ast::{
    CallExpr, Callee, ClassDecl, Decl, DefaultDecl, Expr, FnDecl, Lit, Module, ModuleDecl,
    ModuleItem, NewExpr, Pat, TsEnumDecl, TsExprWithTypeArgs, TsInterfaceDecl, TsTypeAliasDecl,
    VarDecl,
};
use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};
use swc_ecma_visit::{Visit, VisitWith};

use crate::sloc::production_sloc;

/// A top-level declaration extracted from a module, with its production SLOC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    /// Source-declared name of the symbol or type.
    pub name: SmolStr,
    /// Whether the declaration is a type-level entity (interface, alias, enum).
    pub is_type: bool,
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
    /// Names re-exported, empty for `export * from '...'`.
    pub names: Vec<SmolStr>,
    /// `true` for `export type { x } from '...'`.
    pub type_only: bool,
}

/// A serializable, language-agnostic summary of one parsed module.
///
/// This is the payload the adapter threads through [`strata_ir::ParseTree`]:
/// [`parse`] produces it, [`crate::bind::bind`] consumes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedModule {
    /// Repository-relative path of the source file.
    pub path: SmolStr,
    /// Top-level declarations in source order.
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
    // Top-level statements that declare nothing — vitest/jest suites
    // (`describe('...', () => ...)`) and side-effect calls alike. Nothing about
    // them is minted as a symbol, yet their references couple this module to
    // others, so they aggregate into one synthetic `<module>` declaration.
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
                            .filter_map(export_specifier_name)
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
            is_type: false,
            exported: false,
            sloc,
            supertypes: Vec::new(),
            referenced: references.referenced,
            called: references.called,
            dynamic_imports: references.dynamic_imports,
        });
    }

    ParsedModule {
        path: path.clone(),
        declarations,
        imports,
        re_exports,
    }
}

/// Resolves the bound name of an export specifier (`export { a as b }` -> `b`).
fn export_specifier_name(export: &swc_ecma_ast::ExportSpecifier) -> Option<SmolStr> {
    use swc_ecma_ast::{ExportSpecifier, ModuleExportName};
    let name = match export {
        ExportSpecifier::Named(named) => named.exported.as_ref().unwrap_or(&named.orig),
        ExportSpecifier::Namespace(ns) => &ns.name,
        ExportSpecifier::Default(_) => return None,
    };
    match name {
        ModuleExportName::Ident(ident) => Some(SmolStr::new(ident.sym.as_str())),
        ModuleExportName::Str(string) => Some(specifier(string)),
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
                name,
                is_type: false,
                exported: true,
                sloc: slice_sloc(default.span(), contents, base),
                supertypes: supertypes_of_class(&class.class),
                referenced: references.referenced,
                called: references.called,
                dynamic_imports: references.dynamic_imports,
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
                name,
                is_type: false,
                exported: true,
                sloc: slice_sloc(default.span(), contents, base),
                supertypes: Vec::new(),
                referenced: references.referenced,
                called: references.called,
                dynamic_imports: references.dynamic_imports,
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
        is_type: false,
        exported,
        sloc: slice_sloc(class.class.span(), contents, base),
        supertypes: supertypes_of_class(&class.class),
        referenced: references.referenced,
        called: references.called,
        dynamic_imports: references.dynamic_imports,
    }
}

/// Builds a [`Declaration`] for a function.
fn fn_declaration(function: &FnDecl, exported: bool, contents: &str, base: BytePos) -> Declaration {
    let mut references = ReferenceCollector::default();
    function.function.visit_with(&mut references);
    Declaration {
        name: SmolStr::new(function.ident.sym.as_str()),
        is_type: false,
        exported,
        sloc: slice_sloc(function.function.span(), contents, base),
        supertypes: Vec::new(),
        referenced: references.referenced,
        called: references.called,
        dynamic_imports: references.dynamic_imports,
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
            is_type: false,
            exported,
            sloc: slice_sloc(declarator.span(), contents, base),
            supertypes: Vec::new(),
            referenced: references.referenced,
            called: references.called,
            dynamic_imports: references.dynamic_imports,
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
    Declaration {
        name: SmolStr::new(interface.id.sym.as_str()),
        is_type: true,
        exported,
        sloc: slice_sloc(interface.span, contents, base),
        supertypes: interface.extends.iter().filter_map(type_ref_name).collect(),
        referenced: Vec::new(),
        called: Vec::new(),
        dynamic_imports: Vec::new(),
    }
}

/// Builds a type-level [`Declaration`] for a `type` alias.
fn type_alias_declaration(
    alias: &TsTypeAliasDecl,
    exported: bool,
    contents: &str,
    base: BytePos,
) -> Declaration {
    Declaration {
        name: SmolStr::new(alias.id.sym.as_str()),
        is_type: true,
        exported,
        sloc: slice_sloc(alias.span, contents, base),
        supertypes: Vec::new(),
        referenced: Vec::new(),
        called: Vec::new(),
        dynamic_imports: Vec::new(),
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
        is_type: true,
        exported,
        sloc: slice_sloc(ts_enum.span, contents, base),
        supertypes: Vec::new(),
        referenced: Vec::new(),
        called: Vec::new(),
        dynamic_imports: Vec::new(),
    }
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
    /// Identifiers referenced within the visited subtree.
    referenced: Vec<SmolStr>,
    /// Identifiers invoked as a call or `new` target within the subtree.
    called: Vec<SmolStr>,
    /// Literal specifiers of `import('...')` calls within the subtree.
    dynamic_imports: Vec<SmolStr>,
}

impl Visit for ReferenceCollector {
    fn visit_ident(&mut self, ident: &swc_ecma_ast::Ident) {
        self.referenced.push(SmolStr::new(ident.sym.as_str()));
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
