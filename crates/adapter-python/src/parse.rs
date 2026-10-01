//! rustpython parsing of `.py` sources into serializable [`ParsedModule`]s.
//!
//! Parsing runs in parallel across files (one rayon task per file). A syntax
//! error is surfaced as [`AdapterError::Parse`] carrying the offending path and
//! a line/column-derived reason — files are never skipped silently.
//!
//! The extractor walks only the module's top level: every `def` / `async def`
//! function, every `class`, and every module-level constant binding becomes a
//! [`Declaration`], and module-level `import` / `from import` statements become
//! [`Import`]s. References, calls, annotation types, and base classes are
//! collected from each declaration's body so the binder can resolve them
//! through the scope chain.

mod collector;
mod declaration;
mod expressions;
mod imports;
mod statements;

use rayon::prelude::*;
use rustpython_parser::Parse;
use rustpython_parser::ast::Stmt;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use strata_ir::{AdapterError, SourceFile};

use self::declaration::{
    async_function_declaration, class_declaration, constant_declarations, function_declaration,
};
use self::imports::{collect_dunder_all, from_import, plain_import};

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

/// A top-level declaration (function, class, or constant) with its production
/// SLOC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    /// Source-declared name of the function, class, or constant.
    pub name: SmolStr,
    /// Whether the declaration is a class (`true`) or a value symbol — a
    /// function or a module-level constant (`false`).
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
    /// Top-level functions, classes, and constant bindings in source order.
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
                declarations.extend(constant_declarations(
                    &assign.targets,
                    Some(&assign.value),
                    None,
                    statement,
                    source,
                ));
            }
            Stmt::AnnAssign(annotation) => {
                declarations.extend(constant_declarations(
                    std::slice::from_ref(annotation.target.as_ref()),
                    annotation.value.as_deref(),
                    Some(&annotation.annotation),
                    statement,
                    source,
                ));
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
    fn should_extract_a_module_level_constant_as_a_declaration() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "_ledger = []\n")?;

        let names: Vec<&str> = module
            .declarations
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect();
        assert_eq!(names, vec!["_ledger"]);
        let first = first_declaration(&module)?;
        assert!(!first.is_class);
        assert!(first.sloc >= 1);
        Ok(())
    }

    #[test]
    fn should_collect_the_initializer_calls_of_a_constant() -> Result<(), AdapterError> {
        let module = parse_source(
            "pkg/mod.py",
            "def build(limit):\n    return limit\n\n\n_cache = build(8)\n",
        )?;

        let cache = module
            .declarations
            .iter()
            .find(|declaration| declaration.name == "_cache")
            .ok_or_else(|| AdapterError::Bind {
                path: module.path.clone(),
                reason: "no _cache declaration".to_string(),
            })?;
        assert_eq!(cache.called, vec![SmolStr::new("build")]);
        Ok(())
    }

    #[test]
    fn should_collect_the_annotation_of_an_annotated_constant() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "limit: MaxSize = MAX\n")?;

        let first = first_declaration(&module)?;
        assert_eq!(first.name, SmolStr::new("limit"));
        assert_eq!(first.annotations, vec![SmolStr::new("MaxSize")]);
        assert_eq!(first.referenced, vec![SmolStr::new("MAX")]);
        Ok(())
    }

    #[test]
    fn should_mint_one_declaration_per_tuple_unpack_target() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "lo, hi = 0, 100\n")?;

        let names: Vec<&str> = module
            .declarations
            .iter()
            .map(|declaration| declaration.name.as_str())
            .collect();
        assert_eq!(names, vec!["lo", "hi"]);
        Ok(())
    }

    #[test]
    fn should_not_mint_a_symbol_for_dunder_all() -> Result<(), AdapterError> {
        let module = parse_source("pkg/__init__.py", "__all__ = [\"a\", \"b\"]\n")?;

        assert!(module.declarations.is_empty());
        assert_eq!(
            module.dunder_all,
            vec![SmolStr::new("a"), SmolStr::new("b")]
        );
        Ok(())
    }

    #[test]
    fn should_skip_augmented_assignment_and_bare_annotations() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "total += 1\ncount: int\n")?;

        assert!(module.declarations.is_empty());
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

    /// Names referenced (`false`) or called (`true`) by the first declaration of
    /// `source`, which must define `f`.
    fn first_names(source: &str, called: bool) -> Result<Vec<String>, AdapterError> {
        let module = parse_source("pkg/mod.py", source)?;
        let declaration = first_declaration(&module)?;
        let names = if called {
            &declaration.called
        } else {
            &declaration.referenced
        };
        Ok(names.iter().map(ToString::to_string).collect())
    }

    /// Asserts `source` references `name` as a value in its first declaration.
    fn assert_value_use(source: &str, name: &str) -> Result<(), AdapterError> {
        let referenced = first_names(source, false)?;
        assert!(
            referenced.iter().any(|n| n == name),
            "{name} should be a value use in {source:?}: {referenced:?}"
        );
        Ok(())
    }

    #[test]
    fn should_record_a_function_used_as_a_value_in_a_lambda() -> Result<(), AdapterError> {
        assert_value_use(
            "def f(xs):\n    return sorted(xs, key=lambda x: helper)\n",
            "helper",
        )?;
        let referenced = first_names("def f():\n    return lambda x: x\n", false)?;
        assert!(
            referenced.is_empty(),
            "a lambda parameter is local: {referenced:?}"
        );
        Ok(())
    }

    #[test]
    fn should_record_value_uses_inside_comprehensions() -> Result<(), AdapterError> {
        assert_value_use("def f(xs):\n    return [helper for x in xs]\n", "helper")?;
        assert_value_use("def f(xs):\n    return {helper for x in xs}\n", "helper")?;
        assert_value_use("def f(xs):\n    return {x: helper for x in xs}\n", "helper")?;
        assert_value_use(
            "def f(xs):\n    return (helper for x in xs if x)\n",
            "helper",
        )?;
        let referenced = first_names("def f(xs):\n    return [x for x in xs]\n", false)?;
        assert!(!referenced.iter().any(|n| n == "x"), "{referenced:?}");
        Ok(())
    }

    #[test]
    fn should_record_value_uses_in_fstrings_walrus_yield_slice_and_await()
    -> Result<(), AdapterError> {
        assert_value_use("def f():\n    return f\"{helper}\"\n", "helper")?;
        assert_value_use(
            "def f():\n    if (n := helper):\n        return n\n",
            "helper",
        )?;
        assert_value_use("def f():\n    yield helper\n", "helper")?;
        assert_value_use("def f():\n    yield from helper\n", "helper")?;
        assert_value_use("def f(xs):\n    return xs[helper:]\n", "helper")?;
        let referenced = first_names("def f():\n    if (n := 1):\n        return n\n", false)?;
        assert!(
            referenced.is_empty(),
            "a walrus target is local: {referenced:?}"
        );
        Ok(())
    }

    #[test]
    fn should_record_value_uses_in_async_for_with_and_match() -> Result<(), AdapterError> {
        assert_value_use(
            "async def f(xs):\n    async for x in xs:\n        helper\n",
            "helper",
        )?;
        assert_value_use(
            "async def f(xs):\n    async with xs as c:\n        helper\n",
            "helper",
        )?;
        assert_value_use(
            "def f(v):\n    match v:\n        case 1:\n            helper\n",
            "helper",
        )?;
        let referenced = first_names(
            "def f(v):\n    match v:\n        case [a, b]:\n            return a\n",
            false,
        )?;
        assert!(!referenced.iter().any(|n| n == "a"), "{referenced:?}");
        Ok(())
    }

    #[test]
    fn should_not_record_a_shadowed_local_as_a_reference() -> Result<(), AdapterError> {
        let referenced = first_names(
            "def f(arg):\n    helper = 1\n    return helper + arg\n",
            false,
        )?;
        assert!(referenced.is_empty(), "{referenced:?}");
        let called = first_names("def f():\n    helper = make()\n    return helper()\n", true)?;
        assert!(!called.iter().any(|n| n == "helper"), "{called:?}");
        Ok(())
    }

    #[test]
    fn should_record_a_class_body_rebinding_of_a_module_name() -> Result<(), AdapterError> {
        // A class body reads the module name before its own binding lands.
        assert_value_use("class C:\n    helper = helper\n", "helper")
    }

    #[test]
    fn should_bind_a_class_body_loop_target_inside_its_loop() -> Result<(), AdapterError> {
        let referenced = first_names(
            "class C:\n    for item in range(3):\n        y = item\n",
            false,
        )?;
        assert!(!referenced.iter().any(|n| n == "item"), "{referenced:?}");
        Ok(())
    }

    #[test]
    fn should_bind_a_class_body_name_inside_its_own_if_block() -> Result<(), AdapterError> {
        let referenced = first_names(
            "class C:\n    if cond:\n        x = 1\n        y = x\n",
            false,
        )?;
        assert!(!referenced.iter().any(|n| n == "x"), "{referenced:?}");
        assert!(referenced.iter().any(|n| n == "cond"), "{referenced:?}");
        Ok(())
    }

    #[test]
    fn should_bind_class_body_with_and_except_targets() -> Result<(), AdapterError> {
        let referenced = first_names(
            "class C:\n    with open(p) as fh:\n        a = fh\n    try:\n        pass\n    except E as err:\n        b = err\n",
            false,
        )?;
        assert!(
            !referenced.iter().any(|n| n == "fh" || n == "err"),
            "{referenced:?}"
        );
        Ok(())
    }

    #[test]
    fn should_keep_a_global_name_bound_at_module_level() -> Result<(), AdapterError> {
        assert_value_use(
            "def f():\n    global helper\n    helper = 1\n    return helper\n",
            "helper",
        )
    }

    #[test]
    fn should_let_an_inner_global_win_over_an_enclosing_binding() -> Result<(), AdapterError> {
        assert_value_use(
            "def f():\n    helper = 1\n    def g():\n        global helper\n        return helper\n    return g\n",
            "helper",
        )
    }

    #[test]
    fn should_resolve_a_nonlocal_name_to_the_enclosing_binding() -> Result<(), AdapterError> {
        let referenced = first_names(
            "def f():\n    helper = 0\n    def g():\n        nonlocal helper\n        helper = 1\n        return helper\n    return g\n",
            false,
        )?;
        assert!(!referenced.iter().any(|n| n == "helper"), "{referenced:?}");
        Ok(())
    }

    #[test]
    fn should_not_record_a_class_body_binding_as_a_reference() -> Result<(), AdapterError> {
        let referenced = first_names("class C:\n    helper = 1\n    alias = helper\n", false)?;
        assert!(!referenced.iter().any(|n| n == "helper"), "{referenced:?}");
        Ok(())
    }

    #[test]
    fn should_not_expose_class_scope_to_a_method() -> Result<(), AdapterError> {
        assert_value_use(
            "class C:\n    helper = 1\n    def m(self):\n        return helper\n",
            "helper",
        )
    }

    #[test]
    fn should_walk_the_first_comprehension_iterable_in_the_enclosing_scope()
    -> Result<(), AdapterError> {
        assert_value_use(
            "def f():\n    return [helper for helper in helper]\n",
            "helper",
        )
    }

    #[test]
    fn should_bind_a_comprehension_walrus_in_the_enclosing_function() -> Result<(), AdapterError> {
        let referenced = first_names(
            "def f(xs):\n    [n := x for x in xs]\n    return n\n",
            false,
        )?;
        assert!(!referenced.iter().any(|n| n == "n"), "{referenced:?}");
        Ok(())
    }

    #[test]
    fn should_keep_a_function_local_import_name_as_a_reference() -> Result<(), AdapterError> {
        let called = first_names(
            "def f():\n    from .m import helper\n    return helper()\n",
            true,
        )?;
        assert!(called.iter().any(|n| n == "helper"), "{called:?}");
        Ok(())
    }
}
