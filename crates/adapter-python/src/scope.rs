//! Local-name discovery for Python scopes.
//!
//! A name bound inside a function (parameter, assignment target, loop or
//! `with` target, nested definition, exception or pattern capture) is a local,
//! not a reference to a module-level declaration. `global` names resolve at
//! module level and `nonlocal` names in the enclosing function, so neither is
//! bound in the declaring scope. Function-local imports are deliberately not
//! locals: the parser records only module-level imports, so such a name must
//! stay a reference for the binder to resolve it through the module's table.
//! Class bodies are scopes whose names are invisible to nested functions.

use std::collections::HashSet;

use rustpython_parser::ast::{Arguments, ExceptHandler, Expr, Pattern, Stmt};
use smol_str::SmolStr;

/// What kind of Python scope a [`Scope`] models.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    /// A `def` or `lambda`: a barrier that hides enclosing class scopes.
    Function,
    /// A class body: visible only to code directly in the body.
    Class,
    /// A comprehension: transparent for class visibility and for walrus targets.
    Comprehension,
}

/// The names one scope binds, and the names it declares `global`.
pub struct Scope {
    /// Which kind of scope this is.
    pub kind: ScopeKind,
    /// Locally bound names.
    pub names: HashSet<SmolStr>,
    /// Names declared `global` in this scope; they resolve at module level.
    pub globals: HashSet<SmolStr>,
    /// Names declared `nonlocal` in this scope; they bind in an enclosing one.
    nonlocals: HashSet<SmolStr>,
}

/// `global` / `nonlocal` declarations found in a scope body.
#[derive(Default)]
pub struct Declared {
    globals: HashSet<SmolStr>,
    nonlocals: HashSet<SmolStr>,
}

impl Scope {
    /// A scope with no `global` declarations.
    pub fn plain(kind: ScopeKind, names: HashSet<SmolStr>) -> Self {
        Self {
            kind,
            names,
            globals: HashSet::new(),
            nonlocals: HashSet::new(),
        }
    }

    /// A function scope: parameters plus body bindings, minus `global` and
    /// `nonlocal` names.
    pub fn function(arguments: &Arguments, body: &[Stmt]) -> Self {
        Self::from_body(ScopeKind::Function, argument_names(arguments), body)
    }

    /// An empty class-body scope; its names are added in execution order by
    /// [`Scope::bind_after`].
    pub fn class() -> Self {
        Self::plain(ScopeKind::Class, HashSet::new())
    }

    /// Adds the names `statement` binds, once it has run. A class body binds
    /// names in execution order, so `helper = helper` still reads the outer
    /// `helper` on its right-hand side.
    pub fn bind_after(&mut self, statement: &Stmt) {
        let mut declared = Declared::default();
        statement_bindings(statement, &mut self.names, &mut declared);
        self.globals.extend(declared.globals);
        self.nonlocals.extend(declared.nonlocals);
        let Self {
            names,
            globals,
            nonlocals,
            ..
        } = self;
        names.retain(|name| !globals.contains(name) && !nonlocals.contains(name));
    }

    fn from_body(kind: ScopeKind, mut names: HashSet<SmolStr>, body: &[Stmt]) -> Self {
        let mut declared = Declared::default();
        body_bindings(body, &mut names, &mut declared);
        names.retain(|name| !declared.globals.contains(name) && !declared.nonlocals.contains(name));
        Self {
            kind,
            names,
            globals: declared.globals,
            nonlocals: declared.nonlocals,
        }
    }
}

/// Whether `name` is bound by one of `scopes` (innermost last) as seen from the
/// innermost scope: a `global` declaration stops the search, and class scopes
/// are skipped once a function or lambda scope has been crossed.
pub fn is_bound(scopes: &[Scope], name: &str) -> bool {
    let mut crossed_function = false;
    for scope in scopes.iter().rev() {
        if scope.globals.contains(name) {
            return false;
        }
        let visible = scope.kind != ScopeKind::Class || !crossed_function;
        if visible && scope.names.contains(name) {
            return true;
        }
        crossed_function |= scope.kind == ScopeKind::Function;
    }
    false
}

/// Parameter names of a `def` or `lambda`.
pub fn argument_names(arguments: &Arguments) -> HashSet<SmolStr> {
    let mut names: HashSet<SmolStr> = arguments
        .posonlyargs
        .iter()
        .chain(&arguments.args)
        .chain(&arguments.kwonlyargs)
        .map(|argument| SmolStr::new(argument.def.arg.as_str()))
        .collect();
    for extra in [&arguments.vararg, &arguments.kwarg].into_iter().flatten() {
        names.insert(SmolStr::new(extra.arg.as_str()));
    }
    names
}

/// Adds the simple names an assignment-like target binds (unpacking included).
pub fn target_names(target: &Expr, out: &mut HashSet<SmolStr>) {
    match target {
        Expr::Name(name) => {
            out.insert(SmolStr::new(name.id.as_str()));
        }
        Expr::Tuple(tuple) => tuple.elts.iter().for_each(|e| target_names(e, out)),
        Expr::List(list) => list.elts.iter().for_each(|e| target_names(e, out)),
        Expr::Starred(starred) => target_names(&starred.value, out),
        _ => {}
    }
}

