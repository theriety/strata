//! Scores the snapshot's current layout, optionally with symbols overlaid onto other files.

use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_core::score::{Coefficients, KindWeights, ScoreBreakdown as CoreBreakdown, score};
use strata_ir::{ContainerId, ScopeLevel, Snapshot};

use super::candidate::score_candidate;
use super::distance::move_distance;
use crate::analyze::relocation::file_inventory;
use crate::config::CapacityConfig;

/// Scores the snapshot's current layout under `coefficients`.
///
/// The current candidate carries the snapshot's own edges (each crossing the LCA
/// level of its endpoints in the current tree), the file-level container sizes,
/// and a zero move distance, so its objective is the genuine `J(T0)` baseline the
/// candidates are measured against.
#[cfg(test)]
pub(in crate::analyze) fn score_current(
    snapshot: &Snapshot,
    coefficients: &Coefficients,
    weights: &KindWeights,
    folder_budget: u32,
) -> CoreBreakdown {
    let capacity = CapacityConfig {
        folder: folder_budget,
        ..CapacityConfig::default()
    };
    score_current_with_affinity(snapshot, coefficients, weights, &capacity, 1.0, 3.0)
}

pub(in crate::analyze) fn score_current_with_affinity(
    snapshot: &Snapshot,
    coefficients: &Coefficients,
    weights: &KindWeights,
    capacity: &CapacityConfig,
    same_file_symbol: f64,
    same_file_type: f64,
) -> CoreBreakdown {
    score_current_with_overlay(
        snapshot,
        coefficients,
        weights,
        capacity,
        same_file_symbol,
        same_file_type,
        &BTreeMap::new(),
    )
}

/// Scores the current tree with some declarations placed in other current
/// files: the price of a candidate that moves no file but relocates symbols.
///
/// `overlay` maps a node id to the current file container it moves to. Every
/// other node stays where it is, and the move-distance term counts exactly the
/// relocated nodes, so an empty overlay prices the current layout itself.
pub(in crate::analyze) fn score_current_with_overlay(
    snapshot: &Snapshot,
    coefficients: &Coefficients,
    weights: &KindWeights,
    capacity: &CapacityConfig,
    same_file_symbol: f64,
    same_file_type: f64,
    overlay: &BTreeMap<u32, ContainerId>,
) -> CoreBreakdown {
    let ir = snapshot.ir();
    let (files, _) = file_inventory(ir);
    let namespaces: BTreeMap<ContainerId, SmolStr> = files
        .iter()
        .map(|file| (ContainerId(file.container), file.namespace.clone()))
        .collect();
    let container_of: BTreeMap<u32, ContainerId> = ir
        .nodes
        .iter()
        .map(|node| {
            let file = overlay.get(&node.id.0).copied().unwrap_or(node.container);
            (node.id.0, file)
        })
        .collect();
    let placement = |id: u32| container_of.get(&id).copied();
    let pass_start_file_by_candidate: BTreeMap<ContainerId, ContainerId> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id, container.id))
        .collect();
    let distance = if overlay.is_empty() {
        0.0
    } else {
        move_distance(snapshot, &ir.containers, &placement)
    };
    let candidate = score_candidate(
        snapshot,
        &placement,
        &pass_start_file_by_candidate,
        &ir.containers,
        &namespaces,
        distance,
        capacity,
        same_file_symbol,
        same_file_type,
    );
    score(&candidate, coefficients, weights)
}
