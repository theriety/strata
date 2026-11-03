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
use super::references::{collect_class, collect_expression, collect_function, collect_statements};
use super::{
    Declaration, DeclarationKind, ExportClause, ExportLoad, ImportBinding, ImportedName,
    ParsedModule, ReExport, ReExportBinding, StaticImport, specifier,
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
    let mut exports = Exports::default();
    // Top-level executable statements that declare nothing still carry SLOC
    // and dependencies, so they aggregate into one synthetic `<module>` entity.
    let mut module_statements: Vec<&swc_ecma_ast::Stmt> = Vec::new();

    for item in &module.body {
        match item {
            ModuleItem::ModuleDecl(decl) => match decl {
                ModuleDecl::Import(import) => imports.push(static_import(import)),
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
                    } else {
                        exports
                            .clauses
                            .extend(named.specifiers.iter().filter_map(local_export_clause));
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
                    exports
                        .clauses
                        .extend(declarations.last().map(|declaration| ExportClause {
                            local: declaration.name.clone(),
                            exported: SmolStr::new("default"),
                            mentioned: false,
                        }));
                }
                ModuleDecl::ExportDefaultExpr(default) => {
                    exports.expression(&default.expr, "default");
                }
                ModuleDecl::TsExportAssignment(assignment) => {
                    exports.expression(&assignment.expr, "=");
                }
                ModuleDecl::TsImportEquals(import) => exports.import_equals(import),
                ModuleDecl::TsNamespaceExport(_) => {}
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
            requires: references.requires,
            signature_companions: Vec::new(),
        });
    }

    ParsedModule {
        path: path.clone(),
        declarations,
        imports,
        re_exports,
        export_clauses: exports.clauses,
        export_loads: exports.loads,
    }
}

/// Summarizes one static import declaration.
fn static_import(import: &swc_ecma_ast::ImportDecl) -> StaticImport {
    StaticImport {
        source: specifier(&import.src),
        names: import
            .specifiers
            .iter()
            .map(|specifier| SmolStr::new(specifier.local().sym.as_str()))
            .collect(),
        type_only: import.type_only,
        namespace: import
            .specifiers
            .iter()
            .any(|specifier| matches!(specifier, swc_ecma_ast::ImportSpecifier::Namespace(_))),
        bindings: import.specifiers.iter().map(import_binding).collect(),
    }
}

/// Records which export of the source module an import specifier picks.
fn import_binding(specifier: &swc_ecma_ast::ImportSpecifier) -> ImportBinding {
    use swc_ecma_ast::ImportSpecifier;
    let local = SmolStr::new(specifier.local().sym.as_str());
    let imported = match specifier {
        ImportSpecifier::Named(named) => ImportedName::Named(
            named
                .imported
                .as_ref()
                .map_or_else(|| local.clone(), module_export_name),
        ),
        ImportSpecifier::Default(_) => ImportedName::Default,
        ImportSpecifier::Namespace(_) => ImportedName::Namespace,
    };
    ImportBinding { local, imported }
}

/// The clause a source-less `export { name as alias }` specifier states.
fn local_export_clause(export: &swc_ecma_ast::ExportSpecifier) -> Option<ExportClause> {
    match export {
        swc_ecma_ast::ExportSpecifier::Named(named) => Some(ExportClause {
            local: module_export_name(&named.orig),
            exported: module_export_name(named.exported.as_ref().unwrap_or(&named.orig)),
            mentioned: false,
        }),
        swc_ecma_ast::ExportSpecifier::Namespace(_) | swc_ecma_ast::ExportSpecifier::Default(_) => {
            None
        }
    }
}

/// What a module publishes without declaring it, gathered while walking it.
#[derive(Default)]
struct Exports {
    clauses: Vec<ExportClause>,
    loads: Vec<ExportLoad>,
}

impl Exports {
    /// Records an exported expression under `exported`.
    ///
    /// A bare identifier names the exported binding. Any other expression
    /// (`export default { foo }`, `export default wrap(Base)`) may hand out
    /// whatever it mentions, so each mentioned name becomes a clause marked as
    /// mentioned. A `require()` or `import()` inside it hands out the loaded
    /// module, and a computed target cannot be followed.
    fn expression(&mut self, expr: &swc_ecma_ast::Expr, exported: &str) {
        let clause = |local: SmolStr, mentioned: bool| ExportClause {
            local,
            exported: SmolStr::new(exported),
            mentioned,
        };
        if let swc_ecma_ast::Expr::Ident(ident) = expr {
            self.clauses
                .push(clause(SmolStr::new(ident.sym.as_str()), false));
            return;
        }
        let references = collect_expression(expr);
        let mut names: Vec<SmolStr> = references
            .referenced
            .into_iter()
            .chain(references.called)
            .collect();
        names.sort_unstable();
        names.dedup();
        self.clauses
            .extend(names.into_iter().map(|name| clause(name, true)));
        let load = |source: Option<SmolStr>| ExportLoad {
            exported: SmolStr::new(exported),
            source,
        };
        self.loads.extend(
            references
                .dynamic_imports
                .into_iter()
                .chain(references.requires)
                .map(|source| load(Some(source))),
        );
        if references.opaque_loads {
            self.loads.push(load(None));
        }
    }

    /// Records an `export import alias = ...`; a plain `import alias = ...`
    /// publishes nothing.
    fn import_equals(&mut self, import: &swc_ecma_ast::TsImportEqualsDecl) {
        if !import.is_export {
            return;
        }
        let source = match &import.module_ref {
            swc_ecma_ast::TsModuleRef::TsExternalModuleRef(external) => {
                Some(specifier(&external.expr))
            }
            swc_ecma_ast::TsModuleRef::TsEntityName(_) => None,
        };
        self.loads.push(ExportLoad {
            exported: SmolStr::new(import.id.sym.as_str()),
            source,
        });
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
                requires: references.requires,
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
                requires: references.requires,
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
