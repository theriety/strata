use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::score::KindWeights;
use strata_ir::{ContainerId, Edge, ScopeLevel};

use crate::analyze::relocation::CandidateTree;

/// Refuses a symbol relocation that would either force an existing dependant
/// to reach a new folder or give the destination folder a new outbound
/// dependency (FIX13).
///
/// The objective cannot see this. A type two sibling adapters share costs the
/// same in a neutral `adapters/types/` file as buried inside
/// `adapters/anthropic/`: both homes cross at `adapters` and sit at the same
/// depth, so `cut_cost` and every other term rate them identically. What
/// separates them is direction: folding the type into one sharer can make the
/// other sharer reach through new territory, while moving runtime behavior can
/// make its destination reach a new dependency. No term prices either
/// directional change. Both refusals therefore read immutable pass-start
/// folder edges upstream of scoring, exactly as the source/test boundary does
/// (FIX11 D-3), rather than becoming terms the other six can outvote.
///
/// Measured on `~/Repositories/ai`, across both modes: 609 suggested moves
/// became 547, and the 67 that buried one production module's symbol inside
/// another — `openai -> anthropic`, `google -> openai` — became 0. Moves
/// inventing any inbound module edge fell 129 -> 52; every survivor has a
/// `spec/` module as the reaching party, which FIX11 zero-prices by design.
pub(in crate::analyze) struct ReachGuard {
    /// The folders that already depend on each node, read off the base
    /// placement: the parties a relocation could newly inconvenience.
    dependant_folders: BTreeMap<u32, Vec<ContainerId>>,
    /// Owning folder of each file container — the grain a dependency between
    /// modules is actually written at, so a move inside one folder rearranges
    /// nothing a dependant can see.
    owner: BTreeMap<ContainerId, ContainerId>,
    /// Folder-to-folder dependencies the base placement already carries. A
    /// pair absent here is a coupling the move would invent.
    owner_edges: BTreeSet<(u32, u32)>,
    /// Folder-to-folder dependencies carried by every structural edge at pass
    /// start. Unlike `owner_edges`, this envelope is admission policy rather
    /// than priced incidence, so zero-priced edges still constrain arrivals.
    outbound_owner_edges: BTreeSet<(u32, u32)>,
    /// Pass-start target folders reached by each node's outbound edges.
    outbound_targets: BTreeMap<u32, Vec<ContainerId>>,
    /// Slash-keyed folder identities. The internal tree keeps one flat folder
    /// per real directory and only the render boundary nests them (see
    /// [`nest_folder_segments`]), so directory ancestry lives in this key
    /// rather than in the parent chain.
    folder_key: BTreeMap<u32, SmolStr>,
}

impl ReachGuard {
    /// Reads the base placement once into the folder-grain view the veto asks
    /// its questions of.
    ///
    /// `touches_zone` is the caller's test-zone predicate, applied to the same
    /// edges on the same terms as the incidence map, so no grain disagrees
    /// about which edges bind placement.
    pub(super) fn new(
        assembled: &CandidateTree,
        base: &BTreeMap<u32, ContainerId>,
        edges: &[Edge],
        weights: &KindWeights,
        touches_zone: &dyn Fn(u32) -> bool,
    ) -> Self {
        let containers = assembled.tree.containers();
        let folder_key = containers
            .iter()
            .filter(|container| container.level == ScopeLevel::Folder)
            .map(|container| (container.id.0, container.name.clone()))
            .collect();
        let owner: BTreeMap<ContainerId, ContainerId> = containers
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .filter_map(|container| Some((container.id, container.parent?)))
            .collect();
        let home = |node: u32| base.get(&node).and_then(|file| owner.get(file)).copied();

        let mut dependant_folders: BTreeMap<u32, Vec<ContainerId>> = BTreeMap::new();
        let mut owner_edges: BTreeSet<(u32, u32)> = BTreeSet::new();
        let mut outbound_owner_edges: BTreeSet<(u32, u32)> = BTreeSet::new();
        let mut outbound_targets: BTreeMap<u32, Vec<ContainerId>> = BTreeMap::new();
        for edge in edges {
            if edge.source != edge.target
                && !touches_zone(edge.source.0)
                && !touches_zone(edge.target.0)
                && let (Some(source), Some(target)) = (home(edge.source.0), home(edge.target.0))
            {
                outbound_targets
                    .entry(edge.source.0)
                    .or_default()
                    .push(target);
                if source != target {
                    outbound_owner_edges.insert((source.0, target.0));
                }
            }
            let weight = weights.edge_weight(edge.kind, edge.confidence);
            if weight <= 0.0 || touches_zone(edge.source.0) || touches_zone(edge.target.0) {
                continue;
            }
            let Some(dependant) = home(edge.source.0) else {
                continue;
            };
            dependant_folders
                .entry(edge.target.0)
                .or_default()
                .push(dependant);
            if let Some(target) = home(edge.target.0)
                && dependant != target
            {
                owner_edges.insert((dependant.0, target.0));
            }
        }
        Self {
            dependant_folders,
            owner,
            owner_edges,
            outbound_owner_edges,
            outbound_targets,
            folder_key,
        }
    }

    /// Reports whether an arrival would give its destination folder an
    /// outbound dependency absent from the pass-start folder graph.
    pub(super) fn invents_an_outbound_reach(&self, node: u32, destination: ContainerId) -> bool {
        let Some(&into) = self.owner.get(&destination) else {
            return false;
        };
        self.outbound_targets
            .get(&node)
            .into_iter()
            .flatten()
            .any(|&target| {
                target != into && !self.outbound_owner_edges.contains(&(into.0, target.0))
            })
    }

    /// Reports whether moving `node` from `source_file` to `destination` would
    /// invent a folder-to-folder dependency for one of its dependants.
    ///
    /// A move *up* — into a folder that already contains the origin — is
    /// exempt: hoisting a symbol toward its dependants' common ground is the
    /// direction this guard exists to protect, and refusing it would block the
    /// genuine consolidations measured alongside the burials.
    ///
    /// This check covers inbound reach only: it protects third parties from a
    /// new neighbour. [`Self::invents_an_outbound_reach`] independently checks
    /// the destination's outbound envelope, without applying this method's
    /// ancestry exemption.
    pub(super) fn invents_a_reach(
        &self,
        node: u32,
        source_file: ContainerId,
        destination: ContainerId,
    ) -> bool {
        let (Some(&from), Some(&into)) =
            (self.owner.get(&source_file), self.owner.get(&destination))
        else {
            return false;
        };
        if from == into || self.encloses(into, from) {
            return false;
        }
        self.dependant_folders
            .get(&node)
            .into_iter()
            .flatten()
            .any(|&home| home != into && !self.owner_edges.contains(&(home.0, into.0)))
    }

    /// Reports whether the real directory `ancestor` names contains the one
    /// `descendant` names — the hoist exemption in [`Self::invents_a_reach`].
    ///
    /// A folder is not its own ancestor: a move inside one folder never reaches
    /// this test.
    fn encloses(&self, ancestor: ContainerId, descendant: ContainerId) -> bool {
        let (Some(outer), Some(inner)) = (
            self.folder_key.get(&ancestor.0),
            self.folder_key.get(&descendant.0),
        ) else {
            return false;
        };
        inner.starts_with(outer.as_str()) && inner.as_bytes().get(outer.len()) == Some(&b'/')
    }
}
