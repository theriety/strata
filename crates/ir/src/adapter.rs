//! The two-phase contract every language adapter implements (ad-4).

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;
use thiserror::Error;

use crate::Affinity;
use crate::container::Container;
use crate::edge::Edge;
use crate::node::{Node, NodeId};

/// A source file handed to an adapter for parsing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceFile {
    /// Repository-relative path of the file, used for diagnostics and container naming.
    pub path: SmolStr,
    /// Full UTF-8 contents of the file.
    pub contents: String,
}

/// A language-specific syntax tree produced by [`Adapter::parse`].
///
/// The IR crate treats parse trees as opaque: each adapter owns its concrete
/// representation and only the originating adapter interprets the payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseTree {
    /// The source file this tree was parsed from.
    pub path: SmolStr,
    /// Opaque, adapter-defined serialized syntax tree.
    pub payload: String,
}

/// A node's resolved Rust module scope, expressed as repository-relative files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisibilityScope {
    /// Fragment-local node whose visibility is restricted to `files`.
    pub node: NodeId,
    /// Complete file set of the resolved module subtree.
    pub files: Vec<SmolStr>,
}

/// A partial IR produced by one adapter; the engine merges fragments into one IR.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct IrFragment {
    /// Nodes contributed by this adapter.
    pub nodes: Vec<Node>,
    /// Edges contributed by this adapter.
    pub edges: Vec<Edge>,
    /// Non-dependency semantic relationships contributed by this adapter.
    pub affinities: Vec<Affinity>,
    /// Containers contributed by this adapter.
    pub containers: Vec<Container>,
    /// Resolved language scopes that must be projected onto the merged tree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub visibility_scopes: Vec<VisibilityScope>,
}

/// Failures raised while parsing or binding source with an adapter.
#[derive(Debug, Error)]
pub enum AdapterError {
    /// A source file could not be parsed into a syntax tree.
    #[error("failed to parse {path}: {reason}")]
    Parse {
        /// The file that failed to parse.
        path: SmolStr,
        /// Human-readable explanation of the parse failure.
        reason: String,
    },

    /// Name resolution failed while binding parse trees into an IR fragment.
    #[error("failed to bind {path}: {reason}")]
    Bind {
        /// The file whose names could not be resolved.
        path: SmolStr,
        /// Human-readable explanation of the binding failure.
        reason: String,
    },
}

/// The two-phase contract every language adapter implements (ad-4).
pub trait Adapter {
    /// Parses source files into language-specific syntax trees.
    ///
    /// Implementations are expected to parse files in parallel (rayon-per-file).
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError::Parse`] if any file cannot be parsed.
    fn parse(&self, files: &[SourceFile]) -> Result<Vec<ParseTree>, AdapterError>;

    /// Resolves names across the parse trees and emits a language-agnostic IR fragment.
    ///
    /// # Errors
    ///
    /// Returns [`AdapterError::Bind`] if names cannot be resolved.
    fn bind(&self, trees: Vec<ParseTree>) -> Result<IrFragment, AdapterError>;
}
