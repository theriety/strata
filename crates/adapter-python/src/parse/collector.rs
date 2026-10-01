//! The reference collector: names, calls, annotations, and dynamic constructs.
//!
//! The collector owns the scope stack (see [`crate::scope`]) while the
//! statement and expression walkers feed it.

use std::collections::HashSet;

use rustpython_parser::ast::{Constant, Expr, Stmt, StmtAnnAssign};
use smol_str::SmolStr;

use super::DynamicRef;
use super::expressions::{collect_child_expressions_of_expr, leading_name};
use super::statements::collect_child_expressions;
use crate::scope::{Scope, ScopeKind, is_bound, target_names};

/// Collects the annotation references carried by a function's arguments.
pub(super) fn collect_arguments(
    arguments: &rustpython_parser::ast::Arguments,
    collector: &mut ReferenceCollector,
) {
    let positional = arguments
        .posonlyargs
        .iter()
        .chain(&arguments.args)
        .chain(&arguments.kwonlyargs);
    for argument in positional {
        if let Some(annotation) = &argument.def.annotation {
            collector.collect_annotation(annotation);
        }
    }
    if let Some(vararg) = &arguments.vararg
        && let Some(annotation) = &vararg.annotation
    {
        collector.collect_annotation(annotation);
    }
    if let Some(kwarg) = &arguments.kwarg
        && let Some(annotation) = &kwarg.annotation
    {
        collector.collect_annotation(annotation);
    }
}

/// Walks a list of body statements, feeding each into the collector.
pub(super) fn collect_body(body: &[Stmt], collector: &mut ReferenceCollector) {
    for statement in body {
        collector.collect_statement(statement);
        collector.bind_class_statement(statement);
    }
}

/// Collects referenced names, calls, annotations, and dynamic constructs from a
/// single top-level declaration's body. Nested `def` / `class` bodies are also
/// walked so a dependency reached through a closure is not missed.
#[derive(Default)]
pub(super) struct ReferenceCollector {
    /// Identifiers referenced (non-call, non-annotation).
    pub(super) referenced: Vec<SmolStr>,
    /// Identifiers invoked as a call target.
    pub(super) called: Vec<SmolStr>,
    /// Identifiers referenced in annotation positions.
    pub(super) annotations: Vec<SmolStr>,
    /// Dynamic-resolution constructs (`getattr`, `importlib`).
    pub(super) dynamic: Vec<DynamicRef>,
    /// Enclosing function, lambda, and comprehension scopes, innermost last;
    /// a name bound in any of them is a local, not a declaration reference.
    pub(super) scopes: Vec<Scope>,
}

impl ReferenceCollector {
    /// Whether `name` is bound by an enclosing local scope.
    fn is_local(&self, name: &str) -> bool {
        is_bound(&self.scopes, name)
    }

    /// Binds `names` in the innermost non-comprehension scope (PEP 572 walrus
    /// targets leak out of a comprehension), opening one when none exists.
    pub(super) fn bind_locals(&mut self, names: HashSet<SmolStr>) {
        match self
            .scopes
            .iter_mut()
            .rev()
            .find(|scope| scope.kind != ScopeKind::Comprehension)
        {
            Some(scope) => scope.names.extend(names),
            None => self.scopes.push(Scope::plain(ScopeKind::Function, names)),
        }
    }

    /// Records every name appearing in an annotation expression.
    pub(super) fn collect_annotation(&mut self, expr: &Expr) {
        match expr {
            Expr::Name(name) => self.annotations.push(SmolStr::new(name.id.as_str())),
            Expr::Attribute(attribute) => {
                if let Some(name) = leading_name(&attribute.value) {
                    self.annotations.push(name);
                }
            }
            Expr::Subscript(subscript) => {
                if let Some(name) = leading_name(&subscript.value) {
                    self.annotations.push(name);
                }
                self.collect_annotation(&subscript.slice);
            }
            Expr::Tuple(tuple) => {
                for element in &tuple.elts {
                    self.collect_annotation(element);
                }
            }
            Expr::List(list) => {
                for element in &list.elts {
                    self.collect_annotation(element);
                }
            }
            _ => {}
        }
    }

