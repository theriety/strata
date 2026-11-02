//! Ordered declaration, import, and re-export extraction from TypeScript modules.

mod builders;
mod companions;

use smol_str::SmolStr;
use swc_common::{BytePos, Spanned};
use swc_ecma_ast::{Decl, DefaultDecl, Module, ModuleDecl, ModuleItem};

use crate::sloc::production_sloc;

use self::builders::{
    class_declaration, enum_declaration, fn_declaration, interface_declaration,
    supertypes_of_class, type_alias_declaration, var_declarations,
};
use self::companions::{class_signature_companions, function_signature_companions};
use super::references::{collect_class, collect_function, collect_statements};
use super::{
    Declaration, DeclarationKind, ParsedModule, ReExport, ReExportBinding, StaticImport, specifier,
};

/// Walks `module` top-level items, building the [`ParsedModule`] summary.
pub(super) fn extract(
    path: &SmolStr,
    module: &Module,
    contents: &str,
    base: BytePos,
) -> ParsedModule {
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
        let references = collect_statements(&module_statements);
        let mut sloc: u32 = 0;
        for statement in &module_statements {
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
            let references = collect_class(&class.class);
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
            let references = collect_function(&function.function);
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

/// Computes production SLOC for the source slice a span covers.
fn slice_sloc(span: swc_common::Span, contents: &str, base: BytePos) -> u32 {
    let lo = (span.lo.0 - base.0) as usize;
    let hi = (span.hi.0 - base.0) as usize;
    contents.get(lo..hi).map_or(0, production_sloc)
}
