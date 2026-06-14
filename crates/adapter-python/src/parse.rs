//! rustpython parsing of `.py` sources into serializable [`ParsedModule`]s.
//!
//! Parsing runs in parallel across files (one rayon task per file). A syntax
//! error is surfaced as [`AdapterError::Parse`] carrying the offending path and
//! a line/column-derived reason — files are never skipped silently.
//!
//! The extractor walks only the module's top level: every `def` / `async def`
//! function and every `class` becomes a [`Declaration`], and module-level
//! `import` / `from import` statements become [`Import`]s. References, calls,
//! annotation types, and base classes are collected from each declaration's
//! body so the binder can resolve them through the scope chain.

use rayon::prelude::*;
use rustpython_parser::Parse;
use rustpython_parser::ast::{
    Alias, Constant, Expr, Ranged, Stmt, StmtAnnAssign, StmtAsyncFunctionDef, StmtClassDef,
    StmtFunctionDef,
};
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use strata_ir::{AdapterError, SourceFile};

use crate::sloc::production_sloc;

/// A dynamic-resolution construct that yields a confidence below `1.0`.
///
/// Python's dynamic surface cannot be resolved statically; the binder still
/// records honest, low-confidence edges rather than dropping the dependency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DynamicRef {
    /// A `getattr(obj, "name")` access on a module-level name.
    GetAttr(SmolStr),
    /// An `importlib.import_module("pkg")` style dynamic import.
    ImportModule(SmolStr),
}

/// A top-level declaration (function or class) with its production SLOC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    /// Source-declared name of the function or class.
    pub name: SmolStr,
    /// Whether the declaration is a class (`true`) or a function (`false`).
    pub is_class: bool,
    /// Production SLOC attributed to this declaration (docstrings excluded).
    pub sloc: u32,
    /// Names this class inherits from (drives inheritance edges).
    pub bases: Vec<SmolStr>,
    /// Identifiers referenced in annotation positions (params, returns, vars).
    pub annotations: Vec<SmolStr>,
    /// Identifiers referenced (non-call, non-annotation) within the body.
    pub referenced: Vec<SmolStr>,
    /// Identifiers invoked as a call within the body (drives call edges).
    pub called: Vec<SmolStr>,
    /// Dynamic-resolution constructs found within the body.
    pub dynamic: Vec<DynamicRef>,
}

/// A statement importing one or more names into a module.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Import {
    /// Dotted module path the names come from.
    ///
    /// For `import a.b.c` this is `a.b.c`; for `from a.b import x` it is `a.b`.
    /// For a bare relative `from . import x` it is empty.
    pub module: SmolStr,
    /// Number of leading dots for a relative import (`0` for absolute).
    pub level: u32,
    /// Locally bound names brought in (the `asname` when aliased, else the
    /// imported name). Empty marks a star import (`from m import *`).
    pub names: Vec<SmolStr>,
    /// The original (pre-`as`) names imported from `module`, name-aligned with
    /// `names`. For `import a.b` the bound name is `a` but the target is `a.b`.
    pub targets: Vec<SmolStr>,
    /// `true` for `from module import *`.
    pub star: bool,
}

/// A serializable, language-agnostic summary of one parsed module.
///
/// This is the payload the adapter threads through [`strata_ir::ParseTree`]:
/// [`parse`] produces it, [`crate::bind::bind`] consumes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedModule {
    /// Repository-relative path of the source file.
    pub path: SmolStr,
    /// Top-level functions and classes in source order.
    pub declarations: Vec<Declaration>,
    /// Module-level import statements.
    pub imports: Vec<Import>,
    /// Names listed in a module-level `__all__` assignment (drives the public
    /// surface a star import re-exports).
    pub dunder_all: Vec<SmolStr>,
}

/// Parses Python sources into [`ParsedModule`]s, one rayon task per file.
///
/// # Errors
///
/// Returns [`AdapterError::Parse`] for the first file (in input order) that
/// contains a syntax error; the reason embeds the line and column of the failure.
pub fn parse(files: &[SourceFile]) -> Result<Vec<ParsedModule>, AdapterError> {
    files.par_iter().map(parse_one).collect()
}

