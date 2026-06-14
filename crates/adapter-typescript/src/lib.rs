//! The TypeScript / TSX language adapter for Strata.
//!
//! It implements the two-phase [`strata_ir::Adapter`] contract: [`parse`]
//! turns `.ts` / `.tsx` sources into syntax summaries (via swc, in parallel),
//! and [`bind`] resolves the module graph into a language-agnostic
//! [`strata_ir::IrFragment`] — typed edges, three-valued test polarity, and
//! per-symbol production SLOC.
//!
//! The trait threads the intermediate [`parse::ParsedModule`] summary through
//! the opaque [`strata_ir::ParseTree`] payload as canonical JSON, so the engine
//! never needs to understand TypeScript to merge fragments.
//!
//! [`parse`]: parse::parse
//! [`bind`]: bind::bind

pub mod bind;
pub mod parse;
pub mod sloc;
pub mod tsconfig;

use std::collections::BTreeMap;
use std::path::PathBuf;

use smol_str::SmolStr;
use strata_ir::{Adapter, AdapterError, IrFragment, ParseTree, SourceFile};

use crate::parse::ParsedModule;

/// The TypeScript adapter: parses and binds `.ts` / `.tsx` sources.
///
/// The adapter is anchored at a repository `root`; module specifiers and
/// `tsconfig` `paths` aliases resolve relative to it. The alias table is read
/// once at construction (the `tsconfig` cache).
#[derive(Debug, Clone)]
pub struct TypeScriptAdapter {
    /// Repository root that module paths are relative to.
    root: PathBuf,
    /// `tsconfig` `paths` alias prefix -> repo-relative target prefix.
    aliases: BTreeMap<SmolStr, SmolStr>,
}

impl TypeScriptAdapter {
    /// Creates an adapter anchored at `root`, reading `root/tsconfig.json` for
    /// `paths` aliases when present.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let aliases = tsconfig::load_aliases(&root);
        Self { root, aliases }
    }
}

impl Default for TypeScriptAdapter {
    fn default() -> Self {
        Self::new(".")
    }
}

impl Adapter for TypeScriptAdapter {
    fn parse(&self, files: &[SourceFile]) -> Result<Vec<ParseTree>, AdapterError> {
        let modules = parse::parse(files)?;
        modules.iter().map(serialize_module).collect()
    }

    fn bind(&self, trees: Vec<ParseTree>) -> Result<IrFragment, AdapterError> {
        let modules = trees
            .iter()
            .map(deserialize_module)
            .collect::<Result<Vec<_>, _>>()?;
        bind::bind(&modules, &self.root, &self.aliases).map_err(|outcome| AdapterError::Bind {
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
