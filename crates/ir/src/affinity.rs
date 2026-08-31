//! Non-dependency semantic relationships between declarations.

use serde::{Deserialize, Serialize};

use crate::node::NodeId;

/// A semantic preference relating a companion declaration to its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Affinity {
    /// Declaration that owns the companion contract.
    pub owner: NodeId,
    /// Companion declaration that prefers the owner's immutable file.
    pub companion: NodeId,
    /// Semantic relationship represented by this affinity.
    pub kind: AffinityKind,
}

/// Language-neutral semantic affinity categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AffinityKind {
    /// A signature type whose name uniquely identifies an owning declaration.
    CompanionOwner,
}
