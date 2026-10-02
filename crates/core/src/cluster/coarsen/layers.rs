//! Tight longest-path layering of a contracted graph.

use super::UNMATCHED;
use crate::graph::csr::Csr;

/// Computes tight longest-path layers of `graph` (edges point from dependent to
/// dependency): sinks sit at layer `1` and `layer(u) = 1 + max(layer(dep))`.
///
/// Inheriting a coarse vertex's layer from its members leaves gaps (a merged
/// pair keeps the max member layer, so adjacent coarse vertices can sit two
/// layers apart), and a gapped layering blinds the `is_safe_to_merge` screen —
/// a direct one-edge hop then looks like a two-hop detour and every further
/// merge is rejected, stalling the chain. Each level therefore recomputes its
/// own layering from scratch. The result is order-independent (each layer is a
/// max over all dependencies), hence deterministic. Public so the engine can
/// layer the upper-level quotient graphs it clusters recursively.
#[must_use]
pub fn tight_layers(graph: &Csr) -> Vec<u32> {
    let count = graph.vertex_count();
    let mut layers = vec![1_u32; count];

    // Kahn over the dependency direction: a vertex is ready once all of its
    // dependencies (outgoing neighbours) are finalized.
    let mut dependents: Vec<Vec<u32>> = vec![Vec::new(); count];
    let mut pending: Vec<u32> = vec![0; count];
    let mut ready: Vec<u32> = Vec::new();
    for u in 0..count {
        let u32_u = u32::try_from(u).unwrap_or(UNMATCHED);
        let dependencies = graph.neighbors(u32_u);
        if let Some(slot) = pending.get_mut(u) {
            *slot = u32::try_from(dependencies.len()).unwrap_or(u32::MAX);
        }
        if dependencies.is_empty() {
            ready.push(u32_u);
        }
        for &v in dependencies {
            if let Some(list) = dependents.get_mut(v as usize) {
                list.push(u32_u);
            }
        }
    }

    while let Some(v) = ready.pop() {
        let layer_v = layers.get(v as usize).copied().unwrap_or(1);
        for &u in dependents
            .get(v as usize)
            .map_or(&[] as &[u32], Vec::as_slice)
        {
            if let Some(slot) = layers.get_mut(u as usize) {
                *slot = (*slot).max(layer_v + 1);
            }
            if let Some(slot) = pending.get_mut(u as usize) {
                *slot = slot.saturating_sub(1);
                if *slot == 0 {
                    ready.push(u);
                }
            }
        }
    }

    layers
}
