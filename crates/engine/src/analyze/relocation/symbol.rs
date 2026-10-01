use std::collections::{BTreeMap, BTreeSet};

use smol_str::SmolStr;
use strata_core::cluster::Partition;
use strata_core::graph::csr::Csr;
use strata_core::score::{Coefficients, KindWeights, score};
use strata_core::visibility::derive_visibility;
use strata_ir::{ContainerId, Edge, Node, NodeId, NodeKind, Polarity, ScopeLevel, Snapshot};

use crate::analyze::relocation::mirror::MirrorEvidence;
use crate::analyze::relocation::{
    CandidateTree, PipelineSolver, SYMBOL_MIN_IMPROVEMENT, SYMBOL_SWEEPS, SYMBOL_TARGETS,
};
use crate::analyze::scoring::{CycleCounts, move_distance, score_candidate};
use crate::config::CapacityConfig;
use crate::result::{SymbolKind, SymbolMove};

#[cfg(test)]
mod tests;

impl PipelineSolver<'_> {
    /// Sweeps symbols over existing files while preserving every structural
    /// veto and accepting only strict objective improvement.
    #[cfg(test)]
    fn symbol_polish(&self, parts: &Partition) -> SymbolOutcome {
        self.symbol_polish_with_mirror_evidence(parts, &MirrorEvidence::default())
    }

    pub(super) fn symbol_polish_with_mirror_evidence(
        &self,
        parts: &Partition,
        evidence: &MirrorEvidence,
    ) -> SymbolOutcome {
        let ir = self.snapshot.ir();
        let assembled = self.assemble_with_mirror_evidence(parts, evidence);
        let mut pass = SymbolPass::new_with_policy(
            PassInputs {
                snapshot: self.snapshot,
                coefficients: &self.coefficients,
                weights: &self.weights,
                same_file_symbol: self.same_file_symbol,
                same_file_type: self.same_file_type,
                capacity: self.capacity,
                assembled: &assembled,
                nodes: &ir.nodes,
                edges: &ir.edges,
            },
            RelocationPolicy {
                forbidden_sources: self.symbol_source_blocks(&assembled),
                forbidden_destinations: self.symbol_destination_blocks(&assembled),
                pin_test_polarity: self.pin_detected_test_symbols(),
                allow_cross_package: self.relocation_identity.allows_cross_package_moves(),
            },
        );
        pass.run();
        let overlay = pass.overlay;
        let relocations = pass.relocations;
        let placement = |id: u32| {
            overlay
                .get(&id)
                .copied()
                .or_else(|| assembled.placement.get(&id).copied())
        };
        let distance = move_distance(self.snapshot, &assembled.tree, &placement);
        let total = score(
            &score_candidate(
                self.snapshot,
                &placement,
                &assembled.pass_start_file_by_candidate,
                &assembled.tree,
                &assembled.namespace_by_file,
                distance,
                &self.capacity,
                self.same_file_symbol,
                self.same_file_type,
            ),
            &self.coefficients,
            &self.weights,
        )
        .total;
        SymbolOutcome {
            overlay,
            relocations,
            total,
        }
    }

    fn symbol_source_blocks(&self, assembled: &CandidateTree) -> BTreeSet<ContainerId> {
        let pass_start_names: BTreeMap<ContainerId, &str> = self
            .snapshot
            .ir()
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id, container.name.as_str()))
            .collect();
        assembled
            .pass_start_file_by_candidate
            .iter()
            .filter_map(|(candidate, pass_start)| {
                let test_pinned = self.pin_detected_test_symbols()
                    && assembled
                        .zone_by_file
                        .get(candidate)
                        .copied()
                        .unwrap_or(false);
                let path_pinned = pass_start_names
                    .get(pass_start)
                    .is_some_and(|path| self.forbidden_symbol_path(path));
                (test_pinned || path_pinned).then_some(*candidate)
            })
            .collect()
    }

    fn symbol_destination_blocks(&self, assembled: &CandidateTree) -> BTreeSet<ContainerId> {
        assembled
            .tree
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .filter(|container| self.forbidden_symbol_path(&container.name))
            .map(|container| container.id)
            .collect()
    }

    fn forbidden_symbol_path(&self, path: &str) -> bool {
        self.forbidden_symbol_files
            .iter()
            .any(|pattern| pattern.matches_path(std::path::Path::new(path)))
    }

    fn pin_detected_test_symbols(&self) -> bool {
        // `pin-detected-test-symbols` pins two things: declarations inside a
        // detected test zone, and test-polarity declarations anywhere, even
        // in a production file (ADR-22). Turning it off releases both.
        self.pin_test_symbols
    }
    /// Wraps a finished partition, re-pricing it at the true current tree when
    /// it converged back to the identity layout, so every identity entry in the
    /// pool carries one consistent score.
    /// Narrates the accepted symbol relocations as scored [`SymbolMove`] DTOs.
    ///
    /// `from_path` reads the current tree and `to_path` the assembled candidate.
    /// `broken_imports` counts distinct external callers or callees to repoint.
    pub(super) fn symbol_narrate(
        &self,
        assembled: &CandidateTree,
        symbols: &SymbolOutcome,
    ) -> Vec<SymbolMove> {
        if symbols.relocations.is_empty() {
            return Vec::new();
        }
        let ir = self.snapshot.ir();
        let base = &assembled.placement;
        let effective = |id: u32| -> Option<ContainerId> {
            symbols
                .overlay
                .get(&id)
                .copied()
                .or_else(|| base.get(&id).copied())
        };
        let current_name: BTreeMap<u32, &str> = ir
            .containers
            .containers()
            .iter()
            .map(|container| (container.id.0, container.name.as_str()))
            .collect();
        let candidate_name: BTreeMap<u32, &str> = assembled
            .tree
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id.0, container.name.as_str()))
            .collect();
        let kind_of = |node: &Node| match node.kind {
            strata_ir::NodeKind::Symbol => SymbolKind::Symbol,
            strata_ir::NodeKind::Type => SymbolKind::Type,
            strata_ir::NodeKind::FileBody => {
                unreachable!("file-body nodes are immobile and cannot appear in symbol narration")
            }
        };

        let by_node: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();
        let mut moves = Vec::with_capacity(symbols.relocations.len());
        for relocation in &symbols.relocations {
            let Some(symbol) = by_node.get(&relocation.node).copied() else {
                continue;
            };
            let Some(&home) = base.get(&relocation.node) else {
                continue;
            };
            debug_assert_eq!(
                home, relocation.from_file,
                "origin file is the symbol's base placement"
            );
            let mut severed: BTreeSet<ContainerId> = BTreeSet::new();
            for edge in &ir.edges {
                let neighbour = if edge.source.0 == relocation.node {
                    edge.target.0
                } else if edge.target.0 == relocation.node {
                    edge.source.0
                } else {
                    continue;
                };
                let Some(place) = effective(neighbour) else {
                    continue;
                };
                if place == relocation.to_file {
                    continue;
                }
                severed.insert(place);
            }
            moves.push(SymbolMove {
                symbol: symbol.name.to_string(),
                kind: kind_of(symbol),
                from_path: current_name
                    .get(&symbol.container.0)
                    .copied()
                    .unwrap_or_default()
                    .to_owned(),
                to_path: candidate_name
                    .get(&relocation.to_file.0)
                    .copied()
                    .unwrap_or_default()
                    .to_owned(),
                delta: -relocation.delta,
                broken_imports: u32::try_from(severed.len()).unwrap_or(u32::MAX),
            });
        }
        moves
    }
}

