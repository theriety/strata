//! Directed dependency edges between nodes.

use serde::{Deserialize, Serialize};

use crate::node::NodeId;

/// Dependency kinds; weights are assigned later by config, not stored here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum EdgeKind {
    /// Import of a runtime value.
    ValueImport,
    /// Reference to a type in a type position.
    TypeReference,
    /// A subtype-of / implements relationship.
    Inheritance,
    /// A direct call to the target.
    Call,
    /// A re-export of the target through the source.
    ReExport,
}

/// Hard edges constrain acyclicity; soft edges only score.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Hardness {
    /// Must be respected by any acyclic layering.
    Hard,
    /// Contributes to scoring only; may be violated.
    Soft,
}

/// A directed dependency from `source` to `target`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    /// The node that depends on `target`.
    pub source: NodeId,
    /// The node that `source` depends on.
    pub target: NodeId,
    /// The kind of dependency.
    pub kind: EdgeKind,
    /// Whether the edge is hard (constraining) or soft (scoring only).
    pub hardness: Hardness,
    /// `1.0` = statically certain; below `1.0` for dynamic constructs
    /// (dynamic import, getattr, macro-expanded references).
    pub confidence: f64,
}
