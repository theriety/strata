//! Declaration builders for classes, functions, variables, and type-level items.

use smol_str::SmolStr;
use swc_common::{BytePos, Spanned};
use swc_ecma_ast::{
    ClassDecl, Expr, FnDecl, Pat, TsEnumDecl, TsExprWithTypeArgs, TsInterfaceDecl, TsTypeAliasDecl,
    VarDecl,
};

use super::super::references::{
    References, collect_class, collect_expression, collect_function, collect_interface,
    collect_type_alias,
};
use super::super::{Declaration, DeclarationKind};
use super::companions::{class_signature_companions, function_signature_companions};
use super::slice_sloc;

/// Builds a [`Declaration`] for a class, capturing supertypes and references.
pub(super) fn class_declaration(
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
pub(super) fn fn_declaration(
    function: &FnDecl,
    exported: bool,
    contents: &str,
    base: BytePos,
) -> Declaration {
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
pub(super) fn var_declarations(
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
pub(super) fn interface_declaration(
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
pub(super) fn type_alias_declaration(
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
pub(super) fn enum_declaration(
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

/// Collects the names a class extends and implements.
pub(super) fn supertypes_of_class(class: &swc_ecma_ast::Class) -> Vec<SmolStr> {
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
