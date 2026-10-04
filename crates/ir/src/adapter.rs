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

/// One scope a language can spell for a declaration, as repository files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeRung {
    /// Complete file set of the scope.
    pub files: Vec<SmolStr>,
    /// Definition file of the scope's module, stated like
    /// [`VisibilityScope::definition_file`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_file: Option<SmolStr>,
    /// How the language writes this scope on the declaration (for example
    /// `pub(super)`), appended to the visibility advice when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spelling: Option<SmolStr>,
}

/// A node's language scope statement: the scopes its language can spell and the
/// one it declares, expressed as repository-relative files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisibilityScope {
    /// Fragment-local node whose visibility is restricted to `files`.
    pub node: NodeId,
    /// Complete file set of the declared module subtree for a restricted
    /// declaration, which the engine projects as the node's declared level;
    /// empty for any other declaration and when the scope is unresolved.
    pub files: Vec<SmolStr>,
    /// Definition file of the target module, set only for a non-root module
    /// defined in `foo.rs` beside its `foo/` folder (or in `foo/mod.rs`); the
    /// adapter states it so the engine never guesses it from file names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_file: Option<SmolStr>,
    /// Every scope the language can spell for this declaration that the adapter
    /// resolved, narrowest first. A rung that could not be resolved is left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expressible: Vec<ScopeRung>,
    /// Index into `expressible` of the rung the declaration states today, or
    /// `None` when the adapter could not resolve it. Without it the engine knows
    /// nothing to compare against and stays silent (ADR-25).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_rung: Option<u32>,
}

/// A declaration that names another one outside its body, so the file of the
/// exposer uses `exposed` and, for a signature type, the exposed declaration
/// must also be reachable wherever the exposer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exposure {
    /// Fragment-local node whose signature or header names `exposed`.
    pub exposer: NodeId,
    /// Fragment-local node named in the exposer's signature or header.
    pub exposed: NodeId,
    /// `true` when `exposed` sits in the exposer's public signature, so its
    /// declared reach must hold `exposed`; `false` for a header use that only
    /// needs the exposer's own file (the self type of an `impl`).
    pub reach: bool,
}

/// A use the adapter could only match by name, in code its semantic loader
/// cannot see. It informs visibility advice and nothing else: it is never a
/// graph edge, so no score, cohesion, cycle or move reads it (ADR-25).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvisoryUse {
    /// Fragment-local production node whose body names `used`.
    pub user: NodeId,
    /// Fragment-local node sharing a name with an identifier of `user`.
    pub used: NodeId,
    /// Every identifier match behind this pair sits in test-only code (a macro
    /// body inside a `cfg(test)` module), so it is no production user.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_test: bool,
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
    /// Signature exposures between this adapter's declarations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exposures: Vec<Exposure>,
    /// Name-matched uses that count for visibility advice only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisory_uses: Vec<AdvisoryUse>,
    /// Test pairs and entry files this adapter states for its language.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_facts: Vec<crate::FileFact>,
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
