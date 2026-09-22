//! Ordered declaration, import, and re-export extraction from TypeScript modules.

use std::collections::BTreeSet;

use smol_str::SmolStr;
use swc_common::{BytePos, Spanned};
use swc_ecma_ast::{
    ClassDecl, ClassMember, Decl, DefaultDecl, Expr, FnDecl, Module, ModuleDecl, ModuleItem, Pat,
    PropName, TsEnumDecl, TsExprWithTypeArgs, TsInterfaceDecl, TsTypeAliasDecl, VarDecl,
};

use crate::sloc::production_sloc;

use super::references::{
    References, collect_class, collect_expression, collect_function, collect_interface,
    collect_statements, collect_type_alias, signature_type_names,
};
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

/// Builds a [`Declaration`] for a class, capturing supertypes and references.
fn class_declaration(
    class: &ClassDecl,
    exported: bool,
    contents: &str,
    base: BytePos,
) -> Declaration {
    let references = collect_class(&class.class);
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
    let references = collect_function(&function.function);
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
        let references = declarator
            .init
            .as_deref()
            .map_or_else(References::default, collect_expression);
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
    let references = collect_interface(interface);
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
    let references = collect_type_alias(alias);
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