/// Parses a single source file into a [`ParsedModule`].
fn parse_one(file: &SourceFile) -> Result<ParsedModule, AdapterError> {
    let suite = rustpython_parser::ast::Suite::parse(&file.contents, file.path.as_str()).map_err(
        |error| AdapterError::Parse {
            path: file.path.clone(),
            reason: format_parse_error(&error, &file.contents),
        },
    )?;
    Ok(extract(&file.path, &suite, &file.contents))
}

/// Renders a parse error as a `line L, column C: message` string.
///
/// The line and column are derived from the byte offset the parser reports,
/// counting `\n` boundaries up to that point (both one-indexed).
fn format_parse_error(error: &rustpython_parser::ParseError, source: &str) -> String {
    let offset = usize::from(error.offset);
    let consumed = source.get(..offset).unwrap_or(source);
    let line = consumed.bytes().filter(|&byte| byte == b'\n').count() + 1;
    let column = consumed.len() - consumed.rfind('\n').map_or(0, |index| index + 1) + 1;
    format!("line {line}, column {column}: {}", error.error)
}

/// Walks the module's top-level statements into a [`ParsedModule`].
fn extract(path: &SmolStr, suite: &[Stmt], source: &str) -> ParsedModule {
    let mut declarations = Vec::new();
    let mut imports = Vec::new();
    let mut dunder_all = Vec::new();

    for statement in suite {
        match statement {
            Stmt::FunctionDef(function) => {
                declarations.push(function_declaration(function, source));
            }
            Stmt::AsyncFunctionDef(function) => {
                declarations.push(async_function_declaration(function, source));
            }
            Stmt::ClassDef(class) => declarations.push(class_declaration(class, source)),
            Stmt::Import(import) => imports.push(plain_import(&import.names)),
            Stmt::ImportFrom(from) => imports.push(from_import(from)),
            Stmt::Assign(assign) => {
                collect_dunder_all(&assign.targets, &assign.value, &mut dunder_all);
            }
            _ => {}
        }
    }

    ParsedModule {
        path: path.clone(),
        declarations,
        imports,
        dunder_all,
    }
}

/// Builds a [`Declaration`] for a synchronous function definition.
fn function_declaration(function: &StmtFunctionDef, source: &str) -> Declaration {
    let mut collector = ReferenceCollector::default();
    collect_arguments(&function.args, &mut collector);
    if let Some(returns) = &function.returns {
        collector.collect_annotation(returns);
    }
    collect_body(&function.body, &mut collector);
    Declaration {
        name: SmolStr::new(function.name.as_str()),
        is_class: false,
        sloc: body_sloc(&function.body, source),
        bases: Vec::new(),
        annotations: collector.annotations,
        referenced: collector.referenced,
        called: collector.called,
        dynamic: collector.dynamic,
    }
}

/// Builds a [`Declaration`] for an `async def` function definition.
fn async_function_declaration(function: &StmtAsyncFunctionDef, source: &str) -> Declaration {
    let mut collector = ReferenceCollector::default();
    collect_arguments(&function.args, &mut collector);
    if let Some(returns) = &function.returns {
        collector.collect_annotation(returns);
    }
    collect_body(&function.body, &mut collector);
    Declaration {
        name: SmolStr::new(function.name.as_str()),
        is_class: false,
        sloc: body_sloc(&function.body, source),
        bases: Vec::new(),
        annotations: collector.annotations,
        referenced: collector.referenced,
        called: collector.called,
        dynamic: collector.dynamic,
    }
}