pub(in crate::analyze) struct SymbolRelocation {
    /// The relocated node's id.
    node: u32,
    /// The candidate file container the symbol leaves.
    from_file: ContainerId,
    /// The candidate file container the symbol joins.
    to_file: ContainerId,
    /// The objective improvement contributed (positive; J drops by this much).
    delta: f64,
}

/// The outcome of one symbol polish pass: the effective placement overlay, the
/// accepted relocations in acceptance order, and the final objective total.
pub(in crate::analyze) struct SymbolOutcome {
    /// Node id → candidate file container for relocated symbols only.
    pub(super) overlay: BTreeMap<u32, ContainerId>,
    /// Accepted relocations in acceptance order.
    relocations: Vec<SymbolRelocation>,
    /// The objective total after all accepted relocations.
    pub(super) total: f64,
}

impl SymbolOutcome {
    pub(super) fn preserves_namespaces(&self, assembled: &CandidateTree) -> bool {
        self.relocations.iter().all(|relocation| {
            assembled.shares_namespace(relocation.from_file, relocation.to_file)
                && self.overlay.get(&relocation.node) == Some(&relocation.to_file)
        })
    }
}

/// A reconstructed candidate tree plus the placement of every symbol node.
/// One deterministic run of the FIX08 symbol polish over an assembled tree.
///
/// Owns every piece of mutable trial state — the placement overlay, the
/// running-best score, the cycle and visibility baselines, the working
/// visibility copy of the node table, and the incremental per-file SLOC and
/// occupancy ledgers — so [`PipelineSolver::symbol_polish`] stays a thin
/// orchestrator. Determinism is structural: symbols sweep in ascending id
/// order, destinations rank by summed two-way priced pull with ties broken
/// toward the lower file id, and every tie elsewhere resolves to staying.
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
    fn new(
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
    fn invents_an_outbound_reach(&self, node: u32, destination: ContainerId) -> bool {
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
    fn invents_a_reach(
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

/// preserves dependency evidence from the layout a symbol pass started with
pub(in crate::analyze) struct PassStartGuard {
    claimant_files: BTreeMap<u32, BTreeSet<ContainerId>>,
    reachable: BTreeSet<(ContainerId, ContainerId)>,
}

impl PassStartGuard {
    fn new(base: &BTreeMap<u32, ContainerId>, edges: &[Edge]) -> Self {
        let mut claimant_files: BTreeMap<u32, BTreeSet<ContainerId>> = BTreeMap::new();
        let mut successors: BTreeMap<ContainerId, BTreeSet<ContainerId>> = BTreeMap::new();
        let files: BTreeSet<ContainerId> = base.values().copied().collect();

        for edge in edges {
            let (Some(source), Some(target)) = (base.get(&edge.source.0), base.get(&edge.target.0))
            else {
                continue;
            };
            if source == target {
                if edge.source != edge.target {
                    claimant_files
                        .entry(edge.target.0)
                        .or_default()
                        .insert(*source);
                }
                continue;
            }
            successors.entry(*source).or_default().insert(*target);
        }

        let mut reachable = BTreeSet::new();
        for start in files {
            let mut pending: Vec<ContainerId> = successors
                .get(&start)
                .into_iter()
                .flatten()
                .copied()
                .collect();
            while let Some(file) = pending.pop() {
                if !reachable.insert((start, file)) {
                    continue;
                }
                pending.extend(successors.get(&file).into_iter().flatten().copied());
            }
        }

        Self {
            claimant_files,
            reachable,
        }
    }

    fn blocks(&self, node: u32, destination: ContainerId) -> bool {
        self.claimant_files
            .get(&node)
            .into_iter()
            .flatten()
            .any(|claimant| self.reachable.contains(&(destination, *claimant)))
    }
}

/// Protects a declaration jointly consumed by sibling physical-folder
/// branches from being buried inside just one of those branches. Ownership is
/// derived once from pass-start paths and raw structural edges; unrelated
/// folder reach therefore cannot manufacture permission later in the pass.
pub(in crate::analyze) struct ConsumerBranchGuard {
    ownership_by_node: BTreeMap<u32, ConsumerBranchOwnership>,
    folder_by_candidate_file: BTreeMap<ContainerId, Vec<SmolStr>>,
}

pub(in crate::analyze) struct ConsumerBranchOwnership {
    lca: Vec<SmolStr>,
    occupied_branches: BTreeSet<SmolStr>,
}

impl ConsumerBranchGuard {
    fn new(assembled: &CandidateTree, snapshot: &Snapshot, edges: &[Edge]) -> Self {
        let pass_start_folder_by_file: BTreeMap<ContainerId, Vec<SmolStr>> = snapshot
            .ir()
            .containers
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .map(|container| (container.id, physical_folder_segments(&container.name)))
            .collect();
        let folder_by_candidate_file = assembled
            .pass_start_file_by_candidate
            .iter()
            .filter_map(|(&candidate, original)| {
                Some((candidate, pass_start_folder_by_file.get(original)?.clone()))
            })
            .collect();
        let nodes: BTreeMap<u32, &Node> = snapshot
            .ir()
            .nodes
            .iter()
            .map(|node| (node.id.0, node))
            .collect();
        let mut consumer_folders: BTreeMap<u32, BTreeSet<Vec<SmolStr>>> = BTreeMap::new();
        for edge in edges {
            if edge.source == edge.target {
                continue;
            }
            let Some(source) = nodes.get(&edge.source.0) else {
                continue;
            };
            let Some(folder) = pass_start_folder_by_file.get(&source.container) else {
                continue;
            };
            consumer_folders
                .entry(edge.target.0)
                .or_default()
                .insert(folder.clone());
        }

        let ownership_by_node = consumer_folders
            .into_iter()
            .filter_map(|(node, folders)| {
                let folders: Vec<Vec<SmolStr>> = folders.into_iter().collect();
                let first = folders.first()?;
                let lca_len = first
                    .iter()
                    .enumerate()
                    .take_while(|(index, segment)| {
                        folders
                            .iter()
                            .all(|folder| folder.get(*index) == Some(*segment))
                    })
                    .count();
                let occupied_branches: BTreeSet<SmolStr> = folders
                    .iter()
                    .filter_map(|folder| folder.get(lca_len).cloned())
                    .collect();
                (occupied_branches.len() >= 2).then_some((
                    node,
                    ConsumerBranchOwnership {
                        lca: first.get(..lca_len)?.to_vec(),
                        occupied_branches,
                    },
                ))
            })
            .collect();

        Self {
            ownership_by_node,
            folder_by_candidate_file,
        }
    }

    fn blocks(&self, node: u32, destination: ContainerId) -> bool {
        let Some(ownership) = self.ownership_by_node.get(&node) else {
            return false;
        };
        let Some(folder) = self.folder_by_candidate_file.get(&destination) else {
            return false;
        };
        folder.starts_with(&ownership.lca)
            && folder
                .get(ownership.lca.len())
                .is_some_and(|branch| ownership.occupied_branches.contains(branch))
    }

    /// Reports a destination inside the consumers' shared LCA but outside
    /// every occupied branch. Such a sibling is neutral shared territory, so
    /// the older pairwise reach guard must not mistake its absent branch edges
    /// for one consumer taking ownership from another.
    fn is_neutral_shared_destination(&self, node: u32, destination: ContainerId) -> bool {
        let Some(ownership) = self.ownership_by_node.get(&node) else {
            return false;
        };
        let Some(folder) = self.folder_by_candidate_file.get(&destination) else {
            return false;
        };
        folder.starts_with(&ownership.lca)
            && folder
                .get(ownership.lca.len())
                .is_some_and(|branch| !ownership.occupied_branches.contains(branch))
    }
}

pub(in crate::analyze) fn physical_folder_segments(file: &str) -> Vec<SmolStr> {
    file.rsplit_once('/').map_or_else(Vec::new, |(folder, _)| {
        folder
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(SmolStr::new)
            .collect()
    })
}

pub(in crate::analyze) struct SymbolPass<'a> {
    snapshot: &'a Snapshot,
    coefficients: &'a Coefficients,
    weights: &'a KindWeights,
    same_file_symbol: f64,
    same_file_type: f64,
    capacity: CapacityConfig,
    assembled: &'a CandidateTree,
    base: &'a BTreeMap<u32, ContainerId>,
    nodes: &'a [Node],
    edges: &'a [Edge],
    /// Both-direction priced incidence per node, computed once: an edge
    /// priced 0.0 never nominates a destination (FIX04).
    incident: BTreeMap<u32, Vec<(u32, f64)>>,
    /// Per-file production SLOC under the assembly, maintained
    /// incrementally across acceptances.
    sloc: BTreeMap<ContainerId, u32>,
    /// Per-file production occupancy, maintained alongside [`Self::sloc`].
    residents: BTreeMap<ContainerId, u32>,
    /// Per-file count of the production symbols the assembly placed there
    /// that have not yet left — arrivals never raise it (FIX12-A). The
    /// "no empty shells" guard reads this rather than [`Self::residents`],
    /// so an unrelated symbol moving *in* can never license draining the
    /// file's own last resident out.
    native: BTreeMap<ContainerId, u32>,
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
    /// Immutable pass-start origins whose declarations cannot leave.
    forbidden_sources: BTreeSet<ContainerId>,
    /// Candidate files that no declaration may enter.
    forbidden_destinations: BTreeSet<ContainerId>,
    /// Whether detected test declarations (`TestCase`/`TestSupport` polarity)
    /// are pinned wherever they live, per `pin-detected-test-symbols`.
    pin_test_polarity: bool,
    /// Whether declarations may leave their manifest package, per
    /// `allow-cross-package-moves` (ADR-0017).
    allow_cross_package: bool,
    /// Nodes already relocated in this pass. A symbol moves at most once per
    /// candidate (FIX12-C), so no reader is ever told two contradictory
    /// destinations for the same name.
    moved: BTreeSet<u32>,
    /// Dense vertex per candidate FILE, for the crossing graph.
    file_vertices: BTreeMap<ContainerId, u32>,
    /// Working copy of the node table whose containers track the overlay,
    /// so the visibility floor is derived over trial placements.
    visibility_nodes: Vec<Node>,
    /// Effective placement overrides accepted so far.
    overlay: BTreeMap<u32, ContainerId>,
    /// Running best objective value; acceptance must beat it by more than
    /// [`SYMBOL_MIN_IMPROVEMENT`].
    best: f64,
    /// Cycle baseline: relocation may raise neither cyclic dimension.
    cyclic_base: CycleCounts,
    /// Visibility baseline: relocation may never raise the finding count.
    vis_base: usize,
    relocations: Vec<SymbolRelocation>,
}

/// The read-only analysis inputs a [`SymbolPass`] prices placements against.
#[derive(Clone, Copy)]
struct PassInputs<'a> {
    snapshot: &'a Snapshot,
    coefficients: &'a Coefficients,
    weights: &'a KindWeights,
    same_file_symbol: f64,
    same_file_type: f64,
    capacity: CapacityConfig,
    assembled: &'a CandidateTree,
    nodes: &'a [Node],
    edges: &'a [Edge],
}

/// The admission rules of a [`SymbolPass`]: which files may not give or
/// receive a symbol, whether test-polarity declarations are pinned, and
/// whether the package wall is lifted.
#[derive(Default)]
struct RelocationPolicy {
    /// Files whose symbols may not leave.
    forbidden_sources: BTreeSet<ContainerId>,
    /// Files that may not receive a symbol.
    forbidden_destinations: BTreeSet<ContainerId>,
    /// Refuse any non-production-polarity declaration, wherever it lives.
    pin_test_polarity: bool,
    /// Allow a symbol to land in a file of another package.
    allow_cross_package: bool,
}

impl<'a> SymbolPass<'a> {
    #[allow(clippy::too_many_lines)]
    fn new_with_policy(inputs: PassInputs<'a>, policy: RelocationPolicy) -> Self {
        let PassInputs {
            snapshot,
            coefficients,
            weights,
            same_file_symbol,
            same_file_type,
            capacity,
            assembled,
            nodes,
            edges,
        } = inputs;
        let RelocationPolicy {
            forbidden_sources,
            forbidden_destinations,
            pin_test_polarity,
            allow_cross_package,
        } = policy;
        let base = &assembled.placement;
        let mut sloc: BTreeMap<ContainerId, u32> = BTreeMap::new();
        let mut residents: BTreeMap<ContainerId, u32> = BTreeMap::new();
        for node in nodes {
            if node.polarity != Polarity::Production {
                continue;
            }
            let Some(file) = base.get(&node.id.0).copied() else {
                continue;
            };
            *sloc.entry(file).or_insert(0) += node.effective_size;
            *residents.entry(file).or_insert(0) += 1;
        }
        let mut incident: BTreeMap<u32, Vec<(u32, f64)>> = BTreeMap::new();
        // FIX11: the test-zone tie-cut rides placement into symbol grain. A
        // node's zone is its placed file's mark; an edge with either endpoint
        // inside the zone prices to zero exactly as `build_file_graph` prices
        // it — the single-pricing choke point, mirrored so no grain disagrees
        // about what binds placement.
        let touches_zone = |node: u32| -> bool {
            base.get(&node)
                .and_then(|file| assembled.zone_by_file.get(file))
                .copied()
                .unwrap_or(false)
        };
        let node_by_id: BTreeMap<u32, &Node> = nodes.iter().map(|node| (node.id.0, node)).collect();
        for edge in edges {
            let affinity = match (
                node_by_id.get(&edge.source.0),
                node_by_id.get(&edge.target.0),
            ) {
                (Some(source), Some(target)) if source.container == target.container => {
                    if source.kind == NodeKind::Type || target.kind == NodeKind::Type {
                        same_file_type
                    } else {
                        same_file_symbol
                    }
                }
                _ => 1.0,
            };
            let weight = weights.edge_weight(edge.kind, edge.confidence) * affinity;
            if weight <= 0.0 || touches_zone(edge.source.0) || touches_zone(edge.target.0) {
                continue;
            }
            incident
                .entry(edge.source.0)
                .or_default()
                .push((edge.target.0, weight));
            incident
                .entry(edge.target.0)
                .or_default()
                .push((edge.source.0, weight));
        }
        if coefficients.companion_separation > 0.0 {
            for affinity in snapshot
                .ir()
                .affinities
                .iter()
                .copied()
                .collect::<BTreeSet<_>>()
            {
                incident
                    .entry(affinity.companion.0)
                    .or_default()
                    .push((affinity.owner.0, coefficients.companion_separation));
            }
        }
        let reach = ReachGuard::new(assembled, base, edges, weights, &touches_zone);
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
        let file_vertices: BTreeMap<ContainerId, u32> = assembled
            .tree
            .containers()
            .iter()
            .filter(|container| container.level == ScopeLevel::File)
            .enumerate()
            .map(|(index, container)| (container.id, u32::try_from(index).unwrap_or(u32::MAX)))
            .collect();
        let native = residents.clone();
        let mut pass = Self {
            snapshot,
            coefficients,
            weights,
            same_file_symbol,
            same_file_type,
            capacity,
            assembled,
            base,
            nodes,
            edges,
            incident,
            reach,
            consumer_branches,
            type_only_files,
            file_depth,
            pass_start,
            forbidden_sources,
            forbidden_destinations,
            pin_test_polarity,
            allow_cross_package,
            sloc,
            residents,
            native,
            moved: BTreeSet::new(),
            file_vertices,
            visibility_nodes: nodes.to_vec(),
            overlay: BTreeMap::new(),
            best: 0.0,
            cyclic_base: CycleCounts::default(),
            vis_base: 0,
            relocations: Vec::new(),
        };
        pass.best = pass.score_with(&pass.overlay);
        let crossing = pass.crossing_csr();
        pass.cyclic_base = CycleCounts::from_graph(&crossing);
        pass.vis_base = pass.refresh_visibility();
        pass
    }

    #[cfg(test)]
    fn new(inputs: PassInputs<'a>) -> Self {
        Self::new_with_policy(inputs, RelocationPolicy::default())
    }

    /// Sweeps every symbol in ascending id order, at most
    /// [`SYMBOL_SWEEPS`] times, stopping early once a sweep relocates
    /// nothing.
    fn run(&mut self) {
        for _ in 0..SYMBOL_SWEEPS {
            let mut improved = false;
            for node in self.nodes {
                improved |= self.try_relocate(node);
            }
            if !improved {
                break;
            }
        }
    }

    /// Ranks candidate destination files for `node` by summed two-way priced
    /// pull from their *base-placed* residents (FIX12-B); strongest first, ties
    /// toward the lower id, capped at [`SYMBOL_TARGETS`]. For a type, a
    /// destination must exert more pull than its pass-start file, so
    /// repository-wide objective normalization cannot trade a strong local
    /// type affinity away for an unrelated global improvement. Runtime
    /// symbols retain their established 1x admission behavior.
    ///
    /// Reading the overlay here would let a symbol chase a neighbour that moved
    /// earlier in the same pass, nominating a destination justified by nothing
    /// but another suggestion.
    fn nominate(&self, node: &Node, source_file: ContainerId) -> Vec<ContainerId> {
        let mut pull: BTreeMap<ContainerId, f64> = BTreeMap::new();
        let mut source_pull = 0.0;
        for &(neighbour, weight) in self.incident.get(&node.id.0).into_iter().flatten() {
            if neighbour == node.id.0 {
                continue;
            }
            let Some(place) = self.base.get(&neighbour).copied() else {
                continue;
            };
            if place == source_file {
                source_pull += weight;
                continue;
            }
            *pull.entry(place).or_insert(0.0) += weight;
        }
        let mut ranked: Vec<(ContainerId, f64)> = pull
            .into_iter()
            .filter(|(_, destination_pull)| {
                node.kind != NodeKind::Type || *destination_pull > source_pull
            })
            .collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.0.cmp(&right.0))
        });
        ranked
            .into_iter()
            .take(SYMBOL_TARGETS)
            .map(|(destination, _)| destination)
            .collect()
    }

    /// Placement of one node: the overlay wins, the assembly fills the rest.
    fn effective(&self, id: u32) -> Option<ContainerId> {
        self.overlay
            .get(&id)
            .copied()
            .or_else(|| self.base.get(&id).copied())
    }

    fn collides_in_destination(&self, node: &Node, destination: ContainerId) -> bool {
        self.nodes.iter().any(|resident| {
            resident.id != node.id
                && resident.name == node.name
                && self.effective(resident.id.0) == Some(destination)
        })
    }

    /// Full objective value of the layout `state` describes.
    fn score_with(&self, state: &BTreeMap<u32, ContainerId>) -> f64 {
        let placement = |id: u32| {
            state
                .get(&id)
                .copied()
                .or_else(|| self.base.get(&id).copied())
        };
        let distance = move_distance(self.snapshot, &self.assembled.tree, &placement);
        let candidate = score_candidate(
            self.snapshot,
            &placement,
            &self.assembled.pass_start_file_by_candidate,
            &self.assembled.tree,
            &self.assembled.namespace_by_file,
            distance,
            &self.capacity,
            self.same_file_symbol,
            self.same_file_type,
        );
        score(&candidate, self.coefficients, self.weights).total
    }

    /// The crossing graph over candidate FILES induced by the current
    /// overlay, condensed-ready exactly as the file polish builds its
    /// quotient.
    fn crossing_csr(&self) -> Csr {
        let mut pairs: BTreeSet<(u32, u32)> = BTreeSet::new();
        for edge in self.edges {
            let (Some(source), Some(target)) =
                (self.effective(edge.source.0), self.effective(edge.target.0))
            else {
                continue;
            };
            if source == target {
                continue;
            }
            let (Some(source), Some(target)) = (
                self.file_vertices.get(&source),
                self.file_vertices.get(&target),
            ) else {
                continue;
            };
            pairs.insert((*source, *target));
        }
        let sorted: Vec<(u32, u32)> = pairs.into_iter().collect();
        Csr::from_sorted_edges(self.file_vertices.len(), &sorted)
    }

    /// Applies the whole overlay to the working visibility copy, then
    /// counts findings — the initial baseline path.
    fn refresh_visibility(&mut self) -> usize {
        for node in &mut self.visibility_nodes {
            if let Some(file) = self.overlay.get(&node.id.0)
                && node.container != *file
            {
                node.container = *file;
            }
        }
        self.count_findings()
    }

    /// Points one node's working-copy container at `container`.
    fn set_visible(&mut self, id: u32, container: ContainerId) {
        for node in &mut self.visibility_nodes {
            if node.id == NodeId(id) {
                node.container = container;
            }
        }
    }

    /// Visibility finding count over the working copy as it stands.
    fn count_findings(&self) -> usize {
        derive_visibility(&self.assembled.tree, &self.visibility_nodes, self.edges)
            .findings
            .len()
    }

    /// Offers one symbol its strongest-pulling destinations under the full
    /// veto family; records an accepted relocation and returns whether the
    /// sweep made progress.
    #[allow(clippy::too_many_lines)]
    fn try_relocate(&mut self, node: &Node) -> bool {
        if node.kind == NodeKind::FileBody {
            return false;
        }
        // One move per symbol per candidate (FIX12-C): a second relocation
        // would narrate the same name twice with contradictory destinations.
        if self.moved.contains(&node.id.0) {
            return false;
        }
        // A detected test declaration stays pinned by its own polarity, not
        // only by its file's zone: a `#[cfg(test)]` helper or `#[test]` case
        // living in a production file would otherwise be free to move.
        if self.pin_test_polarity && node.polarity != Polarity::Production {
            return false;
        }
        let Some(source_file) = self.effective(node.id.0) else {
            return false;
        };
        if self.forbidden_sources.contains(&source_file) {
            return false;
        }
        for destination in self.nominate(node, source_file) {
            if self.forbidden_destinations.contains(&destination) {
                continue;
            }
            // No empty shells: the origin keeps at least one of the
            // production symbols the assembly placed there. Counted over
            // `native`, so an arrival cannot unlock the drain (FIX12-A).
            if self.native.get(&source_file).copied().unwrap_or(0) <= 1 {
                break;
            }
            // A declaration never leaves its manifest package unless the
            // profile lifts the wall (ADR-0017): a move across crates changes
            // a package's public surface and manifest dependencies.
            if !self.assembled.shares_namespace(source_file, destination)
                || (!self.allow_cross_package
                    && !self.assembled.shares_package(source_file, destination))
            {
                continue;
            }
            if self.file_depth.get(&destination).copied().unwrap_or(0)
                > self.file_depth.get(&source_file).copied().unwrap_or(0)
            {
                continue;
            }
            if self.collides_in_destination(node, destination)
                || self.consumer_branches.blocks(node.id.0, destination)
            {
                continue;
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
                continue;
            }
            // FIX13: never hand one of the symbol's dependants a folder it
            // does not already depend on (see `ReachGuard`).
            if self
                .reach
                .invents_a_reach(node.id.0, source_file, destination)
                && !self
                    .consumer_branches
                    .is_neutral_shared_destination(node.id.0, destination)
            {
                continue;
            }
            if node.kind == NodeKind::Symbol && self.type_only_files.contains(&destination) {
                continue;
            }
            if self.reach.invents_an_outbound_reach(node.id.0, destination) {
                continue;
            }
            if self.pass_start.blocks(node.id.0, destination) {
                continue;
            }
            // SLOC cap on the destination, priced in production SLOC.
            let destination_sloc = self.sloc.get(&destination).copied().unwrap_or(0);
            let moving_sloc =
                (node.polarity == Polarity::Production).then_some(node.effective_size);
            if let Some(size) = moving_sloc
                && destination_sloc.saturating_add(size) > self.capacity.file
            {
                continue;
            }

            // Tentatively relocate, then run the structural vetoes.
            let previous = self.overlay.insert(node.id.0, destination);
            let crossing = self.crossing_csr();
            let cyclic_now = CycleCounts::from_graph(&crossing);
            if cyclic_now.exceeds(self.cyclic_base) {
                Self::undo(&mut self.overlay, node.id.0, previous);
                continue;
            }
            self.set_visible(node.id.0, destination);
            let vis_now = self.count_findings();
            if vis_now > self.vis_base {
                Self::undo(&mut self.overlay, node.id.0, previous);
                self.set_visible(node.id.0, source_file);
                continue;
            }

            let total = self.score_with(&self.overlay);
            // Strict improvement with a real margin: float-dust gains are
            // rejected, not accepted (see SYMBOL_MIN_IMPROVEMENT).
            if self.best - total > SYMBOL_MIN_IMPROVEMENT {
                let delta = self.best - total;
                self.best = total;
                self.cyclic_base = cyclic_now;
                self.vis_base = vis_now;
                if node.polarity == Polarity::Production {
                    if let Some(slot) = self.sloc.get_mut(&source_file) {
                        *slot = slot.saturating_sub(node.effective_size);
                    }
                    *self.sloc.entry(destination).or_insert(0) += node.effective_size;
                    if let Some(slot) = self.residents.get_mut(&source_file) {
                        *slot = slot.saturating_sub(1);
                    }
                    *self.residents.entry(destination).or_insert(0) += 1;
                    // A departure lowers the origin's native count; the
                    // arrival deliberately does not raise the destination's.
                    if let Some(slot) = self.native.get_mut(&source_file) {
                        *slot = slot.saturating_sub(1);
                    }
                }
                self.moved.insert(node.id.0);
                self.relocations.push(SymbolRelocation {
                    node: node.id.0,
                    from_file: source_file,
                    to_file: destination,
                    delta,
                });
                return true;
            }
            // Rejected: undo the tentative relocation.
            Self::undo(&mut self.overlay, node.id.0, previous);
            self.set_visible(node.id.0, source_file);
        }
        false
    }

    /// Restores the prior overlay entry for one node.
    fn undo(overlay: &mut BTreeMap<u32, ContainerId>, id: u32, previous: Option<ContainerId>) {
        match previous {
            Some(place) => {
                overlay.insert(id, place);
            }
            None => {
                overlay.remove(&id);
            }
        }
    }
}
