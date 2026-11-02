//! Scoped reference collection for parsed TypeScript declarations.

mod collector;
mod visit;

use std::collections::BTreeSet;

use smol_str::SmolStr;
use swc_ecma_ast::{Expr, Pat, Stmt};
use swc_ecma_visit::VisitWith;

use self::collector::ReferenceCollector;

/// References collected from one declaration-owned syntax subtree.
#[derive(Default)]
pub(super) struct References {
    pub(super) referenced: Vec<SmolStr>,
    pub(super) called: Vec<SmolStr>,
    pub(super) dynamic_imports: Vec<SmolStr>,
}

/// Collects references from a class body.
pub(super) fn collect_class(class: &swc_ecma_ast::Class) -> References {
    collect(class)
}

/// Collects references from a function body and signature.
pub(super) fn collect_function(function: &swc_ecma_ast::Function) -> References {
    collect(function)
}

/// Collects references from a variable initializer.
pub(super) fn collect_expression(expression: &Expr) -> References {
    collect(expression)
}

/// Collects references from top-level executable statements in source order.
pub(super) fn collect_statements(statements: &[&Stmt]) -> References {
    let mut collector = ReferenceCollector::default();
    for statement in statements {
        statement.visit_with(&mut collector);
    }
    collector.finish()
}

/// Collects scoped references from an interface's declaration members.
pub(super) fn collect_interface(interface: &swc_ecma_ast::TsInterfaceDecl) -> References {
    let mut collector = ReferenceCollector::for_declaration_members();
    collector.visit_type_parameter_scope(interface.type_params.as_deref(), |references| {
        interface.body.visit_with(references);
    });
    collector.finish()
}

/// Collects scoped references from a type alias body.
pub(super) fn collect_type_alias(alias: &swc_ecma_ast::TsTypeAliasDecl) -> References {
    let mut collector = ReferenceCollector::for_declaration_members();
    collector.visit_type_parameter_scope(alias.type_params.as_deref(), |references| {
        alias.type_ann.visit_with(references);
    });
    collector.finish()
}

/// Collects the unique type names appearing in a function signature.
pub(super) fn signature_type_names(function: &swc_ecma_ast::Function) -> BTreeSet<SmolStr> {
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

fn collect<T>(node: &T) -> References
where
    T: VisitWith<ReferenceCollector>,
{
    let mut collector = ReferenceCollector::default();
    node.visit_with(&mut collector);
    collector.finish()
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