/// Adds the names a `match` pattern captures.
pub fn pattern_names(pattern: &Pattern, out: &mut HashSet<SmolStr>) {
    match pattern {
        Pattern::MatchAs(capture) => {
            if let Some(name) = &capture.name {
                out.insert(SmolStr::new(name.as_str()));
            }
            if let Some(inner) = &capture.pattern {
                pattern_names(inner, out);
            }
        }
        Pattern::MatchStar(star) => {
            if let Some(name) = &star.name {
                out.insert(SmolStr::new(name.as_str()));
            }
        }
        Pattern::MatchSequence(sequence) => {
            sequence.patterns.iter().for_each(|p| pattern_names(p, out));
        }
        Pattern::MatchOr(alternatives) => {
            alternatives
                .patterns
                .iter()
                .for_each(|p| pattern_names(p, out));
        }
        Pattern::MatchClass(class) => {
            let nested = class.patterns.iter().chain(&class.kwd_patterns);
            nested.for_each(|p| pattern_names(p, out));
        }
        Pattern::MatchMapping(mapping) => {
            mapping.patterns.iter().for_each(|p| pattern_names(p, out));
            if let Some(rest) = &mapping.rest {
                out.insert(SmolStr::new(rest.as_str()));
            }
        }
        Pattern::MatchValue(_) | Pattern::MatchSingleton(_) => {}
    }
}

/// Collects bindings made by `body` (not descending into nested scopes) into
/// `locals`, and `global` / `nonlocal` declarations into `declared_outer`.
fn body_bindings(body: &[Stmt], locals: &mut HashSet<SmolStr>, declared_outer: &mut Declared) {
    for statement in body {
        statement_bindings(statement, locals, declared_outer);
    }
}

/// Collects the bindings of one statement and its nested blocks.
fn statement_bindings(
    statement: &Stmt,
    locals: &mut HashSet<SmolStr>,
    declared_outer: &mut Declared,
) {
    match statement {
        Stmt::Assign(assign) => assign.targets.iter().for_each(|t| target_names(t, locals)),
        Stmt::AugAssign(assign) => target_names(&assign.target, locals),
        Stmt::AnnAssign(assign) => target_names(&assign.target, locals),
        Stmt::For(node) => {
            target_names(&node.target, locals);
            body_bindings(&node.body, locals, declared_outer);
            body_bindings(&node.orelse, locals, declared_outer);
        }
        Stmt::AsyncFor(node) => {
            target_names(&node.target, locals);
            body_bindings(&node.body, locals, declared_outer);
            body_bindings(&node.orelse, locals, declared_outer);
        }
        Stmt::While(node) => {
            body_bindings(&node.body, locals, declared_outer);
            body_bindings(&node.orelse, locals, declared_outer);
        }
        Stmt::If(node) => {
            body_bindings(&node.body, locals, declared_outer);
            body_bindings(&node.orelse, locals, declared_outer);
        }
        Stmt::With(node) => {
            for item in node.items.iter().filter_map(|i| i.optional_vars.as_deref()) {
                target_names(item, locals);
            }
            body_bindings(&node.body, locals, declared_outer);
        }
        Stmt::AsyncWith(node) => {
            for item in node.items.iter().filter_map(|i| i.optional_vars.as_deref()) {
                target_names(item, locals);
            }
            body_bindings(&node.body, locals, declared_outer);
        }
        Stmt::Try(node) => {
            body_bindings(&node.body, locals, declared_outer);
            body_bindings(&node.orelse, locals, declared_outer);
            body_bindings(&node.finalbody, locals, declared_outer);
            handler_bindings(&node.handlers, locals, declared_outer);
        }
        Stmt::TryStar(node) => {
            body_bindings(&node.body, locals, declared_outer);
            body_bindings(&node.orelse, locals, declared_outer);
            body_bindings(&node.finalbody, locals, declared_outer);
            handler_bindings(&node.handlers, locals, declared_outer);
        }
        Stmt::Match(node) => {
            for case in &node.cases {
                pattern_names(&case.pattern, locals);
                body_bindings(&case.body, locals, declared_outer);
            }
        }
        Stmt::FunctionDef(node) => {
            locals.insert(SmolStr::new(node.name.as_str()));
        }
        Stmt::AsyncFunctionDef(node) => {
            locals.insert(SmolStr::new(node.name.as_str()));
        }
        Stmt::ClassDef(node) => {
            locals.insert(SmolStr::new(node.name.as_str()));
        }
        Stmt::Global(node) => {
            declared_outer
                .globals
                .extend(node.names.iter().map(|n| SmolStr::new(n.as_str())));
        }
        Stmt::Nonlocal(node) => {
            declared_outer
                .nonlocals
                .extend(node.names.iter().map(|n| SmolStr::new(n.as_str())));
        }
        _ => {}
    }
}

/// Collects exception-handler names and the bindings inside handler bodies.
fn handler_bindings(
    handlers: &[ExceptHandler],
    locals: &mut HashSet<SmolStr>,
    declared_outer: &mut Declared,
) {
    for handler in handlers {
        let ExceptHandler::ExceptHandler(handler) = handler;
        if let Some(name) = &handler.name {
            locals.insert(SmolStr::new(name.as_str()));
        }
        body_bindings(&handler.body, locals, declared_outer);
    }
}
