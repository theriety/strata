use std::collections::BTreeMap;

use strata_core::visibility::derive_visibility;
use strata_ir::{ContainerId, Edge, Node, NodeId};

use crate::analyze::relocation::CandidateTree;
use crate::analyze::relocation::symbol::inputs::PassInputs;

/// The visibility floor of a symbol pass: a working copy of the node table that
/// follows trial placements, and the finding count a relocation may never
/// raise.
pub(super) struct VisibilityTracker<'a> {
    assembled: &'a CandidateTree,
    edges: &'a [Edge],
    /// Working copy of the node table, seeded from the base placement (the
    /// tree the pass runs on, not the node table's current container ids)
    /// and tracking the overlay, so the visibility floor is derived over
    /// trial placements in the same arena the veto reads.
    visibility_nodes: Vec<Node>,
    /// Visibility baseline: relocation may never raise the finding count.
    pub(super) vis_base: usize,
}

impl<'a> VisibilityTracker<'a> {
    pub(super) fn new(inputs: PassInputs<'a>) -> Self {
        let PassInputs {
            assembled,
            nodes,
            edges,
            ..
        } = inputs;
        let base = &assembled.placement;
        Self {
            assembled,
            edges,
            visibility_nodes: nodes
                .iter()
                .map(|node| Node {
                    container: base.get(&node.id.0).copied().unwrap_or(node.container),
                    ..node.clone()
                })
                .collect(),
            vis_base: 0,
        }
    }

    /// Applies the whole overlay to the working visibility copy, then
    /// counts findings — the initial baseline path.
    pub(super) fn refresh_visibility(&mut self, overlay: &BTreeMap<u32, ContainerId>) -> usize {
        for node in &mut self.visibility_nodes {
            if let Some(file) = overlay.get(&node.id.0)
                && node.container != *file
            {
                node.container = *file;
            }
        }
        self.count_findings()
    }

    /// Points one node's working-copy container at `container`.
    pub(super) fn set_visible(&mut self, id: u32, container: ContainerId) {
        for node in &mut self.visibility_nodes {
            if node.id == NodeId(id) {
                node.container = container;
            }
        }
    }

    /// Visibility finding count over the working copy as it stands.
    pub(super) fn count_findings(&self) -> usize {
        derive_visibility(&self.assembled.tree, &self.visibility_nodes, self.edges)
            .findings
            .len()
    }
}