    /// Walks a class body in its own scope. Class names bind in execution
    /// order, not up front, so `helper = helper` still reads the outer name.
    pub(super) fn collect_class_body(&mut self, body: &[Stmt]) {
        self.scopes.push(Scope::class());
        collect_body(body, self);
        self.scopes.pop();
    }

    /// In a class scope, binds what `statement` bound once it has run, so the
    /// name is local only to the statements after it, at every nesting level.
    fn bind_class_statement(&mut self, statement: &Stmt) {
        if let Some(scope) = self
            .scopes
            .last_mut()
            .filter(|s| s.kind == ScopeKind::Class)
        {
            scope.bind_after(statement);
        }
    }

    /// Binds a loop or `with` target before its body runs; in a class scope
    /// the target is otherwise unbound inside that body.
    pub(super) fn bind_target(&mut self, target: &Expr) {
        let mut names = HashSet::new();
        target_names(target, &mut names);
        self.bind_locals(names);
    }

    /// Walks a statement, dispatching annotations, expressions, and nested bodies.
    fn collect_statement(&mut self, statement: &Stmt) {
        match statement {
            Stmt::AnnAssign(annotation) => self.collect_ann_assign(annotation),
            Stmt::FunctionDef(function) => {
                collect_arguments(&function.args, self);
                if let Some(returns) = &function.returns {
                    self.collect_annotation(returns);
                }
                self.scopes
                    .push(Scope::function(&function.args, &function.body));
                collect_body(&function.body, self);
                self.scopes.pop();
            }
            Stmt::AsyncFunctionDef(function) => {
                collect_arguments(&function.args, self);
                if let Some(returns) = &function.returns {
                    self.collect_annotation(returns);
                }
                self.scopes
                    .push(Scope::function(&function.args, &function.body));
                collect_body(&function.body, self);
                self.scopes.pop();
            }
            Stmt::ClassDef(class) => {
                for base in &class.bases {
                    self.collect_expression(base);
                }
                self.collect_class_body(&class.body);
            }
            other => collect_child_expressions(other, self),
        }
    }

    /// Records the annotation and the assigned value of an `x: T = v` statement.
    fn collect_ann_assign(&mut self, annotation: &StmtAnnAssign) {
        self.collect_annotation(&annotation.annotation);
        if let Some(value) = &annotation.value {
            self.collect_expression(value);
        }
    }

    /// Walks an expression, recording calls, names, and dynamic constructs.
    pub(super) fn collect_expression(&mut self, expr: &Expr) {
        match expr {
            Expr::Call(call) => self.collect_call(call),
            Expr::Name(name) => {
                if !self.is_local(name.id.as_str()) {
                    self.referenced.push(SmolStr::new(name.id.as_str()));
                }
            }
            _ => collect_child_expressions_of_expr(expr, self),
        }
    }

    /// Records a call: either a dynamic construct or a hard call edge target.
    fn collect_call(&mut self, call: &rustpython_parser::ast::ExprCall) {
        if let Some(dynamic) = dynamic_call(call) {
            self.dynamic.push(dynamic);
        } else if let Expr::Name(name) = call.func.as_ref() {
            if !self.is_local(name.id.as_str()) {
                self.called.push(SmolStr::new(name.id.as_str()));
            }
        } else {
            self.collect_expression(&call.func);
        }
        for argument in &call.args {
            self.collect_expression(argument);
        }
        for keyword in &call.keywords {
            self.collect_expression(&keyword.value);
        }
    }
}

/// Detects a dynamic-resolution call (`getattr(...)` / `importlib.import_module(...)`).
fn dynamic_call(call: &rustpython_parser::ast::ExprCall) -> Option<DynamicRef> {
    match call.func.as_ref() {
        Expr::Name(name) if name.id.as_str() == "getattr" => call
            .args
            .first()
            .and_then(leading_name)
            .map(DynamicRef::GetAttr),
        Expr::Attribute(attribute) if attribute.attr.as_str() == "import_module" => {
            string_argument(call).map(DynamicRef::ImportModule)
        }
        _ => None,
    }
}

/// Extracts the first string-literal argument of a call, if present.
fn string_argument(call: &rustpython_parser::ast::ExprCall) -> Option<SmolStr> {
    match call.args.first() {
        Some(Expr::Constant(constant)) => match &constant.value {
            Constant::Str(text) => Some(SmolStr::new(text.as_str())),
            _ => None,
        },
        _ => None,
    }
}
