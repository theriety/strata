//! Expression walker: feeds compound expressions and comprehensions.

use std::collections::HashSet;

use rustpython_parser::ast::Expr;
use smol_str::SmolStr;

use super::collector::ReferenceCollector;
use crate::scope::{Scope, ScopeKind, argument_names, target_names};

/// Resolves the leading identifier name of an expression used as a base class
/// or annotation (`Shape` from `Shape`, `mod.Shape`, or `Shape[int]`).
pub(super) fn leading_name(expr: &Expr) -> Option<SmolStr> {
    match expr {
        Expr::Name(name) => Some(SmolStr::new(name.id.as_str())),
        Expr::Attribute(attribute) => leading_name(&attribute.value),
        Expr::Subscript(subscript) => leading_name(&subscript.value),
        _ => None,
    }
}

/// Walks the direct child expressions of a compound expression.
pub(super) fn collect_child_expressions_of_expr(expr: &Expr, collector: &mut ReferenceCollector) {
    match expr {
        Expr::Attribute(attribute) => collector.collect_expression(&attribute.value),
        Expr::Subscript(subscript) => {
            collector.collect_expression(&subscript.value);
            collector.collect_expression(&subscript.slice);
        }
        Expr::BinOp(operation) => {
            collector.collect_expression(&operation.left);
            collector.collect_expression(&operation.right);
        }
        Expr::BoolOp(operation) => {
            for value in &operation.values {
                collector.collect_expression(value);
            }
        }
        Expr::UnaryOp(operation) => collector.collect_expression(&operation.operand),
        Expr::Compare(comparison) => {
            collector.collect_expression(&comparison.left);
            for comparator in &comparison.comparators {
                collector.collect_expression(comparator);
            }
        }
        Expr::Await(expression) => collector.collect_expression(&expression.value),
        Expr::Starred(expression) => collector.collect_expression(&expression.value),
        Expr::List(list) => {
            for element in &list.elts {
                collector.collect_expression(element);
            }
        }
        Expr::Tuple(tuple) => {
            for element in &tuple.elts {
                collector.collect_expression(element);
            }
        }
        Expr::Set(set) => {
            for element in &set.elts {
                collector.collect_expression(element);
            }
        }
        Expr::Dict(dict) => {
            for value in dict.values.iter().chain(dict.keys.iter().flatten()) {
                collector.collect_expression(value);
            }
        }
        Expr::IfExp(expression) => {
            collector.collect_expression(&expression.test);
            collector.collect_expression(&expression.body);
            collector.collect_expression(&expression.orelse);
        }
        Expr::Lambda(lambda) => {
            collector.scopes.push(Scope::plain(
                ScopeKind::Function,
                argument_names(&lambda.args),
            ));
            collector.collect_expression(&lambda.body);
            collector.scopes.pop();
        }
        Expr::ListComp(comp) => collect_comprehension(&comp.generators, &[&comp.elt], collector),
        Expr::SetComp(comp) => collect_comprehension(&comp.generators, &[&comp.elt], collector),
        Expr::GeneratorExp(comp) => {
            collect_comprehension(&comp.generators, &[&comp.elt], collector);
        }
        Expr::DictComp(comp) => {
            collect_comprehension(&comp.generators, &[&comp.key, &comp.value], collector);
        }
        Expr::JoinedStr(joined) => {
            for value in &joined.values {
                collector.collect_expression(value);
            }
        }
        Expr::FormattedValue(formatted) => {
            collector.collect_expression(&formatted.value);
            if let Some(spec) = &formatted.format_spec {
                collector.collect_expression(spec);
            }
        }
        Expr::NamedExpr(named) => {
            collector.collect_expression(&named.value);
            let mut targets = HashSet::new();
            target_names(&named.target, &mut targets);
            collector.bind_locals(targets);
        }
        Expr::Yield(expression) => {
            if let Some(value) = &expression.value {
                collector.collect_expression(value);
            }
        }
        Expr::YieldFrom(expression) => collector.collect_expression(&expression.value),
        Expr::Slice(slice) => {
            for bound in [&slice.lower, &slice.upper, &slice.step]
                .into_iter()
                .flatten()
            {
                collector.collect_expression(bound);
            }
        }
        _ => {}
    }
}

/// Walks a comprehension: generator iterables and filters, then the element
/// expressions, with the generator targets bound as locals for its duration.
fn collect_comprehension(
    generators: &[rustpython_parser::ast::Comprehension],
    elements: &[&Expr],
    collector: &mut ReferenceCollector,
) {
    let mut targets = HashSet::new();
    for generator in generators {
        target_names(&generator.target, &mut targets);
    }
    // The first iterable is evaluated in the enclosing scope, before any target
    // is bound.
    if let Some(first) = generators.first() {
        collector.collect_expression(&first.iter);
    }
    collector
        .scopes
        .push(Scope::plain(ScopeKind::Comprehension, targets));
    for (index, generator) in generators.iter().enumerate() {
        if index > 0 {
            collector.collect_expression(&generator.iter);
        }
        for condition in &generator.ifs {
            collector.collect_expression(condition);
        }
    }
    for element in elements {
        collector.collect_expression(element);
    }
    collector.scopes.pop();
}
