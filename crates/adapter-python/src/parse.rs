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
mod dunder_all;
mod expressions;
mod imports;
mod lazy_loading;
mod statements;

use rayon::prelude::*;
use rustpython_parser::Parse;
use rustpython_parser::ast::Stmt;
use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use unlace_ir::{AdapterError, SourceFile};

use self::declaration::{
    async_function_declaration, class_declaration, constant_declarations, function_declaration,
};
use self::dunder_all::scan as scan_dunder_all;
use self::imports::{conditional_imports, from_import, plain_import};
use self::lazy_loading::sets_up_lazy_loading;

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
    /// The bound names of a plain `import a.b as x` that name the whole module
    /// `a.b`; `import a.b` binds the top package `a` and is not listed.
    pub aliases: Vec<SmolStr>,
}

/// A serializable, language-agnostic summary of one parsed module.
///
/// This is the payload the adapter threads through [`unlace_ir::ParseTree`]:
/// [`parse`] produces it, [`crate::bind::bind`] consumes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedModule {
    /// Repository-relative path of the source file.
    pub path: SmolStr,
    /// Top-level functions, classes, and constant bindings in source order.
    pub declarations: Vec<Declaration>,
    /// Module-level import statements.
    pub imports: Vec<Import>,
    /// Imports nested in a module-level `if`, `try`, `with`, loop or `match`
    /// block (the `TYPE_CHECKING` and accelerator-fallback idioms). They feed
    /// public-API detection only, never the dependency edges.
    pub conditional_imports: Vec<Import>,
    /// Whether the module sets up lazy loading: it defines, assigns or imports
    /// a module-level `__getattr__` (PEP 562), or replaces itself in
    /// `sys.modules`, also inside a conditional block.
    pub lazy_loading: bool,
    /// Names listed in a module-level `__all__` assignment (drives the public
    /// surface a star import re-exports).
    pub dunder_all: Vec<SmolStr>,
    /// How the module states `__all__`: whether `dunder_all` is the whole list.
    pub dunder_all_form: DunderAllForm,
}

/// How a module states its `__all__`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DunderAllForm {
    /// The module never mentions `__all__`.
    Absent,
    /// Every statement that sets `__all__` assigns a list or tuple of string
    /// literals, so `dunder_all` is the whole list.
    Literal,
    /// `__all__` is built or changed in a way the parser cannot read (`+=`, a
    /// comprehension, a method call, a conditional block).
    Dynamic,
}

