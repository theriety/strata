//! Builds the scorer's candidate view from a node placement over a candidate tree.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_core::score::{Candidate as ScoreCandidate, ScoredEdge};
use strata_ir::{ContainerId, ContainerTree, Node, NodeKind, ScopeLevel, Snapshot};

use super::capacity::capacity_pressure;
use super::inputs::{cohesion_inputs, container_sizes};
use super::relocation_counts::{companion_separations, dependency_only_relocations};
use crate::config::CapacityConfig;

/// Builds the scorer's [`ScoreCandidate`] view from a node-placement function over
/// the candidate `tree`.
///
/// `placement` maps each node id to the file container it occupies in the
/// candidate tree; the edge LCA levels, per-container child sizes, and naming
/// groups are derived from that placement and the tree. `move_distance` is the
/// already-computed fraction of relocated symbols.
pub(in crate::analyze) fn score_candidate(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    pass_start_file_by_candidate: &BTreeMap<ContainerId, ContainerId>,
    tree: &ContainerTree,
    namespaces: &BTreeMap<ContainerId, SmolStr>,
    move_distance: f64,
    capacity: &CapacityConfig,
    same_file_symbol: f64,
    same_file_type: f64,
) -> ScoreCandidate {
    let ir = snapshot.ir();
    let parent_of: BTreeMap<u32, Option<ContainerId>> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container.parent))
        .collect();
    let level_of: BTreeMap<u32, ScopeLevel> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container.level))
        .collect();

    let original_nodes: BTreeMap<u32, &Node> =
        ir.nodes.iter().map(|node| (node.id.0, node)).collect();
    let edges = ir
        .edges
        .iter()
        .filter_map(|edge| {
            let source = placement(edge.source.0)?;
            let target = placement(edge.target.0)?;
            let lca_level = lca_container(&parent_of, source, target)
                .and_then(|id| level_of.get(&id.0).copied())
                .unwrap_or(ScopeLevel::PackageGroup);
            Some(ScoredEdge {
                kind: edge.kind,
                confidence: edge.confidence,
                affinity: match (
                    original_nodes.get(&edge.source.0),
                    original_nodes.get(&edge.target.0),
                ) {
                    (Some(source_node), Some(target_node))
                        if source_node.container == target_node.container =>
                    {
                        if source_node.kind == NodeKind::Type || target_node.kind == NodeKind::Type
                        {
                            same_file_type
                        } else {
                            same_file_symbol
                        }
                    }
                    _ => 1.0,
                },
                lca_level,
            })
        })
        .collect();

    let containers = container_sizes(snapshot, placement, tree);
    let (cohesion_groups, path_cohesion) = cohesion_inputs(snapshot, placement, tree);
    let capacity_pressure = capacity_pressure(snapshot, placement, tree, namespaces, capacity);
    let dependency_only_relocations =
        dependency_only_relocations(snapshot, placement, pass_start_file_by_candidate);
    let companion_separations =
        companion_separations(snapshot, placement, pass_start_file_by_candidate);

    ScoreCandidate {
        edges,
        containers,
        cohesion_groups,
        path_cohesion,
        move_distance,
        capacity_pressure,
        dependency_only_relocations,
        companion_separations,
    }
}

/// Returns the lowest common ancestor container of two containers.
///
/// Walks the ancestor chain of `left` into a set, then ascends `right` until a
/// shared ancestor is found; the highest endpoints share is the package-group root
/// of an empty intersection, so disjoint subtrees cross at the coarsest level.
pub(super) fn lca_container(
    parent_of: &BTreeMap<u32, Option<ContainerId>>,
    left: ContainerId,
    right: ContainerId,
) -> Option<ContainerId> {
    let mut ancestors = std::collections::BTreeSet::new();
    let mut up_left = Some(left);
    while let Some(id) = up_left {
        if !ancestors.insert(id.0) {
            break;
        }
        up_left = parent_of.get(&id.0).copied().flatten();
    }

    let mut up_right = Some(right);
    while let Some(id) = up_right {
        if ancestors.contains(&id.0) {
            return Some(id);
        }
        up_right = parent_of.get(&id.0).copied().flatten();
    }
    None
}
