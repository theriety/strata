//! The set of languages Strata can analyze, with extension matching and
//! adapter construction.

use std::path::Path;

use strata_adapter_python::PythonAdapter;
use strata_adapter_rust::RustAdapter;
use strata_adapter_typescript::TypeScriptAdapter;
use strata_ir::Adapter;

/// A language Strata can analyze, with its file extensions and adapter factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Language {
    /// TypeScript / TSX.
    TypeScript,
    /// Rust.
    Rust,
    /// Python.
    Python,
}

impl Language {
    /// Every supported language, for extension-based classification.
    pub(crate) const ALL: [Self; 3] = [Self::TypeScript, Self::Rust, Self::Python];

    /// Resolves a config language name to a [`Language`], if recognized.
    pub(super) fn from_name(name: &str) -> Option<Self> {
        match name {
            "typescript" => Some(Self::TypeScript),
            "rust" => Some(Self::Rust),
            "python" => Some(Self::Python),
            _ => None,
        }
    }

    /// Returns the config-facing name of this language.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Rust => "rust",
            Self::Python => "python",
        }
    }

    /// Returns whether `path` belongs to this language by extension.
    pub(crate) fn matches_extension(self, path: &str) -> bool {
        let extension = path.rsplit('.').next().unwrap_or("");
        match self {
            Self::TypeScript => matches!(extension, "ts" | "tsx" | "mts" | "cts"),
            Self::Rust => extension == "rs",
            Self::Python => matches!(extension, "py" | "pyi"),
        }
    }

    /// Whether `path` names a file the language's module system is built
    /// around, so emptying or retiring it breaks the module structure: a Rust
    /// crate or module root (`lib.rs`, `main.rs`, `mod.rs`), a TypeScript
    /// directory index (`index.ts` and its variants), or a Python package
    /// marker or entry point (`__init__.py`, `__main__.py`).
    pub(crate) fn is_module_root(path: &str) -> bool {
        let name = path.rsplit('/').next().unwrap_or(path);
        Self::ALL.iter().any(|language| {
            language.matches_extension(name)
                && match language {
                    Self::TypeScript => {
                        matches!(name, "index.ts" | "index.tsx" | "index.mts" | "index.cts")
                    }
                    Self::Rust => matches!(name, "lib.rs" | "main.rs" | "mod.rs"),
                    Self::Python => matches!(
                        name,
                        "__init__.py" | "__init__.pyi" | "__main__.py" | "__main__.pyi"
                    ),
                }
        })
    }

    /// Builds the adapter for this language anchored at `root`.
    pub(super) fn adapter(self, root: &Path) -> Box<dyn Adapter + Send + Sync> {
        match self {
            Self::TypeScript => Box::new(TypeScriptAdapter::new(root)),
            Self::Rust => Box::new(RustAdapter::new(root.join("Cargo.toml"))),
            Self::Python => Box::new(PythonAdapter::new(root)),
        }
    }
}
