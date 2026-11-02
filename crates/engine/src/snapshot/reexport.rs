//! Re-export flattening: rewrites edges through re-export chains to the
//! original definitions under a depth guard.

use std::collections::HashMap;

use smol_str::SmolStr;
use strata_ir::{Edge, EdgeKind, IntermediateRepresentation, Node, NodeId};

use crate::error::StrataError;

/// The maximum re-export chain depth before flattening gives up.
///
/// A legitimate barrel chain is shallow; a chain deeper than this is almost
/// always a cycle, so the guard raises [`StrataError::ReExportDepthExceeded`]
/// rather than looping.
const RE_EXPORT_DEPTH_LIMIT: u32 = 64;

/// Flattens re-export edges to their original definitions, guarding against
/// pathological chains.
///
/// A re-export edge `source -> target` means `source` re-publishes `target`; a
/// consumer of `source` truly depends on whatever `target` ultimately resolves
/// to. Each re-export is rewritten to point at the end of its chain; a chain
/// longer than [`RE_EXPORT_DEPTH_LIMIT`] (almost always a cycle) raises an error.
///
/// # Errors
///
/// Returns [`StrataError::ReExportDepthExceeded`] when a chain exceeds the guard.
pub(super) fn flatten_re_exports(
    mut ir: IntermediateRepresentation,
) -> Result<IntermediateRepresentation, StrataError> {
    // map each re-export source to its immediate target, then resolve transitively.
    let re_export_target = re_export_targets(&ir.edges);

    let name_of = name_lookup(&ir.nodes);
    for edge in &mut ir.edges {
        if edge.kind == EdgeKind::ReExport {
            continue;
        }
        edge.target = resolve_re_export(edge.target, &re_export_target, &name_of)?;
    }

    Ok(ir)
}

/// Maps each re-export source to its immediate target.
pub(super) fn re_export_targets(edges: &[Edge]) -> HashMap<NodeId, NodeId> {
    edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::ReExport)
        .map(|edge| (edge.source, edge.target))
        .collect()
}

/// Follows the re-export chain from `start` to its original definition, bounded
/// by the depth guard.
pub(super) fn resolve_re_export(
    start: NodeId,
    re_export_target: &HashMap<NodeId, NodeId>,
    name_of: &HashMap<NodeId, SmolStr>,
) -> Result<NodeId, StrataError> {
    let mut current = start;
    let mut depth = 0_u32;
    while let Some(&next) = re_export_target.get(&current) {
        if next == current {
            break;
        }
        depth += 1;
        if depth > RE_EXPORT_DEPTH_LIMIT {
            return Err(StrataError::ReExportDepthExceeded {
                origin: name_of
                    .get(&start)
                    .map_or_else(|| format!("node {}", start.0), SmolStr::to_string),
                limit: RE_EXPORT_DEPTH_LIMIT,
            });
        }
        current = next;
    }
    Ok(current)
}

/// Builds a node-id to name lookup for diagnostics.
pub(super) fn name_lookup(nodes: &[Node]) -> HashMap<NodeId, SmolStr> {
    nodes
        .iter()
        .map(|node| (node.id, node.name.clone()))
        .collect()
}
