//! swc parsing of `.ts` / `.tsx` sources into serializable [`ParsedModule`]s.
//!
//! Parsing runs in parallel across files (one rayon task per file). A syntax
//! error is surfaced as [`AdapterError::Parse`] carrying the offending path and
//! a span-derived reason — files are never skipped silently.

use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize};
use smol_str::SmolStr;
use strata_ir::{AdapterError, SourceFile};
use swc_common::{FileName, SourceMap, Spanned, sync::Lrc};
use swc_ecma_parser::{Parser, StringInput, Syntax, TsSyntax, lexer::Lexer};

mod declarations;
mod references;

/// A top-level program entity extracted from a module, with its production SLOC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    /// Source-declared name, or a synthetic name for executable file-scope content.
    pub name: SmolStr,
    /// The program entity represented by this declaration.
    pub kind: DeclarationKind,
    /// Whether the declaration is exported from the module.
    pub exported: bool,
    /// Production SLOC attributed to this declaration.
    pub sloc: u32,
    /// Names this declaration extends or implements (drives inheritance edges).
    pub supertypes: Vec<SmolStr>,
    /// Identifiers referenced within this declaration's body.
    pub referenced: Vec<SmolStr>,
    /// Identifiers invoked as a call or `new` target within this declaration
    /// (drives call edges, resolved through the symbol tables).
    pub called: Vec<SmolStr>,
    /// Literal specifiers of `import('...')` calls inside this declaration.
    pub dynamic_imports: Vec<SmolStr>,
    /// Signature types whose names uniquely match this declaration as owner.
    #[serde(default)]
    pub signature_companions: Vec<SmolStr>,
}

/// The semantic role of a parsed top-level declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeclarationKind {
    /// A value-level declaration.
    Symbol,
    /// A type-level declaration.
    Type,
    /// Executable file-scope content without an independent declaration.
    FileBody,
}

/// A static import (`import { x } from '...'`) with literal specifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaticImport {
    /// The module specifier string (e.g. `./util` or `@scope/pkg`).
    pub source: SmolStr,
    /// Locally bound names brought in by this import.
    pub names: Vec<SmolStr>,
    /// `true` for `import type` / type-only specifiers (erased at runtime).
    pub type_only: bool,
}

/// A re-export (`export { x } from '...'`) emitted verbatim for the engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReExport {
    /// The module specifier the symbols are re-exported from.
    pub source: SmolStr,
    /// Named re-export bindings, empty for `export * from '...'`.
    #[serde(default)]
    pub names: Vec<ReExportBinding>,
    /// `true` for `export type { x } from '...'`.
    pub type_only: bool,
}

/// A source binding introduced by a re-export declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum ReExportBinding {
    /// Legacy unaliased payload (`"Widget"`).
    Legacy(SmolStr),
    /// A named binding, including an explicitly represented unaliased name.
    Named {
        /// Name looked up in the source module.
        original: SmolStr,
        /// Name introduced in the re-exporting module.
        exported: SmolStr,
    },
    /// A namespace binding (`export * as namespace`).
    Namespace {
        /// Namespace symbol introduced in the re-exporting module.
        namespace: SmolStr,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NamedReExportBinding {
    original: SmolStr,
    exported: SmolStr,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NamespaceReExportBinding {
    namespace: SmolStr,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReExportBindingWire {
    Legacy(SmolStr),
    Named(NamedReExportBinding),
    Namespace(NamespaceReExportBinding),
}

impl<'de> Deserialize<'de> for ReExportBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(match ReExportBindingWire::deserialize(deserializer)? {
            ReExportBindingWire::Legacy(name) => Self::Legacy(name),
            ReExportBindingWire::Named(binding) => Self::Named {
                original: binding.original,
                exported: binding.exported,
            },
            ReExportBindingWire::Namespace(binding) => Self::Namespace {
                namespace: binding.namespace,
            },
        })
    }
}

/// A serializable, language-agnostic summary of one parsed module.
///
/// This is the payload the adapter threads through [`strata_ir::ParseTree`]:
/// [`parse`] produces it, [`crate::bind::bind`] consumes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParsedModule {
    /// Repository-relative path of the source file.
    pub path: SmolStr,
    /// Declared entities in source order, followed by any aggregated file body.
    pub declarations: Vec<Declaration>,
    /// Static imports with literal specifiers.
    pub imports: Vec<StaticImport>,
    /// `export ... from '...'` re-exports, emitted as-is.
    pub re_exports: Vec<ReExport>,
}

/// Parses TypeScript / TSX sources into [`ParsedModule`]s, one rayon task per file.
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
    let source_map: Lrc<SourceMap> = Lrc::default();
    let source = source_map.new_source_file(
        Lrc::new(FileName::Custom(file.path.to_string())),
        file.contents.clone(),
    );
    let base = source.start_pos;

    let tsx = file.path.ends_with(".tsx");
    let lexer = Lexer::new(
        Syntax::Typescript(TsSyntax {
            tsx,
            ..TsSyntax::default()
        }),
        swc_ecma_ast::EsVersion::default(),
        StringInput::from(&*source),
        None,
    );
    let mut parser = Parser::new_from(lexer);

    let module = parser.parse_module().map_err(|error| AdapterError::Parse {
        path: file.path.clone(),
        reason: format_parse_error(&error, &source_map),
    })?;

    Ok(declarations::extract(
        &file.path,
        &module,
        &file.contents,
        base,
    ))
}

/// Renders a parser error as a `line L, column C: message` string.
fn format_parse_error(error: &swc_ecma_parser::error::Error, source_map: &SourceMap) -> String {
    let location = source_map.lookup_char_pos(error.span().lo);
    format!(
        "line {}, column {}: {}",
        location.line,
        location.col_display,
        error.kind().msg()
    )
}

/// Converts a string-literal specifier to a [`SmolStr`], lossily for the rare
/// non-UTF-8 case (module specifiers are UTF-8 in every real source).
fn specifier(value: &swc_ecma_ast::Str) -> SmolStr {
    SmolStr::new(value.value.as_str().unwrap_or_default())
}
