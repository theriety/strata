use std::collections::{BTreeMap, BTreeSet};

use strata_ir::{ContainerId, Node, NodeKind, ScopeLevel};

use crate::analyze::relocation::CandidateTree;
use crate::analyze::relocation::symbol::guards::{ConsumerBranchGuard, PassStartGuard};
use crate::analyze::relocation::symbol::inputs::{PassInputs, RelocationPolicy};
use crate::analyze::relocation::symbol::ledger::Ledger;
use crate::analyze::relocation::symbol::reach::ReachGuard;

/// The structural vetoes of a symbol pass: policy pins, the reach and
/// consumer-branch guards, and the destination-specific checks every
/// relocation obeys, ordinary or folded (ADR-21). Everything here reads
/// immutable pass-start facts, plus the ledger's overlay for name collisions;
/// capacity and the objective stay with the caller.
pub(super) struct Admission<'a> {
    /// Files that may not give or receive a symbol, and the test-polarity and
    /// package-wall switches.
    pub(super) policy: RelocationPolicy,
    assembled: &'a CandidateTree,
    nodes: &'a [Node],
    /// Refuses relocations that would hand a third party a folder it never
    /// depended on (FIX13).
    reach: ReachGuard,
    /// Refuses burying a jointly consumed declaration in one occupied sibling
    /// branch, independent of incidental pass-start reach between branches.
    consumer_branches: ConsumerBranchGuard,
    /// Files containing at least one type and no runtime symbol at pass start.
    type_only_files: BTreeSet<ContainerId>,
    /// Physical pass-start directory depth of every candidate file.
    file_depth: BTreeMap<ContainerId, usize>,
    /// Refuses relocations that would close a path back to a dependant's
    /// pass-start file after an earlier move drained that dependant away.
    pass_start: PassStartGuard,
}

impl<'a> Admission<'a> {
    pub(super) fn new(
        inputs: PassInputs<'a>,
        policy: RelocationPolicy,
        touches_zone: &dyn Fn(u32) -> bool,
    ) -> Self {
        let PassInputs {
            snapshot,
            weights,
            assembled,
            nodes,
            edges,
            ..
        } = inputs;
        let base = &assembled.placement;
        let reach = ReachGuard::new(assembled, base, edges, weights, touches_zone);
        let consumer_branches = ConsumerBranchGuard::new(assembled, snapshot, edges);
        let mut file_roles: BTreeMap<ContainerId, (bool, bool)> = BTreeMap::new();
        for node in nodes {
            let Some(&file) = base.get(&node.id.0) else {
                continue;
            };
            let role = file_roles.entry(file).or_default();
            role.0 |= node.kind == NodeKind::Type;
            role.1 |= node.kind != NodeKind::Type;
        }
        let type_only_files = file_roles
            .into_iter()
            .filter_map(|(file, (has_type, has_symbol))| (has_type && !has_symbol).then_some(file))
            .collect();
        let file_depth = assembled
            .tree
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| {
                let depth = container
                    .name
                    .rsplit_once('/')
                    .map_or(0, |(directory, _)| directory.split('/').count());
                (container.id, depth)
            })
            .collect();
        let pass_start = PassStartGuard::new(base, edges);
        Self {
            policy,
            assembled,
            nodes,
            reach,
            consumer_branches,
            type_only_files,
            file_depth,
            pass_start,
        }
    }

    fn collides_in_destination(
        &self,
        node: &Node,
        destination: ContainerId,
        ledger: &Ledger<'_>,
    ) -> bool {
        self.nodes.iter().any(|resident| {
            resident.id != node.id
                && resident.name == node.name
                && ledger.effective(resident.id.0) == Some(destination)
        })
    }

    /// Whether a relocation may take `node` from `source_file` to
    /// `destination`: every structural veto ([`Self::admits`]) plus the two
    /// reach guards (FIX13, ADR-6). Ordinary relocations and collision folds
    /// (ADR-21) both pass through here, and no profile, key or flag reaches the
    /// guards.
    ///
    /// The inbound guard is defined as: never give a dependant a folder it
    /// does not already depend on, except a neutral unoccupied sibling branch
    /// under the consumers' common ancestor (ADR-11). That carve-out is part of
    /// the guard's definition, not a switch: it is fixed, configuration cannot
    /// widen or narrow it, and the outbound guard has none.
    pub(super) fn admits_nominated(
        &self,
        node: &Node,
        source_file: ContainerId,
        destination: ContainerId,
        ledger: &Ledger<'_>,
    ) -> bool {
        if !self.admits(node, source_file, destination, ledger) {
            return false;
        }
        // FIX13: never hand one of the symbol's dependants a folder it
        // does not already depend on (see `ReachGuard`), except a neutral
        // unoccupied sibling branch under the consumers' common ancestor
        // (ADR-11), the guard's one defined carve-out (ADR-6).
        if self
            .reach
            .invents_a_reach(node.id.0, source_file, destination)
            && !self
                .consumer_branches
                .is_neutral_shared_destination(node.id.0, destination)
        {
            return false;
        }
        if self.reach.invents_an_outbound_reach(node.id.0, destination) {
            return false;
        }
        true
    }

    /// The destination-specific structural vetoes every symbol relocation
    /// obeys, ordinary or folded (ADR-21): policy pins, namespace and package,
    /// depth, name collision, consumer branches, the test-zone boundary, file
    /// role, and pass-start dependency order. Capacity and the objective are
    /// checked by the caller.
    fn admits(
        &self,
        node: &Node,
        source_file: ContainerId,
        destination: ContainerId,
        ledger: &Ledger<'_>,
    ) -> bool {
        if self.policy.forbidden_destinations.contains(&destination) {
            return false;
        }
        // A declaration never leaves its manifest package unless the
        // profile lifts the wall (ADR-17): a move across crates changes
        // a package's public surface and manifest dependencies.
        if !self.assembled.shares_namespace(source_file, destination)
            || (!self.policy.allow_cross_package
                && !self.assembled.shares_package(source_file, destination))
        {
            return false;
        }
        if self.file_depth.get(&destination).copied().unwrap_or(0)
            > self.file_depth.get(&source_file).copied().unwrap_or(0)
        {
            return false;
        }
        if self.collides_in_destination(node, destination, ledger)
            || self.consumer_branches.blocks(node.id.0, destination)
        {
            return false;
        }
        // FIX11 source/test boundary: a relocation whose origin and
        // destination sit on opposite sides of the test zone is barred
        // outright, in either direction. This static check runs before any
        // evaluation; nomination should already keep zone files out of
        // `ranked` because the incidence map zero-prices every
        // zone-touching edge, so the veto is defense in depth against
        // future nomination paths. Every emitted file gets a zone entry,
        // so the map is empty only on the no-files early return — where
        // placement is empty and `try_relocate` declines before ever
        // reaching this check; lookups nonetheless default to false on
        // both sides.
        if self
            .assembled
            .zone_by_file
            .get(&source_file)
            .copied()
            .unwrap_or(false)
            != self
                .assembled
                .zone_by_file
                .get(&destination)
                .copied()
                .unwrap_or(false)
        {
            return false;
        }
        if node.kind == NodeKind::Symbol && self.type_only_files.contains(&destination) {
            return false;
        }
        if self.pass_start.blocks(node.id.0, destination) {
            return false;
        }
        true
    }
}