/// Parses Python sources into [`ParsedModule`]s, one rayon task per file.
///
/// # Errors
///
/// Returns [`AdapterError::Parse`] for the first file (in input order) that
/// contains a syntax error; the reason embeds the line and column of the failure.
pub(super) fn parse(files: &[SourceFile]) -> Result<Vec<ParsedModule>, AdapterError> {
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

    let dunder_all = scan_dunder_all(suite, source);
    ParsedModule {
        path: path.clone(),
        declarations,
        imports,
        conditional_imports: conditional_imports(suite),
        lazy_loading: sets_up_lazy_loading(suite),
        dunder_all: dunder_all.names,
        dunder_all_form: dunder_all.form,
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
        assert_eq!(import.names, Vec::<SmolStr>::new());
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
    fn should_record_no_dunder_all_form_for_a_module_without_one() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "def alpha():\n    return 1\n")?;

        assert_eq!(module.dunder_all_form, DunderAllForm::Absent);
        Ok(())
    }

    #[test]
    fn should_record_a_literal_dunder_all_for_a_list_or_a_tuple_of_strings()
    -> Result<(), AdapterError> {
        for source in [
            "__all__ = [\"a\", \"b\"]\n",
            "__all__ = (\"a\", \"b\")\n",
            "__all__ = []\n",
        ] {
            let module = parse_source("pkg/mod.py", source)?;

            assert_eq!(module.dunder_all_form, DunderAllForm::Literal, "{source}");
        }
        Ok(())
    }

    #[test]
    fn should_record_a_dynamic_dunder_all_for_every_form_the_parser_cannot_read()
    -> Result<(), AdapterError> {
        for source in [
            "__all__ = [\"a\"]\n__all__ += [\"b\"]\n",
            "__all__ = [name for name in dir()]\n",
            "__all__ = [\"a\"] + other.__all__\n",
            "__all__ = [\"a\", *other.__all__]\n",
            "__all__ = [name]\n",
            "__all__ = build()\n",
            "__all__ = [\"a\"]\n__all__.extend([\"b\"])\n",
            "__all__ = [\"a\"]\nif flag:\n    __all__.append(\"b\")\n",
            "__all__ = [\"a\"]\ntry:\n    import fast\nexcept ImportError:\n    __all__ += [\"b\"]\n",
            "__all__ = [\"a\"]\nwith guard:\n    __all__ += [\"b\"]\n",
            "__all__ = [\"a\"]\nwhile pending:\n    __all__ += [\"b\"]\n",
            "__all__ = [\"a\"]\nfor name in names:\n    __all__ += [name]\n",
        ] {
            let module = parse_source("pkg/mod.py", source)?;

            assert_eq!(module.dunder_all_form, DunderAllForm::Dynamic, "{source}");
        }
        Ok(())
    }

    #[test]
    fn should_record_a_dynamic_dunder_all_when_a_nested_body_changes_it() -> Result<(), AdapterError>
    {
        for source in [
            "__all__ = []\n\n\ndef export(fn):\n    __all__.append(fn.__name__)\n    return fn\n",
            "__all__ = [\"a\"]\n__all__[:] = [\"b\"]\n",
            "__all__ = [\"a\"]\nglobals()[\"__all__\"] = [\"b\"]\n",
            "__all__ = [\"a\"]\n\n\nclass Registry:\n    __all__.append(\"b\")\n",
            "__all__ = [\"a\"]\n\n\ndef rebuild():\n    global __all__\n    __all__ = [\"b\"]\n",
            "__all__ = [\"a\"]\nsetattr(this, \"__all__\", [\"b\"])\n",
            "__all__ = [\"a\"]\ndel __all__[0]\n",
            "from other import __all__\n",
        ] {
            let module = parse_source("pkg/mod.py", source)?;

            assert_eq!(module.dunder_all_form, DunderAllForm::Dynamic, "{source}");
        }
        Ok(())
    }

    #[test]
    fn should_keep_a_literal_dunder_all_when_only_prose_mentions_it() -> Result<(), AdapterError> {
        let module = parse_source(
            "pkg/mod.py",
            "\"\"\"Names live in __all__.\"\"\"\n# __all__ is closed\n__all__ = [\"a\"]\n\n\ndef a():\n    \"\"\"Listed in __all__.\"\"\"\n",
        )?;

        assert_eq!(module.dunder_all_form, DunderAllForm::Literal);
        Ok(())
    }

    #[test]
    fn should_collect_imports_nested_in_module_level_blocks_apart_from_the_plain_ones()
    -> Result<(), AdapterError> {
        let module = parse_source(
            "pkg/__init__.py",
            "from .top import plain\n\ntry:\n    from ._speedups import fast\nexcept ImportError:\n    from ._pure import fast\n\nif TYPE_CHECKING:\n    from ._types import Alias\nelse:\n    from ._stub import Alias\n\nwith guard:\n    from ._with import w\n\nfor item in items:\n    from ._loop import l\n\nwhile pending:\n    from ._while import k\n\nmatch mode:\n    case 1:\n        from ._case import c\n\n\ndef lazy():\n    from ._inner import hidden\n",
        )?;

        let plain: Vec<&str> = module
            .imports
            .iter()
            .map(|import| import.module.as_str())
            .collect();
        assert_eq!(plain, ["top"]);
        let nested: Vec<&str> = module
            .conditional_imports
            .iter()
            .map(|import| import.module.as_str())
            .collect();
        assert_eq!(
            nested,
            [
                "_speedups",
                "_pure",
                "_types",
                "_stub",
                "_with",
                "_loop",
                "_while",
                "_case"
            ]
        );
        Ok(())
    }

    #[test]
    fn should_record_lazy_loading_for_every_way_a_module_sets_it_up() -> Result<(), AdapterError> {
        for source in [
            "def __getattr__(name):\n    raise AttributeError(name)\n",
            "from ._lazy import __getattr__\n",
            "__getattr__, __dir__ = lazy.attach(__name__)\n",
            "import sys\nsys.modules[__name__] = Lazy(__name__)\n",
            "import sys\nsys.modules[__name__].__class__ = Lazy\n",
            "from sys import modules\nmodules[__name__] = Lazy(__name__)\n",
            "globals()[\"__getattr__\"] = hook\n",
            "setattr(this, \"__getattr__\", hook)\n",
            "try:\n    from ._lazy import __getattr__\nexcept ImportError:\n    import sys\n    sys.modules[__name__] = Lazy()\n",
        ] {
            let module = parse_source("pkg/__init__.py", source)?;

            assert!(module.lazy_loading, "{source}");
        }
        Ok(())
    }

    #[test]
    fn should_not_record_lazy_loading_for_plain_module_code() -> Result<(), AdapterError> {
        for source in [
            "import sys\n\n\ndef main():\n    return sys.modules[\"other\"]\n",
            "import sys\nlookup = sys.modules[\"pkg.other\"]\n",
            "from ._impl import Widget\n",
            "def __dir__():\n    return []\n",
        ] {
            let module = parse_source("pkg/__init__.py", source)?;

            assert!(!module.lazy_loading, "{source}");
        }
        Ok(())
    }

    #[test]
    fn should_list_the_names_a_plain_import_binds_to_a_whole_module() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "import a.b as a\nimport c.d\nimport e as f\n")?;

        let listed: Vec<Vec<&str>> = module
            .imports
            .iter()
            .map(|import| import.aliases.iter().map(SmolStr::as_str).collect())
            .collect();
        assert_eq!(listed, [vec!["a"], vec![], vec!["f"]]);
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

        assert_eq!(module.declarations, []);
        assert_eq!(
            module.dunder_all,
            vec![SmolStr::new("a"), SmolStr::new("b")]
        );
        Ok(())
    }

    #[test]
    fn should_skip_augmented_assignment_and_bare_annotations() -> Result<(), AdapterError> {
        let module = parse_source("pkg/mod.py", "total += 1\ncount: int\n")?;

        assert_eq!(module.declarations, []);
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
