//! The Rust language adapter for Strata.
//!
//! It implements the two-phase [`strata_ir::Adapter`] contract: [`parse`] turns
//! `.rs` sources into per-file syntax summaries (via syn, in parallel), and
//! [`bind`] resolves the cross-crate graph into a language-agnostic
//! [`strata_ir::IrFragment`] — typed edges, three-valued test polarity, and
//! per-symbol production SLOC.
//!
//! Resolution is semantic: [`bind`] loads the on-disk cargo workspace into the
//! rust-analyzer (`ra_ap_*`) database and resolves each recorded reference
//! through `goto_definition`, mapping the resolved definition back to the
//! declaration whose byte range contains it. Macro-expanded references resolve
//! at confidence below `1.0` — honest uncertainty instead of silence (per the
//! AD-4 binder go decision, CN-1).
//!
//! The trait threads the intermediate [`parse::ParsedFile`] summary through the
//! opaque [`strata_ir::ParseTree`] payload as JSON, so the engine never needs to
//! understand Rust to merge fragments.
//!
//! [`parse`]: parse::parse
//! [`bind`]: bind::bind

pub mod bind;
pub mod parse;
pub mod sloc;

use std::path::{Path, PathBuf};

use smol_str::SmolStr;
use strata_ir::{Adapter, AdapterError, IrFragment, ParseTree, SourceFile};

use crate::parse::ParsedFile;

/// The Rust adapter: parses `.rs` sources and binds them against a cargo
/// workspace.
///
/// The adapter is anchored at a `manifest` (the workspace `Cargo.toml`) for
/// semantic resolution and a repository `root` that the parsed file paths are
/// relative to. By convention the manifest's directory is the repository root,
/// which [`RustAdapter::new`] assumes; [`RustAdapter::with_root`] separates them
/// when sources are reported relative to a different anchor.
#[derive(Debug, Clone)]
pub struct RustAdapter {
    /// Path to the workspace `Cargo.toml` the semantic database loads from.
    manifest: PathBuf,
    /// Repository root the parsed file paths are relative to.
    root: PathBuf,
}

impl RustAdapter {
    /// Creates an adapter for the workspace whose `Cargo.toml` is at `manifest`,
    /// treating the manifest's directory as the repository root.
    #[must_use]
    pub fn new(manifest: impl Into<PathBuf>) -> Self {
        let manifest = manifest.into();
        let root = manifest
            .parent()
            .map_or_else(|| manifest.clone(), Path::to_path_buf);
        Self { manifest, root }
    }

    /// Creates an adapter with an explicit repository `root` distinct from the
    /// manifest's directory.
    #[must_use]
    pub fn with_root(manifest: impl Into<PathBuf>, root: impl Into<PathBuf>) -> Self {
        Self {
            manifest: manifest.into(),
            root: root.into(),
        }
    }
}

impl Adapter for RustAdapter {
    fn parse(&self, files: &[SourceFile]) -> Result<Vec<ParseTree>, AdapterError> {
        let parsed = parse::parse(files)?;
        parsed.iter().map(serialize_file).collect()
    }

    fn bind(&self, trees: Vec<ParseTree>) -> Result<IrFragment, AdapterError> {
        let files = trees
            .iter()
            .map(deserialize_file)
            .collect::<Result<Vec<_>, _>>()?;
        bind::bind(&files, &self.manifest, &self.root).map_err(|outcome| AdapterError::Bind {
            path: SmolStr::new(""),
            reason: outcome.to_string(),
        })
    }
}

/// Serializes a [`ParsedFile`] into an opaque [`ParseTree`] payload.
fn serialize_file(file: &ParsedFile) -> Result<ParseTree, AdapterError> {
    let payload = serde_json::to_string(file).map_err(|error| AdapterError::Parse {
        path: file.path.clone(),
        reason: format!("failed to serialize parse summary: {error}"),
    })?;
    Ok(ParseTree {
        path: file.path.clone(),
        payload,
    })
}

/// Deserializes a [`ParseTree`] payload back into a [`ParsedFile`].
fn deserialize_file(tree: &ParseTree) -> Result<ParsedFile, AdapterError> {
    serde_json::from_str(&tree.payload).map_err(|error| AdapterError::Bind {
        path: tree.path.clone(),
        reason: format!("failed to read parse summary: {error}"),
    })
}
