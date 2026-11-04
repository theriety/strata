//! Detects a module that sets up lazy loading: a module-level `__getattr__`
//! (PEP 562) however the module comes by it (a `def`, an assignment, an import,
//! `globals()["__getattr__"] = ...`, `setattr(module, "__getattr__", ...)`), or a
//! module that replaces itself in `sys.modules`; also inside a module-level
//! `if`, `try` or `with` block. A package `__init__.py` that does so can expose
//! modules lazily, so the binder cannot tell what lies below it.

use rustpython_parser::ast::{Constant, Expr, Stmt};

use super::imports::child_blocks;

const GETATTR: &str = "__getattr__";

/// Whether a module-level statement of `suite` sets up lazy loading.
pub(super) fn sets_up_lazy_loading(suite: &[Stmt]) -> bool {
    suite.iter().any(binds_lazy_hook)
}

/// Whether `statement`, or a block nested in it, sets up lazy loading.
fn binds_lazy_hook(statement: &Stmt) -> bool {
    match statement {
        Stmt::FunctionDef(function) => function.name.as_str() == GETATTR,
        Stmt::AsyncFunctionDef(function) => function.name.as_str() == GETATTR,
        Stmt::Assign(assign) => assign.targets.iter().any(assigns_lazy_hook),
        Stmt::AnnAssign(assign) => assigns_lazy_hook(&assign.target),
        Stmt::ImportFrom(from) => from.names.iter().any(|alias| {
            alias
                .asname
                .as_ref()
                .map_or(alias.name.as_str(), |asname| asname.as_str())
                == GETATTR
        }),
        Stmt::Expr(expression) => sets_hook_by_call(&expression.value),
        other => child_blocks(other)
            .into_iter()
            .flatten()
            .any(binds_lazy_hook),
    }
}

/// Whether an assignment target is, or unpacks into, `__getattr__`, or reaches
/// the module through `sys.modules` or `globals()`.
fn assigns_lazy_hook(target: &Expr) -> bool {
    match target {
        Expr::Name(name) => name.id.as_str() == GETATTR,
        Expr::Tuple(tuple) => tuple.elts.iter().any(assigns_lazy_hook),
        Expr::List(list) => list.elts.iter().any(assigns_lazy_hook),
        Expr::Attribute(attribute) => is_own_entry_in_sys_modules(&attribute.value),
        Expr::Subscript(subscript) => {
            is_own_entry_in_sys_modules(target)
                || (is_globals_call(&subscript.value) && is_getattr_literal(&subscript.slice))
        }
        _ => false,
    }
}

/// Whether `expression` is `<anything>.modules[__name__]`, or `modules[__name__]`
/// after `from sys import modules`.
fn is_own_entry_in_sys_modules(expression: &Expr) -> bool {
    let Expr::Subscript(subscript) = expression else {
        return false;
    };
    let own_name =
        matches!(subscript.slice.as_ref(), Expr::Name(name) if name.id.as_str() == "__name__");
    let modules = match subscript.value.as_ref() {
        Expr::Attribute(attribute) => attribute.attr.as_str() == "modules",
        Expr::Name(name) => name.id.as_str() == "modules",
        _ => false,
    };
    own_name && modules
}

/// Whether `expression` is a call to `globals()`.
fn is_globals_call(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Call(call) if matches!(call.func.as_ref(), Expr::Name(name) if name.id.as_str() == "globals")
    )
}

/// Whether `expression` is the string literal `"__getattr__"`.
fn is_getattr_literal(expression: &Expr) -> bool {
    matches!(
        expression,
        Expr::Constant(constant) if matches!(&constant.value, Constant::Str(text) if text == GETATTR)
    )
}

/// Whether `expression` is `setattr(module, "__getattr__", hook)`.
fn sets_hook_by_call(expression: &Expr) -> bool {
    let Expr::Call(call) = expression else {
        return false;
    };
    matches!(call.func.as_ref(), Expr::Name(name) if name.id.as_str() == "setattr")
        && call.args.get(1).is_some_and(is_getattr_literal)
}
