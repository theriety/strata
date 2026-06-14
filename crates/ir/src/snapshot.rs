//! The validated, content-addressed [`Snapshot`] handed to the solver.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::container::{ContainerTree, TreeError};
use crate::edge::Edge;
use crate::node::{Node, NodeId};

/// Current IR schema version, bumped on any breaking contract change.
pub const SCHEMA_VERSION: u32 = 1;

/// The raw, unvalidated typed graph plus its laminar container tree.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntermediateRepresentation {
    /// Bumped on any breaking contract change; checked on deserialization.
    pub schema_version: u32,
    /// All nodes in the graph.
    pub nodes: Vec<Node>,
    /// All directed edges in the graph.
    pub edges: Vec<Edge>,
    /// The laminar container tree.
    pub containers: ContainerTree,
}

impl IntermediateRepresentation {
    /// Builds an IR at the current [`SCHEMA_VERSION`] from its parts.
    #[must_use]
    pub fn new(nodes: Vec<Node>, edges: Vec<Edge>, containers: ContainerTree) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            nodes,
            edges,
            containers,
        }
    }
}

/// A validated, content-addressed input to the solver — immutable after assembly.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    ir: IntermediateRepresentation,
    hash: blake3::Hash,
}

/// Reasons [`Snapshot::assemble`] rejects an [`IntermediateRepresentation`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SnapshotError {
    /// The IR was produced under an incompatible schema version.
    #[error("unsupported schema version {found}; this build expects {expected}")]
    SchemaVersion {
        /// The version stamped on the IR.
        found: u32,
        /// The version this build understands.
        expected: u32,
    },

    /// An edge endpoint references a node id that has no node.
    #[error("edge from {source_node} to {target_node} references missing node {missing}")]
    DanglingEdge {
        /// The edge's source node id.
        source_node: u32,
        /// The edge's target node id.
        target_node: u32,
        /// The endpoint id (source or target) that has no node.
        missing: u32,
    },

    /// The container tree failed well-formedness validation.
    #[error("invalid container tree: {0}")]
    Tree(#[from] TreeError),

    /// The IR could not be serialized for canonical hashing.
    #[error("failed to serialize snapshot for hashing: {reason}")]
    Serialization {
        /// Human-readable serialization failure detail.
        reason: String,
    },
}

impl Snapshot {
    /// Assembles a validated, hashed snapshot from an [`IntermediateRepresentation`].
    ///
    /// Assembly:
    /// 1. the schema version must match [`SCHEMA_VERSION`],
    /// 2. every edge endpoint must reference an existing node,
    /// 3. the container tree must validate,
    /// 4. nodes, edges, and containers are canonicalized (sorted by id),
    /// 5. the hash is `blake3` over the canonical JSON serialization.
    ///
    /// Canonical sorting makes the hash independent of input order: identical
    /// inputs hash identically regardless of how entities were ordered.
    ///
    /// # Errors
    ///
    /// Returns a [`SnapshotError`] if the schema version is unsupported, an edge
    /// dangles, the tree is malformed, or serialization fails.
    pub fn assemble(mut ir: IntermediateRepresentation) -> Result<Self, SnapshotError> {
        if ir.schema_version != SCHEMA_VERSION {
            return Err(SnapshotError::SchemaVersion {
                found: ir.schema_version,
                expected: SCHEMA_VERSION,
            });
        }

        let node_ids: HashSet<NodeId> = ir.nodes.iter().map(|node| node.id).collect();
        for edge in &ir.edges {
            if !node_ids.contains(&edge.source) {
                return Err(SnapshotError::DanglingEdge {
                    source_node: edge.source.0,
                    target_node: edge.target.0,
                    missing: edge.source.0,
                });
            }
            if !node_ids.contains(&edge.target) {
                return Err(SnapshotError::DanglingEdge {
                    source_node: edge.source.0,
                    target_node: edge.target.0,
                    missing: edge.target.0,
                });
            }
        }

        ir.containers.validate()?;

        // Canonicalize so the hash is independent of input order. The edge key is
        // total: parallel edges sharing a (source, target, kind) triple are further
        // ordered by hardness and the raw bit pattern of confidence, so no input
        // ordering of duplicate edges can leak into the hash.
        ir.nodes.sort_by_key(|node| node.id.0);
        ir.edges.sort_by_key(|edge| {
            (
                edge.source.0,
                edge.target.0,
                edge.kind,
                edge.hardness,
                edge.confidence.to_bits(),
            )
        });
        ir.containers.sort_by_id();

        let bytes = serde_json::to_vec(&ir).map_err(|error| SnapshotError::Serialization {
            reason: error.to_string(),
        })?;
        let hash = blake3::hash(&bytes);

        Ok(Self { ir, hash })
    }

    /// Returns the content hash of this snapshot.
    #[must_use]
    pub fn hash(&self) -> &blake3::Hash {
        &self.hash
    }

    /// Returns the validated, canonicalized intermediate representation.
    #[must_use]
    pub fn ir(&self) -> &IntermediateRepresentation {
        &self.ir
    }
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;

    use super::*;
    use crate::container::{Container, ContainerId};
    use crate::edge::{EdgeKind, Hardness};
    use crate::node::{NodeKind, Polarity, ScopeLevel};

    fn root_container() -> Container {
        Container {
            id: ContainerId(0),
            name: SmolStr::new("root"),
            level: ScopeLevel::File,
            parent: None,
        }
    }

    fn node(id: u32, name: &str) -> Node {
        Node {
            id: NodeId(id),
            name: SmolStr::new(name),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
        }
    }

