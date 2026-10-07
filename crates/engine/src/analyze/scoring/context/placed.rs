//! One evaluation's dense view of a placement function.

use strata_ir::ContainerId;

/// The dense view of one placement: the candidate file each node id occupies.
#[derive(Clone)]
pub(in crate::analyze) struct Placed {
    pub(super) files: Vec<Option<ContainerId>>,
    /// One past the largest container id any node occupies.
    pub(super) slots: usize,
}

impl Placed {
    pub(super) fn file(&self, node: u32) -> Option<ContainerId> {
        self.files.get(node as usize).copied().flatten()
    }

    /// Places `node` in `file`, replacing wherever it was.
    pub(in crate::analyze) fn set(&mut self, node: u32, file: ContainerId) {
        if let Some(cell) = self.files.get_mut(node as usize) {
            *cell = Some(file);
            self.slots = self.slots.max(file.0 as usize + 1);
        }
    }
}
