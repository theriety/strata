//! File-level import facts an adapter states about its own language.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

/// One import statement that makes a file load another analysed file.
///
/// The adapter that owns the language states its imports (decision E3); the
/// engine never projects them from symbol edges. They form the file-import
/// graph whose cycles the cycle finding reports, so a fact is only emitted for
/// a statement that resolves to a real analysed file. An import of a package,
/// of a file outside the analysed set, or one the adapter cannot resolve is
/// left out.
///
/// A type-only import (TypeScript `import type`, an import used only under
/// `TYPE_CHECKING`) creates no runtime load and is never emitted, so it can
/// never be named as part of an import cycle.
///
/// Besides the two files, the fact carries the specifier as written and its
/// 1-based line. A cycle finding names the import to remove, and the reader
/// must find that statement in the source: the line locates it, and the
/// specifier is the text they recognise there. The resolved path alone cannot
/// tell apart two statements from one file to the same target (a side-effect
/// import and a value import of `./x`). Columns are left out; they shift with
/// formatting and a line is enough to find a statement.
///
/// Paths are repo-relative with `/` separators, the same spelling the file
/// containers use. Like a [`crate::FileFact`], an import is adapter-trusted:
/// snapshot assembly does not check that either path names a file container,
/// so a consumer that looks one up skips an import whose file it cannot find.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct FileImport {
    /// The file that holds the import statement.
    pub importer: SmolStr,
    /// The analysed file the statement resolves to.
    pub imported: SmolStr,
    /// Whether the statement binds runtime names, only runs the module, or
    /// only declares it as a child module.
    pub kind: ImportKind,
    /// The module specifier exactly as the statement writes it (`./record`,
    /// `.record`, `crate::lexer`).
    pub specifier: SmolStr,
    /// The 1-based line the statement starts on.
    pub line: u32,
}

/// How an import statement loads its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportKind {
    /// The statement binds or re-exports runtime names of the target
    /// (`import { a } from './x'`, `export * from './x'`, `from .x import a`).
    Value,
    /// The statement only runs the target for its effects and binds nothing
    /// (`import './x'`).
    SideEffect,
    /// The statement declares the target as a child module of the importer
    /// and binds nothing else (Rust `mod child;`). It states ownership in the
    /// module tree, not a use of the target's items, so it never forms an
    /// import cycle on its own (ADR-35). The importer's uses of the child's
    /// items are stated as separate `Value` facts.
    ModuleDeclaration,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_round_trip_an_import_with_a_kebab_case_kind() -> Result<(), String> {
        let import = FileImport {
            importer: SmolStr::new("src/a.ts"),
            imported: SmolStr::new("src/b.ts"),
            kind: ImportKind::SideEffect,
            specifier: SmolStr::new("./b"),
            line: 3,
        };

        let json = serde_json::to_string(&import).map_err(|error| error.to_string())?;
        let restored: FileImport =
            serde_json::from_str(&json).map_err(|error| error.to_string())?;

        assert!(json.contains(r#""kind":"side-effect""#), "{json}");
        assert_eq!(restored, import);
        Ok(())
    }

    #[test]
    fn should_spell_a_module_declaration_in_kebab_case() -> Result<(), String> {
        let json = serde_json::to_string(&ImportKind::ModuleDeclaration)
            .map_err(|error| error.to_string())?;

        assert_eq!(json, r#""module-declaration""#);
        Ok(())
    }
}
