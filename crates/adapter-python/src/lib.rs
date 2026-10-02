//! The Python language adapter for Strata.
//!
//! It implements the two-phase [`strata_ir::Adapter`] contract: [`parse`]
//! turns `.py` sources into syntax summaries (via rustpython-parser, in
//! parallel), and [`bind`] runs a custom scope resolver over the module graph,
//! emitting a language-agnostic [`strata_ir::IrFragment`] — typed edges,
//! three-valued test polarity, and per-symbol production SLOC.
//!
//! No off-the-shelf semantic database exists for Python in Rust, so the binder
//! resolves lexical scope chains itself: absolute imports against the package
//! root, relative imports against the importing module's package, `__init__.py`
//! as the folder barrel, and honest low-confidence edges for Python's dynamic
//! constructs (`getattr`, `importlib`, star imports via `__all__`).
//!
//! The trait threads the intermediate [`parse::ParsedModule`] summary through
//! the opaque [`strata_ir::ParseTree`] payload as canonical JSON, so the engine
//! never needs to understand Python to merge fragments.
//!
//! [`parse`]: parse::parse
//! [`bind`]: bind::bind

pub mod bind;
pub mod parse;
pub mod scope;
pub mod sloc;

use std::path::PathBuf;

use smol_str::SmolStr;
use strata_ir::{Adapter, AdapterError, IrFragment, ParseTree, SourceFile};

use crate::parse::ParsedModule;

/// The Python adapter: parses and binds `.py` sources.
///
/// The adapter is anchored at a repository `root`; absolute imports resolve
/// against the dotted module names derived relative to it, and the laminar
/// container tree hangs off it.
#[derive(Debug, Clone)]
pub struct PythonAdapter {
    /// Repository root that module paths are relative to.
    root: PathBuf,
}

impl PythonAdapter {
    /// Creates an adapter anchored at `root`.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl Default for PythonAdapter {
    fn default() -> Self {
        Self::new(".")
    }
}

impl Adapter for PythonAdapter {
    fn parse(&self, files: &[SourceFile]) -> Result<Vec<ParseTree>, AdapterError> {
        let modules = parse::parse(files)?;
        modules.iter().map(serialize_module).collect()
    }

    fn bind(&self, trees: Vec<ParseTree>) -> Result<IrFragment, AdapterError> {
        let modules = trees
            .iter()
            .map(deserialize_module)
            .collect::<Result<Vec<_>, _>>()?;
        bind::bind(&modules, &self.root).map_err(|outcome| AdapterError::Bind {
            path: SmolStr::new(""),
            reason: outcome.to_string(),
        })
    }
}

/// Serializes a [`ParsedModule`] into an opaque [`ParseTree`] payload.
fn serialize_module(module: &ParsedModule) -> Result<ParseTree, AdapterError> {
    let payload = serde_json::to_string(module).map_err(|error| AdapterError::Parse {
        path: module.path.clone(),
        reason: format!("failed to serialize parse summary: {error}"),
    })?;
    Ok(ParseTree {
        path: module.path.clone(),
        payload,
    })
}

/// Deserializes a [`ParseTree`] payload back into a [`ParsedModule`].
fn deserialize_module(tree: &ParseTree) -> Result<ParsedModule, AdapterError> {
    serde_json::from_str(&tree.payload).map_err(|error| AdapterError::Bind {
        path: tree.path.clone(),
        reason: format!("failed to read parse summary: {error}"),
    })
}
