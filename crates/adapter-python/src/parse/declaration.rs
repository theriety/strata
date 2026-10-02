//! Declaration extraction: functions, classes, and module-level constants.
//!
//! Each top-level definition becomes a [`Declaration`] carrying its production
//! SLOC (docstring excluded) and the references its body collects.

use rustpython_parser::ast::{
    Constant, Expr, Ranged, Stmt, StmtAsyncFunctionDef, StmtClassDef, StmtFunctionDef,
};
use smol_str::SmolStr;

use super::Declaration;
use super::collector::{ReferenceCollector, collect_arguments, collect_body};
use super::expressions::leading_name;
use crate::scope::Scope;
use crate::sloc::production_sloc;

/// Builds a [`Declaration`] for a synchronous function definition.
pub(super) fn function_declaration(function: &StmtFunctionDef, source: &str) -> Declaration {
    function_like_declaration(
        function.name.as_str(),
        &function.args,
        function.returns.as_deref(),
        &function.body,
        source,
    )
}

/// Builds a [`Declaration`] for an `async def` function definition.
pub(super) fn async_function_declaration(
    function: &StmtAsyncFunctionDef,
    source: &str,
) -> Declaration {
    function_like_declaration(
        function.name.as_str(),
        &function.args,
        function.returns.as_deref(),
        &function.body,
        source,
    )
}

/// Shared builder for `def` and `async def`: annotations, then the body walked
/// inside the function's own scope.
fn function_like_declaration(
    name: &str,
    arguments: &rustpython_parser::ast::Arguments,
    returns: Option<&Expr>,
    body: &[Stmt],
    source: &str,
) -> Declaration {
    let mut collector = ReferenceCollector::default();
    collect_arguments(arguments, &mut collector);
    if let Some(returns) = returns {
        collector.collect_annotation(returns);
    }
    collector.scopes.push(Scope::function(arguments, body));
    collect_body(body, &mut collector);
    Declaration {
        name: SmolStr::new(name),
        is_class: false,
        sloc: body_sloc(body, source),
        bases: Vec::new(),
        annotations: collector.annotations,
        referenced: collector.referenced,
        called: collector.called,
        dynamic: collector.dynamic,
    }
}

/// Builds a [`Declaration`] for a class definition, capturing its base classes.
pub(super) fn class_declaration(class: &StmtClassDef, source: &str) -> Declaration {
    let mut collector = ReferenceCollector::default();
    let bases = class.bases.iter().filter_map(leading_name).collect();
    collector.collect_class_body(&class.body);
    Declaration {
        name: SmolStr::new(class.name.as_str()),
        is_class: true,
        sloc: body_sloc(&class.body, source),
        bases,
        annotations: collector.annotations,
        referenced: collector.referenced,
        called: collector.called,
        dynamic: collector.dynamic,
    }
}

/// Mints one constant [`Declaration`] per simple name a module-level assignment
/// binds (`x = v`, `a, b = v`, `x: T = v`).
///
/// A constant's references, calls, and dynamic constructs come from its
/// initializing value exactly as a function body's would, so `_cache =
/// build_cache(limit)` prices the same dependency a call inside a function
/// would. `__all__` is exempt — it feeds the star-import surface table rather
/// than the value graph — and a bare `x: T` without a value imports to nothing,
/// so neither mints a symbol.
pub(super) fn constant_declarations(
    targets: &[Expr],
    value: Option<&Expr>,
    annotation: Option<&Expr>,
    statement: &Stmt,
    source: &str,
) -> Vec<Declaration> {
    let Some(value) = value else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for target in targets {
        constant_target_names(target, &mut names);
    }
    if names.is_empty() {
        return Vec::new();
    }
    let mut collector = ReferenceCollector::default();
    collector.collect_expression(value);
    if let Some(annotation) = annotation {
        collector.collect_annotation(annotation);
    }
    names
        .into_iter()
        .map(|name| Declaration {
            name,
            is_class: false,
            sloc: statement_sloc(statement, source),
            bases: Vec::new(),
            annotations: collector.annotations.clone(),
            referenced: collector.referenced.clone(),
            called: collector.called.clone(),
            dynamic: collector.dynamic.clone(),
        })
        .collect()
}

/// Collects the importable names bound by an assignment target: the identifier
/// itself for `x`, and each element's names for tuple or list unpacking.
/// Attribute and subscript targets bind no module-level name.
fn constant_target_names(target: &Expr, out: &mut Vec<SmolStr>) {
    match target {
        Expr::Name(name) => {
            if is_extracted_constant(name.id.as_str()) {
                out.push(SmolStr::new(name.id.as_str()));
            }
        }
        Expr::Tuple(tuple) => {
            for element in &tuple.elts {
                constant_target_names(element, out);
            }
        }
        Expr::List(list) => {
            for element in &list.elts {
                constant_target_names(element, out);
            }
        }
        _ => {}
    }
}

/// Returns `true` unless the bound name is excluded from symbol extraction.
///
/// `__all__` is the one exclusion: it drives the star-import public surface,
/// where a phantom `__all__` symbol would double-represent it.
fn is_extracted_constant(name: &str) -> bool {
    name != "__all__"
}

/// Computes production SLOC for a single top-level statement.
fn statement_sloc(statement: &Stmt, source: &str) -> u32 {
    let lo = usize::from(statement.start());
    let hi = usize::from(statement.end());
    source.get(lo..hi).map_or(0, production_sloc)
}

/// Computes production SLOC for a declaration body, excluding its docstring.
///
/// The span runs from the first statement to the last; a leading docstring (a
/// bare string-literal expression statement) is dropped before counting so the
/// documentation never inflates a symbol's size.
fn body_sloc(body: &[Stmt], source: &str) -> u32 {
    let counted = strip_docstring(body);
    let Some(first) = counted.first() else {
        return 0;
    };
    let Some(last) = counted.last() else {
        return 0;
    };
    let lo = usize::from(first.start());
    let hi = usize::from(last.end());
    source.get(lo..hi).map_or(0, production_sloc)
}

/// Drops a leading docstring statement from a body, returning the remainder.
fn strip_docstring(body: &[Stmt]) -> &[Stmt] {
    if let Some((Stmt::Expr(expression), rest)) = body.split_first()
        && matches!(
            expression.value.as_ref(),
            Expr::Constant(constant) if matches!(constant.value, Constant::Str(_))
        )
    {
        return rest;
    }
    body
}
