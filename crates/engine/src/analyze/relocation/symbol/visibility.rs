use std::collections::BTreeMap;

use strata_core::visibility::PreparedVisibility;
use strata_ir::{ContainerId, Node};

use crate::analyze::relocation::symbol::inputs::PassInputs;

/// The visibility floor of a symbol pass: a working placement of the node table
/// that follows trial placements, and the finding count a relocation may never
/// raise.
pub(super) struct VisibilityTracker {
    /// The tree, edges and node table the pass derives visibility over, resolved
    /// once; only the working placement changes between queries.
    prepared: PreparedVisibility,
    /// Working container of every node, parallel to the node table. Seeded from
    /// the base placement (the tree the pass runs on, not the node table's
    /// current container ids) and tracking the overlay, so the visibility floor
    /// is derived over trial placements in the same arena the veto reads.
    containers: Vec<ContainerId>,
    /// Position in the node table of each raw node id, `None` when absent.
    position_of: Vec<Option<usize>>,
    /// Visibility baseline: relocation may never raise the finding count.
    pub(super) vis_base: usize,
}

impl VisibilityTracker {
    pub(super) fn new(inputs: PassInputs<'_>) -> Self {
        let PassInputs {
            assembled,
            nodes,
            edges,
            ..
        } = inputs;
        let base = &assembled.placement;
        Self {
            prepared: PreparedVisibility::new(&assembled.tree, nodes, edges),
            containers: nodes
                .iter()
                .map(|node| base.get(&node.id.0).copied().unwrap_or(node.container))
                .collect(),
            position_of: position_table(nodes),
            vis_base: 0,
        }
    }

    /// Applies the whole overlay to the working placement, then counts
    /// findings — the initial baseline path.
    ///
    /// `nodes` must be the table this tracker was built from: containers are
    /// matched to nodes by position.
    pub(super) fn refresh_visibility(
        &mut self,
        nodes: &[Node],
        overlay: &BTreeMap<u32, ContainerId>,
    ) -> usize {
        debug_assert_eq!(nodes.len(), self.containers.len());
        for (node, container) in nodes.iter().zip(&mut self.containers) {
            if let Some(file) = overlay.get(&node.id.0) {
                *container = *file;
            }
        }
        self.count_findings()
    }

    /// Points one node's working container at `container`.
    pub(super) fn set_visible(&mut self, id: u32, container: ContainerId) {
        let slot = self
            .position_of
            .get(id as usize)
            .copied()
            .flatten()
            .and_then(|position| self.containers.get_mut(position));
        if let Some(slot) = slot {
            *slot = container;
        }
    }

    /// Visibility finding count over the working placement as it stands.
    pub(super) fn count_findings(&self) -> usize {
        self.prepared.finding_count(&self.containers)
    }
}

/// Maps each raw node id to its position in `nodes`.
fn position_table(nodes: &[Node]) -> Vec<Option<usize>> {
    let max_id = nodes.iter().map(|node| node.id.0 as usize).max();
    let mut table = vec![None; max_id.map_or(0, |id| id + 1)];
    for (position, node) in nodes.iter().enumerate() {
        if let Some(cell) = table.get_mut(node.id.0 as usize) {
            *cell = Some(position);
        }
    }
    table
}