    fn edge(source: u32, target: u32) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind: EdgeKind::Call,
            hardness: Hardness::Hard,
            confidence: 1.0,
        }
    }

    /// Builds a `Call` edge over the same node pair with a chosen hardness and
    /// confidence, used to exercise parallel-edge canonicalization.
    fn edge_with(source: u32, target: u32, hardness: Hardness, confidence: f64) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind: EdgeKind::Call,
            hardness,
            confidence,
        }
    }

    fn ir_with(nodes: Vec<Node>, edges: Vec<Edge>) -> IntermediateRepresentation {
        IntermediateRepresentation::new(nodes, edges, ContainerTree::new(vec![root_container()]))
    }

    #[test]
    fn should_assemble_a_valid_ir() {
        let ir = ir_with(vec![node(0, "a"), node(1, "b")], vec![edge(0, 1)]);

        let snapshot = Snapshot::assemble(ir).map(|snapshot| snapshot.ir().nodes.len());

        assert_eq!(snapshot, Ok(2));
    }

    #[test]
    fn should_reject_an_unsupported_schema_version() {
        let mut ir = ir_with(vec![node(0, "a")], vec![]);
        ir.schema_version = SCHEMA_VERSION + 1;

        assert_eq!(
            Snapshot::assemble(ir),
            Err(SnapshotError::SchemaVersion {
                found: SCHEMA_VERSION + 1,
                expected: SCHEMA_VERSION,
            })
        );
    }

    #[test]
    fn should_reject_an_edge_with_a_missing_source() {
        let ir = ir_with(vec![node(1, "b")], vec![edge(0, 1)]);

        assert_eq!(
            Snapshot::assemble(ir),
            Err(SnapshotError::DanglingEdge {
                source_node: 0,
                target_node: 1,
                missing: 0,
            })
        );
    }

    #[test]
    fn should_reject_an_edge_with_a_missing_target() {
        let ir = ir_with(vec![node(0, "a")], vec![edge(0, 1)]);

        assert_eq!(
            Snapshot::assemble(ir),
            Err(SnapshotError::DanglingEdge {
                source_node: 0,
                target_node: 1,
                missing: 1,
            })
        );
    }

    #[test]
    fn should_propagate_a_container_tree_error() {
        let bad_tree = ContainerTree::new(vec![Container {
            id: ContainerId(0),
            name: SmolStr::new("orphan"),
            level: ScopeLevel::File,
            parent: Some(ContainerId(9)),
        }]);
        let ir = IntermediateRepresentation::new(vec![node(0, "a")], vec![], bad_tree);

        assert_eq!(
            Snapshot::assemble(ir),
            Err(SnapshotError::Tree(TreeError::MissingParent {
                child: 0,
                parent: 9,
            }))
        );
    }

    /// Assembles `ir` and returns its hash as bytes, or `None` on failure.
    fn hash_of(ir: IntermediateRepresentation) -> Option<[u8; 32]> {
        Snapshot::assemble(ir)
            .ok()
            .map(|snapshot| *snapshot.hash().as_bytes())
    }

    #[test]
    fn should_hash_identically_regardless_of_input_order() {
        let ordered = ir_with(vec![node(0, "a"), node(1, "b")], vec![edge(0, 1)]);
        let shuffled = ir_with(vec![node(1, "b"), node(0, "a")], vec![edge(0, 1)]);

        assert_eq!(hash_of(ordered), hash_of(shuffled));
    }

    #[test]
    fn should_hash_identically_for_reordered_parallel_edges_differing_in_confidence() {
        let nodes = || vec![node(0, "a"), node(1, "b")];
        let ordered = ir_with(
            nodes(),
            vec![
                edge_with(0, 1, Hardness::Hard, 0.5),
                edge_with(0, 1, Hardness::Hard, 1.0),
            ],
        );
        let shuffled = ir_with(
            nodes(),
            vec![
                edge_with(0, 1, Hardness::Hard, 1.0),
                edge_with(0, 1, Hardness::Hard, 0.5),
            ],
        );

        assert_eq!(hash_of(ordered), hash_of(shuffled));
    }

    #[test]
    fn should_hash_identically_for_reordered_parallel_edges_differing_in_hardness() {
        let nodes = || vec![node(0, "a"), node(1, "b")];
        let ordered = ir_with(
            nodes(),
            vec![
                edge_with(0, 1, Hardness::Hard, 1.0),
                edge_with(0, 1, Hardness::Soft, 1.0),
            ],
        );
        let shuffled = ir_with(
            nodes(),
            vec![
                edge_with(0, 1, Hardness::Soft, 1.0),
                edge_with(0, 1, Hardness::Hard, 1.0),
            ],
        );

        assert_eq!(hash_of(ordered), hash_of(shuffled));
    }

    #[test]
    fn should_hash_differently_for_parallel_edges_differing_in_confidence() {
        let nodes = || vec![node(0, "a"), node(1, "b")];
        let lower = ir_with(nodes(), vec![edge_with(0, 1, Hardness::Hard, 0.5)]);
        let higher = ir_with(nodes(), vec![edge_with(0, 1, Hardness::Hard, 1.0)]);

        assert_ne!(hash_of(lower), hash_of(higher));
    }

    #[test]
    fn should_hash_differently_for_different_content() {
        let one = ir_with(vec![node(0, "a")], vec![]);
        let two = ir_with(vec![node(0, "different")], vec![]);

        let one_hash = hash_of(one);
        let two_hash = hash_of(two);

        assert!(one_hash.is_some());
        assert_ne!(one_hash, two_hash);
    }
}