/// Builds a [`Declaration`] for a class definition, capturing its base classes.
fn class_declaration(class: &StmtClassDef, source: &str) -> Declaration {
    let mut collector = ReferenceCollector::default();
    let bases = class.bases.iter().filter_map(leading_name).collect();
    collect_body(&class.body, &mut collector);
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

/// Builds an [`Import`] from a plain `import a, b.c as d` statement.
fn plain_import(aliases: &[Alias]) -> Import {
    let mut names = Vec::new();
    let mut targets = Vec::new();
    for alias in aliases {
        let dotted = alias.name.as_str();
        // `import a.b.c` binds `a`; `import a.b.c as d` binds `d`.
        let bound = alias.asname.as_ref().map_or_else(
            || SmolStr::new(dotted.split('.').next().unwrap_or(dotted)),
            |asname| SmolStr::new(asname.as_str()),
        );
        names.push(bound);
        targets.push(SmolStr::new(dotted));
    }
    Import {
        module: SmolStr::new(""),
        level: 0,
        names,
        targets,
        star: false,
    }
}

/// Builds an [`Import`] from a `from module import x, y as z` statement.
fn from_import(from: &rustpython_parser::ast::StmtImportFrom) -> Import {
    let module = from
        .module
        .as_ref()
        .map_or_else(|| SmolStr::new(""), |module| SmolStr::new(module.as_str()));
    let level = from
        .level
        .as_ref()
        .map_or(0, rustpython_parser::ast::Int::to_u32);
    let star = from.names.iter().any(|alias| alias.name.as_str() == "*");
    let mut names = Vec::new();
    let mut targets = Vec::new();
    if !star {
        for alias in &from.names {
            let original = SmolStr::new(alias.name.as_str());
            let bound = alias
                .asname
                .as_ref()
                .map_or_else(|| original.clone(), |asname| SmolStr::new(asname.as_str()));
            names.push(bound);
            targets.push(original);
        }
    }
    Import {
        module,
        level,
        names,
        targets,
        star,
    }
}

/// Records the string entries of a module-level `__all__ = [...]` assignment.
fn collect_dunder_all(targets: &[Expr], value: &Expr, out: &mut Vec<SmolStr>) {
    let assigns_dunder_all = targets
        .iter()
        .any(|target| matches!(target, Expr::Name(name) if name.id.as_str() == "__all__"));
    if !assigns_dunder_all {
        return;
    }
    let elements = match value {
        Expr::List(list) => &list.elts,
        Expr::Tuple(tuple) => &tuple.elts,
        _ => return,
    };
    for element in elements {
        if let Expr::Constant(constant) = element
            && let Constant::Str(text) = &constant.value
        {
            out.push(SmolStr::new(text.as_str()));
        }
    }
}

/// Resolves the leading identifier name of an expression used as a base class
/// or annotation (`Shape` from `Shape`, `mod.Shape`, or `Shape[int]`).
fn leading_name(expr: &Expr) -> Option<SmolStr> {
    match expr {
        Expr::Name(name) => Some(SmolStr::new(name.id.as_str())),
        Expr::Attribute(attribute) => leading_name(&attribute.value),
        Expr::Subscript(subscript) => leading_name(&subscript.value),
        _ => None,
    }
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

/// Collects the annotation references carried by a function's arguments.
fn collect_arguments(
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
fn collect_body(body: &[Stmt], collector: &mut ReferenceCollector) {
    for statement in body {
        collector.collect_statement(statement);
    }
}

/// Collects referenced names, calls, annotations, and dynamic constructs from a
/// single top-level declaration's body. Nested `def` / `class` bodies are also
/// walked so a dependency reached through a closure is not missed.
#[derive(Default)]
struct ReferenceCollector {
    /// Identifiers referenced (non-call, non-annotation).
    referenced: Vec<SmolStr>,
    /// Identifiers invoked as a call target.
    called: Vec<SmolStr>,
    /// Identifiers referenced in annotation positions.
    annotations: Vec<SmolStr>,
    /// Dynamic-resolution constructs (`getattr`, `importlib`).
    dynamic: Vec<DynamicRef>,
}

impl ReferenceCollector {
    /// Records every name appearing in an annotation expression.
    fn collect_annotation(&mut self, expr: &Expr) {
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

    /// Walks a statement, dispatching annotations, expressions, and nested bodies.
    fn collect_statement(&mut self, statement: &Stmt) {
        match statement {
            Stmt::AnnAssign(annotation) => self.collect_ann_assign(annotation),
            Stmt::FunctionDef(function) => {
                collect_arguments(&function.args, self);
                if let Some(returns) = &function.returns {
                    self.collect_annotation(returns);
                }
                collect_body(&function.body, self);
            }
            Stmt::AsyncFunctionDef(function) => {
                collect_arguments(&function.args, self);
                if let Some(returns) = &function.returns {
                    self.collect_annotation(returns);
                }
                collect_body(&function.body, self);
            }
            Stmt::ClassDef(class) => {
                for base in &class.bases {
                    self.collect_expression(base);
                }
                collect_body(&class.body, self);
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
    fn collect_expression(&mut self, expr: &Expr) {
        match expr {
            Expr::Call(call) => self.collect_call(call),
            Expr::Name(name) => self.referenced.push(SmolStr::new(name.id.as_str())),
            _ => collect_child_expressions_of_expr(expr, self),
        }
    }

    /// Records a call: either a dynamic construct or a hard call edge target.
    fn collect_call(&mut self, call: &rustpython_parser::ast::ExprCall) {
        if let Some(dynamic) = dynamic_call(call) {
            self.dynamic.push(dynamic);
        } else if let Expr::Name(name) = call.func.as_ref() {
            self.called.push(SmolStr::new(name.id.as_str()));
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

/// Walks the direct child expressions of a statement into the collector.
fn collect_child_expressions(statement: &Stmt, collector: &mut ReferenceCollector) {
    match statement {
        Stmt::Return(statement) => {
            if let Some(value) = &statement.value {
                collector.collect_expression(value);
            }
        }
        Stmt::Assign(assign) => collector.collect_expression(&assign.value),
        Stmt::AugAssign(assign) => collector.collect_expression(&assign.value),
        Stmt::Expr(statement) => collector.collect_expression(&statement.value),
        Stmt::If(statement) => {
            collector.collect_expression(&statement.test);
            collect_body(&statement.body, collector);
            collect_body(&statement.orelse, collector);
        }
        Stmt::While(statement) => {
            collector.collect_expression(&statement.test);
            collect_body(&statement.body, collector);
            collect_body(&statement.orelse, collector);
        }
        Stmt::For(statement) => {
            collector.collect_expression(&statement.iter);
            collect_body(&statement.body, collector);
            collect_body(&statement.orelse, collector);
        }
        Stmt::With(statement) => {
            for item in &statement.items {
                collector.collect_expression(&item.context_expr);
            }
            collect_body(&statement.body, collector);
        }
        Stmt::Raise(statement) => {
            if let Some(exception) = &statement.exc {
                collector.collect_expression(exception);
            }
        }
        Stmt::Assert(statement) => {
            collector.collect_expression(&statement.test);
            if let Some(message) = &statement.msg {
                collector.collect_expression(message);
            }
        }
        Stmt::Try(statement) => {
            collect_body(&statement.body, collector);
            collect_body(&statement.orelse, collector);
            collect_body(&statement.finalbody, collector);
            for handler in &statement.handlers {
                let rustpython_parser::ast::ExceptHandler::ExceptHandler(handler) = handler;
                collect_body(&handler.body, collector);
            }
        }
        _ => {}
    }
}

/// Walks the direct child expressions of a compound expression.
fn collect_child_expressions_of_expr(expr: &Expr, collector: &mut ReferenceCollector) {
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
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses one in-memory source file, returning its first module summary.
    ///
    /// Returns `Err` on a parse failure or an empty result so callers can use
    /// `?` and surface the reason rather than panicking.
    fn parse_source(path: &str, contents: &str) -> Result<ParsedModule, AdapterError> {
        let files = [SourceFile {
            path: SmolStr::new(path),
            contents: contents.to_string(),
        }];
        parse(&files)?
            .into_iter()
            .next()
            .ok_or_else(|| AdapterError::Parse {
                path: SmolStr::new(path),
                reason: "no module parsed".to_string(),
            })
    }

    /// Returns the first declaration of a module, or a descriptive error.
    fn first_declaration(module: &ParsedModule) -> Result<&Declaration, AdapterError> {
        module
            .declarations
            .first()
            .ok_or_else(|| AdapterError::Bind {
                path: module.path.clone(),
                reason: "no declaration".to_string(),
            })
    }

    /// Returns the first import of a module, or a descriptive error.
    fn first_import(module: &ParsedModule) -> Result<&Import, AdapterError> {
        module.imports.first().ok_or_else(|| AdapterError::Bind {
            path: module.path.clone(),
            reason: "no import".to_string(),
        })
    }

    #[test]
    fn should_extract_top_level_functions_and_classes() -> Result<(), AdapterError> {
        let module = parse_source(
            "pkg/mod.py",
            "def alpha():\n    return 1\n\n\nclass Beta:\n    pass\n",
        )?;

        let names: Vec<&str> = module
            .declarations
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect();
        assert_eq!(names, vec!["alpha", "Beta"]);
        let classes: Vec<bool> = module
            .declarations
            .iter()
            .map(|declaration| declaration.is_class)
            .collect();
        assert_eq!(classes, vec![false, true]);
        Ok(())
    }

    #[test]
    fn should_capture_class_bases() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "class Square(Shape):\n    pass\n")?;

        assert_eq!(
            first_declaration(&module)?.bases,
            vec![SmolStr::new("Shape")]
        );
        Ok(())
    }

    #[test]
    fn should_record_a_relative_from_import_with_level() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "from ..util import helper as h\n")?;

        let import = first_import(&module)?;
        assert_eq!(import.module, SmolStr::new("util"));
        assert_eq!(import.level, 2);
        assert_eq!(import.names, vec![SmolStr::new("h")]);
        assert_eq!(import.targets, vec![SmolStr::new("helper")]);
        assert!(!import.star);
        Ok(())
    }

    #[test]
    fn should_record_a_plain_dotted_import_binding_the_first_segment() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "import a.b.c\n")?;

        let import = first_import(&module)?;
        assert_eq!(import.names, vec![SmolStr::new("a")]);
        assert_eq!(import.targets, vec![SmolStr::new("a.b.c")]);
        Ok(())
    }

    #[test]
    fn should_flag_a_star_import() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "from pkg.api import *\n")?;

        let import = first_import(&module)?;
        assert!(import.star);
        assert!(import.names.is_empty());
        Ok(())
    }

    #[test]
    fn should_collect_dunder_all_entries() -> Result<(), AdapterError> {
        let module = parse_source("pkg/__init__.py", "__all__ = [\"a\", \"b\"]\n")?;

        assert_eq!(
            module.dunder_all,
            vec![SmolStr::new("a"), SmolStr::new("b")]
        );
        Ok(())
    }

    #[test]
    fn should_collect_called_names_within_a_function_body() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "def run():\n    helper()\n    other()\n")?;

        assert_eq!(
            first_declaration(&module)?.called,
            vec![SmolStr::new("helper"), SmolStr::new("other")]
        );
        Ok(())
    }

    #[test]
    fn should_collect_annotation_references() -> Result<(), AdapterError> {
        let module = parse_source(
            "pkg/mod.py",
            "def measure(shape: Shape) -> Dimensions:\n    return shape.size\n",
        )?;

        let first = first_declaration(&module)?;
        assert!(first.annotations.contains(&SmolStr::new("Shape")));
        assert!(first.annotations.contains(&SmolStr::new("Dimensions")));
        Ok(())
    }

    #[test]
    fn should_flag_getattr_as_a_dynamic_reference() -> Result<(), AdapterError> {
        let module = parse_source(
            "pkg/mod.py",
            "def run():\n    return getattr(target, \"x\")\n",
        )?;

        assert_eq!(
            first_declaration(&module)?.dynamic,
            vec![DynamicRef::GetAttr(SmolStr::new("target"))]
        );
        Ok(())
    }

    #[test]
    fn should_flag_importlib_import_module_as_dynamic() -> Result<(), AdapterError> {
        let module = parse_source(
            "pkg/mod.py",
            "def load():\n    return importlib.import_module(\"pkg.plugin\")\n",
        )?;

        assert_eq!(
            first_declaration(&module)?.dynamic,
            vec![DynamicRef::ImportModule(SmolStr::new("pkg.plugin"))]
        );
        Ok(())
    }

    #[test]
    fn should_surface_a_syntax_error_with_location() {
        let files = [SourceFile {
            path: SmolStr::new("pkg/broken.py"),
            contents: "def oops(:\n".to_string(),
        }];

        let result = parse(&files);

        assert!(
            matches!(result, Err(AdapterError::Parse { ref path, .. }) if path == "pkg/broken.py")
        );
    }
}
