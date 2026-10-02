//! Counts of declarations and companions that leave their pass-start file.

use std::collections::{BTreeMap, BTreeSet};

use strata_ir::{ContainerId, Node, NodeId, NodeKind, Polarity, Snapshot};

/// Counts companions not placed in the immutable pass-start file of their owner.
pub(in crate::analyze) fn companion_separations(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    pass_start_file_by_candidate: &BTreeMap<ContainerId, ContainerId>,
) -> u32 {
    let nodes: BTreeMap<NodeId, &Node> = snapshot
        .ir()
        .nodes
        .iter()
        .map(|node| (node.id, node))
        .collect();
    snapshot
        .ir()
        .affinities
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|affinity| {
            let Some(owner) = nodes.get(&affinity.owner) else {
                return false;
            };
            placement(affinity.companion.0)
                .and_then(|file| pass_start_file_by_candidate.get(&file).copied())
                != Some(owner.container)
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Counts production declarations that leave their pass-start file for a file
/// holding one of their dependencies but none of their consumers. Candidate
/// file ids are mapped back to immutable file identity so moving a whole file
/// between folders contributes zero.
pub(in crate::analyze) fn dependency_only_relocations(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    pass_start_file_by_candidate: &BTreeMap<ContainerId, ContainerId>,
) -> u32 {
    let ir = snapshot.ir();
    let nodes: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();
    let mut outgoing_files: BTreeMap<u32, BTreeSet<ContainerId>> = BTreeMap::new();
    let mut incoming_files: BTreeMap<u32, BTreeSet<ContainerId>> = BTreeMap::new();
    for edge in &ir.edges {
        if edge.source == edge.target {
            continue;
        }
        let (Some(source), Some(target)) = (nodes.get(&edge.source.0), nodes.get(&edge.target.0))
        else {
            continue;
        };
        outgoing_files
            .entry(edge.source.0)
            .or_default()
            .insert(target.container);
        incoming_files
            .entry(edge.target.0)
            .or_default()
            .insert(source.container);
    }

    ir.nodes
        .iter()
        .filter(|node| {
            node.polarity == Polarity::Production
                && matches!(node.kind, NodeKind::Symbol | NodeKind::Type)
        })
        .filter(|node| {
            let Some(candidate_file) = placement(node.id.0) else {
                return false;
            };
            let Some(destination) = pass_start_file_by_candidate.get(&candidate_file).copied()
            else {
                return false;
            };
            if destination == node.container {
                return false;
            }

            let destination_has_dependency = outgoing_files
                .get(&node.id.0)
                .is_some_and(|files| files.contains(&destination));
            let destination_has_consumer = incoming_files
                .get(&node.id.0)
                .is_some_and(|files| files.contains(&destination));
            destination_has_dependency && !destination_has_consumer
        })
        .count()
        .try_into()
        .unwrap_or(u32::MAX)
}
