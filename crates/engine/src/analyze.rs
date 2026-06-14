//! The pure analysis pass: snapshot plus config in, [`AnalyzeResult`] out.
//!
//! [`analyze`] performs no I/O and holds no global state, so an identical
//! snapshot, config, and seed always produce an identical result (AD-5). It runs
//! the full restructuring pipeline — SCC condensation, longest-path layering,
//! multilevel acyclic clustering, scoring, and multi-start diversification — to
//! return up to `k` genuinely different candidate layouts per requested mode, each
//! narrated against the current tree. Alongside the candidates it derives the
//! current tree's structural violations (cycles, polarity breaches, over-exports)
//! and scores the current layout.

use std::collections::BTreeMap;

use strata_core::cluster::coarsen::coarsen_chain;
use strata_core::cluster::refine::{GainFn, refine};
use strata_core::cluster::seed::{SeedLevel, seed};
use strata_core::cluster::{ClusterId, LevelCaps, Partition};
use strata_core::condense::{Condensation, condense};
use strata_core::diversify::{ModeConfig, ModeResult as CoreModeResult, SolvedCandidate, Solver};
use strata_core::diversify::{diversify, vi_distance};
use strata_core::graph::csr::{HardnessFilter, build_csr};
use strata_core::layer::layer;
use strata_core::score::{
    Candidate as ScoreCandidate, Coefficients, ContainerSizes, ScoreBreakdown as CoreBreakdown,
    ScoredEdge, score,
};
use strata_core::visibility::derive_visibility;
use strata_ir::{Container, ContainerId, ContainerTree, Node, Polarity, ScopeLevel, Snapshot};

use crate::config::AnalyzeConfig;
use crate::error::StrataError;
use crate::result::{
    AnalyzeResult, Candidate, ContainerNode, CurrentTree, Level, ModeResult, Modes, ScoreBreakdown,
    Severity, Summary, SymbolPlacement, Violation, ViolationKind, narrate_delta,
};

/// Analyzes `snapshot` under `config`, returning the owned [`AnalyzeResult`].
///
/// The pass is pure: it reads only the snapshot and config and returns owned
/// data. It derives the current tree's violations (cycles, polarity breaches,
/// over-exports), scores the current layout, then runs the restructuring pipeline
/// to produce one [`ModeResult`] of up to `k` diverse candidates per requested
/// mode.
///
/// # Errors
///
/// Returns [`StrataError::SnapshotInvalid`] if the snapshot's container tree
/// cannot be rendered (it is otherwise pre-validated at assembly).
pub fn analyze(snapshot: &Snapshot, config: &AnalyzeConfig) -> Result<AnalyzeResult, StrataError> {
    let ir = snapshot.ir();
    let tree = &ir.containers;

    let violations = collect_violations(snapshot, config);
    let current_node = render_tree(tree, &ir.nodes)?;
    let current_breakdown = score_current(snapshot, &Coefficients::anchored());

    let mode = config.analysis.mode;
    let anchored = mode
        .includes_anchored()
        .then(|| build_mode_result(snapshot, config, &Coefficients::anchored()))
        .transpose()?;
    let greenfield = mode
        .includes_greenfield()
        .then(|| build_mode_result(snapshot, config, &Coefficients::greenfield()))
        .transpose()?;

    Ok(AnalyzeResult {
        snapshot_hash: snapshot.hash().to_hex().to_string(),
        summary: summarize(snapshot),
        current: CurrentTree {
            tree: current_node,
            score: current_breakdown.total,
            score_breakdown: current_breakdown.into(),
            violations,
        },
        modes: Modes {
            anchored,
            greenfield,
        },
    })
}

/// Builds the coarse census of the snapshot.
fn summarize(snapshot: &Snapshot) -> Summary {
    let ir = snapshot.ir();
    let files = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .count();

    Summary {
        symbols: u32::try_from(ir.nodes.len()).unwrap_or(u32::MAX),
        edges: u32::try_from(ir.edges.len()).unwrap_or(u32::MAX),
        files: u32::try_from(files).unwrap_or(u32::MAX),
        files_by_language: BTreeMap::new(),
    }
}

/// Collects the structural violations of the current tree: dependency cycles,
/// polarity breaches, and visibility over-exports.
fn collect_violations(snapshot: &Snapshot, _config: &AnalyzeConfig) -> Vec<Violation> {
    let mut violations = Vec::new();
    violations.extend(cycle_violations(snapshot));
    violations.extend(polarity_violations(snapshot));
    violations.extend(visibility_violations(snapshot));
    violations
}

/// Reports every multi-node strongly connected component of the hard-edge graph
/// as a cycle violation.
fn cycle_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let views = build_csr(snapshot, HardnessFilter::HardOnly);
    let condensation = condense(&views.forward);
    let names = node_names(snapshot);

    condensation
        .members
        .iter()
        .filter(|members| members.len() > 1)
        .map(|members| {
            let location = members
                .iter()
                .filter_map(|node| names.get(&node.0).cloned())
                .collect::<Vec<_>>();
            Violation {
                kind: ViolationKind::Cycle,
                severity: Severity::Violation,
                detail: format!("dependency cycle over {} symbols", members.len()),
                location,
                break_suggestions: Some(Vec::new()),
            }
        })
        .collect()
}

/// Reports production nodes that depend on test code as polarity breaches.
fn polarity_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let ir = snapshot.ir();
    let polarity_by_id: BTreeMap<u32, Polarity> = ir
        .nodes
        .iter()
        .map(|node| (node.id.0, node.polarity))
        .collect();
    let names = node_names(snapshot);

    ir.edges
        .iter()
        .filter(|edge| {
            matches!(
                polarity_by_id.get(&edge.source.0),
                Some(Polarity::Production)
            ) && matches!(
                polarity_by_id.get(&edge.target.0),
                Some(Polarity::TestCase | Polarity::TestSupport)
            )
        })
        .map(|edge| {
            let source = names.get(&edge.source.0).cloned().unwrap_or_default();
            let target = names.get(&edge.target.0).cloned().unwrap_or_default();
            Violation {
                kind: ViolationKind::Polarity,
                severity: Severity::Violation,
                detail: format!("production symbol `{source}` depends on test code `{target}`"),
                location: vec![source, target],
                break_suggestions: None,
            }
        })
        .collect()
}

/// Reports symbols whose declared visibility is wider than their derived scope.
fn visibility_violations(snapshot: &Snapshot) -> Vec<Violation> {
    let ir = snapshot.ir();
    let result = derive_visibility(&ir.containers, &ir.nodes, &ir.edges);
    let names = node_names(snapshot);

    result
        .findings
        .iter()
        .map(|finding| {
            let name = names.get(&finding.node.0).cloned().unwrap_or_default();
            Violation {
                kind: ViolationKind::Visibility,
                severity: Severity::Violation,
                detail: format!(
                    "`{name}` is exported at {:?} but needed only at {:?}",
                    finding.declared, finding.derived
                ),
                location: vec![name],
                break_suggestions: None,
            }
        })
        .collect()
}

/// Scores the snapshot's current layout under `coefficients`.
///
/// The current candidate carries the snapshot's own edges (each crossing the LCA
/// level of its endpoints in the current tree), the file-level container sizes,
/// and a zero move distance, so its objective is the genuine `J(T0)` baseline the
/// candidates are measured against.
fn score_current(snapshot: &Snapshot, coefficients: &Coefficients) -> CoreBreakdown {
    let ir = snapshot.ir();
    let container_of: BTreeMap<u32, ContainerId> = ir
        .nodes
        .iter()
        .map(|node| (node.id.0, node.container))
        .collect();
    let candidate = score_candidate(
        snapshot,
        &|id| container_of.get(&id).copied(),
        &ir.containers,
        0.0,
    );
    score(&candidate, coefficients)
}

/// Builds one mode's result by running the diversifying restructuring search.
///
/// The mode's [`Coefficients`] select anchored vs greenfield behaviour. The search
/// runs `condense -> layer -> cluster` per seed (multi-start), scores each layout,
/// and diversifies to up to `k` genuinely different candidates by max-min
/// variation of information. Each surviving partition is reconstructed into a
/// candidate [`ContainerNode`] tree and narrated against the current layout.
///
/// # Errors
///
/// Returns [`StrataError::SnapshotInvalid`] if a candidate tree cannot be rendered.
fn build_mode_result(
    snapshot: &Snapshot,
    config: &AnalyzeConfig,
    coefficients: &Coefficients,
) -> Result<ModeResult, StrataError> {
    let solver = PipelineSolver::new(snapshot, config, *coefficients);
    let mode_config = mode_config(config);
    let CoreModeResult {
        candidates,
        solution_space_converged,
    } = diversify(&solver, &mode_config);

    let current_tree = &snapshot.ir().containers;
    let mut built = Vec::with_capacity(candidates.len());
    for (index, solved) in candidates.iter().enumerate() {
        built.push(build_candidate(
            snapshot,
            current_tree,
            solved,
            coefficients,
            u32::try_from(index + 1).unwrap_or(u32::MAX),
        )?);
    }

    let pairwise_distance = pairwise_distances(&candidates);
    Ok(ModeResult {
        candidates: built,
        pairwise_distance,
        solution_space_converged,
    })
}

/// Returns the variation-of-information matrix over the diversified candidates.
fn pairwise_distances(candidates: &[SolvedCandidate]) -> Vec<Vec<f64>> {
    candidates
        .iter()
        .map(|left| {
            candidates
                .iter()
                .map(|right| vi_distance(&left.partition, &right.partition))
                .collect()
        })
        .collect()
}

/// Translates the engine config into the diversifier's [`ModeConfig`].
fn mode_config(config: &AnalyzeConfig) -> ModeConfig {
    ModeConfig {
        k: config.analysis.candidates as usize,
        base_seed: config.analysis.seed,
        score_tolerance: config.diversity.score_tolerance,
        min_distance: config.diversity.min_distance,
    }
}

/// The restartable solver that runs the cluster pipeline once per seed.
///
/// All of the seed-independent work — condensation, layering, the coarsening
/// chain, and the per-SCC scoring inputs — is computed once at construction; each
/// [`Solver::solve`] call seeds the initial partition deterministically from its
/// seed, refines it, and scores the induced layout, so every seed yields a pure,
/// reproducible candidate.
struct PipelineSolver<'a> {
    /// The analyzed snapshot.
    snapshot: &'a Snapshot,
    /// The SCC condensation of the snapshot's hard-edge graph.
    condensation: Condensation,
    /// Longest-path layers over the condensation DAG, indexed by SCC.
    layers: Vec<u32>,
    /// The per-level member caps.
    caps: LevelCaps,
    /// The objective coefficients for this mode.
    coefficients: Coefficients,
}

impl<'a> PipelineSolver<'a> {
    /// Builds the solver, computing every seed-independent pipeline input once.
    fn new(snapshot: &'a Snapshot, config: &AnalyzeConfig, coefficients: Coefficients) -> Self {
        let views = build_csr(snapshot, HardnessFilter::HardOnly);
        let condensation = condense(&views.forward);
        let layers = layer(&condensation);
        let caps = level_caps(config);
        Self {
            snapshot,
            condensation,
            layers,
            caps,
            coefficients,
        }
    }

    /// Reconstructs the symbol partition this solver's SCC clustering induces.
    ///
    /// Each SCC is clustered into a folder; every member symbol inherits its SCC's
    /// cluster, so the returned partition is over symbol node ids — the shape the
    /// diversifier's variation-of-information selection compares.
    fn symbol_partition(&self, scc_partition: &Partition) -> Partition {
        let node_count = self.snapshot.ir().nodes.len();
        let mut assignment = vec![ClusterId(0); node_count];
        for (scc, members) in self.condensation.members.iter().enumerate() {
            let cluster = scc_partition
                .cluster_of(u32::try_from(scc).unwrap_or(u32::MAX))
                .unwrap_or(ClusterId(0));
            for member in members {
                if let Some(slot) = assignment.get_mut(member.0 as usize) {
                    *slot = cluster;
                }
            }
        }
        let cluster_count = scc_partition.cluster_count().max(1);
        Partition::from_assignment(assignment, cluster_count)
    }

    /// Maps each symbol node id to the folder cluster it lands in under
    /// `scc_partition`.
    fn cluster_of_symbol(&self, scc_partition: &Partition) -> BTreeMap<u32, ClusterId> {
        let mut map = BTreeMap::new();
        for (scc, members) in self.condensation.members.iter().enumerate() {
            let cluster = scc_partition
                .cluster_of(u32::try_from(scc).unwrap_or(u32::MAX))
                .unwrap_or(ClusterId(0));
            for member in members {
                map.insert(member.0, cluster);
            }
        }
        map
    }
}

impl Solver for PipelineSolver<'_> {
    fn solve(&self, seed: u64) -> SolvedCandidate {
        let scc_partition = cluster_sccs(&self.condensation, &self.layers, &self.caps, seed);
        let cluster_of = self.cluster_of_symbol(&scc_partition);
        let candidate_tree = candidate_tree(self.snapshot, &cluster_of);
        let placement = |id: u32| candidate_tree.placement.get(&id).copied();
        let move_distance = move_distance(self.snapshot, &candidate_tree.tree);
        let score_candidate = score_candidate(
            self.snapshot,
            &placement,
            &candidate_tree.tree,
            move_distance,
        );
        let breakdown = score(&score_candidate, &self.coefficients);

        SolvedCandidate {
            partition: self.symbol_partition(&scc_partition),
            score: breakdown.total,
        }
    }
}

/// Runs `coarsen -> seed -> refine` over the condensation, deriving a
/// seed-dependent folder clustering of the SCC DAG.
///
/// The coarsening chain and the cohesion-free gain model are deterministic; the
/// `seed` perturbs the topological seeding order so distinct seeds explore
/// distinct local optima, exactly as multi-start diversification requires. The
/// acyclicity and capacity vetoes inside [`refine`] keep every result a legal
/// laminar clustering.
fn cluster_sccs(
    condensation: &Condensation,
    layers: &[u32],
    caps: &LevelCaps,
    seed_value: u64,
) -> Partition {
    let chain = coarsen_chain(&condensation.dag, layers);
    let Some(top) = chain.last() else {
        return Partition::from_assignment(Vec::new(), 0);
    };
    let perturbed_layers = perturb_layers(layers, seed_value);
    let mut partition = seed(top, &perturbed_layers, caps, SeedLevel::Folder);
    let gain = GainFn::cut_only(top.graph.vertex_count());
    refine(top, &mut partition, &gain, caps, SeedLevel::Folder);
    partition
}

/// Applies a deterministic seed-driven jitter to the layering used for seeding.
///
/// The seeding order is descending layer, ties broken by index; nudging each
/// vertex's layer by a small seed-derived amount reorders the ties differently per
/// seed without changing the longest-path structure materially, so each restart
/// grows a different initial clustering. Seed `0` returns the layers unchanged so
/// the base seed reproduces the canonical clustering.
fn perturb_layers(layers: &[u32], seed_value: u64) -> Vec<u32> {
    if seed_value == 0 {
        return layers.to_vec();
    }
    layers
        .iter()
        .enumerate()
        .map(|(index, &base)| {
            let mut state = seed_value
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add(u64::try_from(index).unwrap_or(u64::MAX));
            // a small splitmix step gives a reproducible per-(seed, index) jitter.
            state ^= state >> 30;
            state = state.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            let jitter = u32::try_from(state % 3).unwrap_or(0);
            base.saturating_mul(3).saturating_add(jitter)
        })
        .collect()
}

/// A reconstructed candidate tree plus the placement of every symbol node.
struct CandidateTree {
    /// The candidate container tree: a package-group root over one folder per
    /// cluster, each holding the file containers whose symbols cluster there.
    tree: ContainerTree,
    /// The file container each symbol node lands in, keyed by node id.
    placement: BTreeMap<u32, ContainerId>,
}

/// Reconstructs a candidate [`ContainerTree`] from a symbol-to-cluster map.
///
/// Each original file container is reparented under the folder of the cluster its
/// symbols predominantly land in, and the clusters become folder containers under
/// a single package-group root. The result is a legal laminar tree whose level
/// ascent (file < folder < package-group) the scorer and renderer both accept.
fn candidate_tree(snapshot: &Snapshot, cluster_of: &BTreeMap<u32, ClusterId>) -> CandidateTree {
    let ir = snapshot.ir();

    // each file's destination cluster is the majority cluster of its symbols.
    let mut votes: BTreeMap<u32, BTreeMap<ClusterId, u32>> = BTreeMap::new();
    for node in &ir.nodes {
        let cluster = cluster_of.get(&node.id.0).copied().unwrap_or(ClusterId(0));
        *votes
            .entry(node.container.0)
            .or_default()
            .entry(cluster)
            .or_default() += 1;
    }

    let files: BTreeMap<u32, &Container> = ir
        .containers
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .map(|container| (container.id.0, container))
        .collect();

    // assign a dense container-id range: root = 0, folders next, files last, so the
    // candidate tree never collides with the snapshot's own ids.
    let root_id = ContainerId(0);
    let mut clusters: Vec<ClusterId> = votes.values().filter_map(dominant_cluster).collect();
    clusters.sort_by_key(|cluster| cluster.0);
    clusters.dedup();
    let folder_id_of: BTreeMap<ClusterId, ContainerId> = clusters
        .iter()
        .enumerate()
        .map(|(index, cluster)| {
            (
                *cluster,
                ContainerId(u32::try_from(index + 1).unwrap_or(u32::MAX)),
            )
        })
        .collect();

    let mut containers = vec![Container {
        id: root_id,
        name: smol_str::SmolStr::new("workspace"),
        level: ScopeLevel::PackageGroup,
        parent: None,
    }];
    for cluster in &clusters {
        if let Some(folder_id) = folder_id_of.get(cluster) {
            containers.push(Container {
                id: *folder_id,
                name: smol_str::SmolStr::new(format!("cluster-{}", cluster.0)),
                level: ScopeLevel::Folder,
                parent: Some(root_id),
            });
        }
    }

    let file_base = clusters.len() + 1;
    let mut placement = BTreeMap::new();
    let mut file_id_of: BTreeMap<u32, ContainerId> = BTreeMap::new();
    for (offset, (original_id, container)) in files.iter().enumerate() {
        let new_id = ContainerId(u32::try_from(file_base + offset).unwrap_or(u32::MAX));
        let cluster = votes
            .get(original_id)
            .and_then(dominant_cluster)
            .unwrap_or(ClusterId(0));
        let parent = folder_id_of
            .get(&cluster)
            .copied()
            .or_else(|| folder_id_of.values().next().copied())
            .unwrap_or(root_id);
        containers.push(Container {
            id: new_id,
            name: container.name.clone(),
            level: ScopeLevel::File,
            parent: Some(parent),
        });
        file_id_of.insert(*original_id, new_id);
    }

    for node in &ir.nodes {
        if let Some(file_id) = file_id_of.get(&node.container.0) {
            placement.insert(node.id.0, *file_id);
        }
    }

    CandidateTree {
        tree: ContainerTree::new(containers),
        placement,
    }
}

/// Returns the cluster with the most votes, ties broken by the lower cluster id.
fn dominant_cluster(tally: &BTreeMap<ClusterId, u32>) -> Option<ClusterId> {
    tally
        .iter()
        .max_by(|left, right| left.1.cmp(right.1).then(right.0.cmp(left.0)))
        .map(|(cluster, _)| *cluster)
}

/// Builds the scorer's [`ScoreCandidate`] view from a node-placement function over
/// the candidate `tree`.
///
/// `placement` maps each node id to the file container it occupies in the
/// candidate tree; the edge LCA levels, per-container child sizes, and naming
/// groups are derived from that placement and the tree. `move_distance` is the
/// already-computed fraction of relocated symbols.
fn score_candidate(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
    move_distance: f64,
) -> ScoreCandidate {
    let ir = snapshot.ir();
    let parent_of: BTreeMap<u32, Option<ContainerId>> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container.parent))
        .collect();
    let level_of: BTreeMap<u32, ScopeLevel> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container.level))
        .collect();

    let edges = ir
        .edges
        .iter()
        .filter_map(|edge| {
            let source = placement(edge.source.0)?;
            let target = placement(edge.target.0)?;
            let lca = lca_level(&parent_of, &level_of, source, target);
            Some(ScoredEdge {
                kind: edge.kind,
                confidence: edge.confidence,
                lca_level: lca,
            })
        })
        .collect();

    let containers = container_sizes(snapshot, placement, tree);

    ScoreCandidate {
        edges,
        containers,
        cohesion_groups: Vec::new(),
        path_cohesion: 0.0,
        move_distance,
    }
}

/// Returns the level of the lowest common ancestor of two containers.
///
/// Walks the ancestor chain of `left` into a set, then ascends `right` until a
/// shared ancestor is found; the highest endpoints share is the package-group root
/// of an empty intersection, so disjoint subtrees cross at the coarsest level.
fn lca_level(
    parent_of: &BTreeMap<u32, Option<ContainerId>>,
    level_of: &BTreeMap<u32, ScopeLevel>,
    left: ContainerId,
    right: ContainerId,
) -> ScopeLevel {
    let mut ancestors = std::collections::BTreeSet::new();
    let mut up_left = Some(left);
    while let Some(id) = up_left {
        if !ancestors.insert(id.0) {
            break;
        }
        up_left = parent_of.get(&id.0).copied().flatten();
    }

    let mut up_right = Some(right);
    while let Some(id) = up_right {
        if ancestors.contains(&id.0) {
            return level_of
                .get(&id.0)
                .copied()
                .unwrap_or(ScopeLevel::PackageGroup);
        }
        up_right = parent_of.get(&id.0).copied().flatten();
    }
    ScopeLevel::PackageGroup
}

/// Computes the per-container child subtree sizes (in production SLOC) for the
/// imbalance term.
fn container_sizes(
    snapshot: &Snapshot,
    placement: &dyn Fn(u32) -> Option<ContainerId>,
    tree: &ContainerTree,
) -> Vec<ContainerSizes> {
    let ir = snapshot.ir();
    // each file's production SLOC is the count of symbols placed in it.
    let mut file_sloc: BTreeMap<u32, u32> = BTreeMap::new();
    for node in &ir.nodes {
        if let Some(container) = placement(node.id.0) {
            *file_sloc.entry(container.0).or_default() += 1;
        }
    }

    // bottom-up subtree size of each container.
    let mut subtree: BTreeMap<u32, u32> = file_sloc.clone();
    let mut children_by_parent: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for container in tree.containers() {
        if let Some(parent) = container.parent {
            children_by_parent
                .entry(parent.0)
                .or_default()
                .push(container.id.0);
        }
    }

    // accumulate sizes bottom-up by repeatedly summing known children; the tree is
    // shallow so a fixpoint over its depth converges quickly.
    for _ in 0..tree.containers().len() {
        for container in tree.containers() {
            let total: u32 = children_by_parent
                .get(&container.id.0)
                .into_iter()
                .flatten()
                .filter_map(|child| subtree.get(child).copied())
                .sum();
            if total > 0 {
                subtree.insert(container.id.0, total);
            }
        }
    }

    children_by_parent
        .into_values()
        .map(|children| ContainerSizes {
            child_sizes: children
                .iter()
                .map(|child| subtree.get(child).copied().unwrap_or(0))
                .collect(),
        })
        .collect()
}

/// Computes the move distance of a candidate tree: the fraction of symbols whose
/// owning file path differs from the current layout.
fn move_distance(snapshot: &Snapshot, candidate: &ContainerTree) -> f64 {
    let ir = snapshot.ir();
    let current_paths = container_path_strings(&ir.containers);
    let candidate_paths = container_path_strings(candidate);
    let candidate_file_name: BTreeMap<smol_str::SmolStr, Vec<String>> = candidate
        .containers()
        .iter()
        .filter(|container| container.level == ScopeLevel::File)
        .filter_map(|container| {
            candidate_paths
                .get(&container.id.0)
                .map(|path| (container.name.clone(), path.clone()))
        })
        .collect();

    let total = ir.nodes.len();
    if total == 0 {
        return 0.0;
    }
    let moved = ir
        .nodes
        .iter()
        .filter(|node| {
            let Some(current) = current_paths.get(&node.container.0) else {
                return false;
            };
            // match candidate file by the original file's name, which the
            // reconstruction preserves.
            let original_name = ir
                .containers
                .containers()
                .iter()
                .find(|container| container.id == node.container)
                .map(|container| container.name.clone());
            let Some(name) = original_name else {
                return false;
            };
            candidate_file_name
                .get(&name)
                .is_none_or(|candidate_path| candidate_path != current)
        })
        .count();

    f64::from(u32::try_from(moved).unwrap_or(u32::MAX))
        / f64::from(u32::try_from(total).unwrap_or(u32::MAX))
}

/// Returns each container's root-to-node name path, keyed by container id.
fn container_path_strings(tree: &ContainerTree) -> BTreeMap<u32, Vec<String>> {
    let by_id: BTreeMap<u32, &Container> = tree
        .containers()
        .iter()
        .map(|container| (container.id.0, container))
        .collect();
    let mut paths = BTreeMap::new();
    for container in tree.containers() {
        let mut path = Vec::new();
        let mut cursor = Some(container.id);
        while let Some(id) = cursor {
            let Some(node) = by_id.get(&id.0) else {
                break;
            };
            path.push(node.name.to_string());
            cursor = node.parent;
        }
        path.reverse();
        paths.insert(container.id.0, path);
    }
    paths
}

/// Builds one DTO [`Candidate`] from a solved partition.
///
/// The candidate's tree is the reconstructed laminar layout the partition induces;
/// its score breakdown is recomputed under the mode's coefficients, and its delta
/// narration is the per-destination move list versus the current tree.
///
/// # Errors
///
/// Returns [`StrataError::SnapshotInvalid`] if the candidate tree has no root.
fn build_candidate(
    snapshot: &Snapshot,
    current_tree: &ContainerTree,
    solved: &SolvedCandidate,
    coefficients: &Coefficients,
    index: u32,
) -> Result<Candidate, StrataError> {
    let cluster_of: BTreeMap<u32, ClusterId> = solved
        .partition
        .assignment()
        .iter()
        .enumerate()
        .map(|(node, cluster)| (u32::try_from(node).unwrap_or(u32::MAX), *cluster))
        .collect();
    let reconstructed = candidate_tree(snapshot, &cluster_of);
    let placement = |id: u32| reconstructed.placement.get(&id).copied();
    let distance = move_distance(snapshot, &reconstructed.tree);
    let breakdown = score(
        &score_candidate(snapshot, &placement, &reconstructed.tree, distance),
        coefficients,
    );
    let node = render_tree(&reconstructed.tree, &snapshot.ir().nodes)?;
    let delta = narrate_delta(current_tree, &reconstructed.tree);

    Ok(Candidate {
        index,
        score: breakdown.total,
        score_breakdown: ScoreBreakdown::from(breakdown),
        tree: node,
        conditional_splits: Vec::new(),
        delta_narration: delta,
    })
}

/// Extracts the per-level member caps from the engine config.
fn level_caps(config: &AnalyzeConfig) -> LevelCaps {
    LevelCaps {
        folder: config.capacity.folder,
        domain: config.capacity.domain,
        package: config.capacity.package,
        package_group: config.capacity.package_group,
    }
}

/// Renders a [`ContainerTree`] into the nested [`ContainerNode`] DTO, attaching
/// each file's symbols and production SLOC.
///
/// # Errors
///
/// Returns [`StrataError::SnapshotInvalid`] if the tree has no root container.
fn render_tree(tree: &ContainerTree, nodes: &[Node]) -> Result<ContainerNode, StrataError> {
    let containers = tree.containers();
    let mut children_by_parent: BTreeMap<u32, Vec<&Container>> = BTreeMap::new();
    let mut roots = Vec::new();
    for container in containers {
        match container.parent {
            Some(parent) => children_by_parent
                .entry(parent.0)
                .or_default()
                .push(container),
            None => roots.push(container),
        }
    }

    let symbols_by_container = symbols_by_container(nodes);

    // a forest with several roots is wrapped under a synthetic package group so
    // the DTO is always a single tree; a lone root is rendered directly.
    match roots.as_slice() {
        [] => Err(StrataError::SnapshotInvalid {
            source: strata_ir::SnapshotError::Serialization {
                reason: "container tree has no root".to_owned(),
            },
        }),
        [root] => Ok(render_node(
            root,
            &children_by_parent,
            &symbols_by_container,
        )),
        many => Ok(ContainerNode {
            name: "workspace".to_owned(),
            level: Level::PackageGroup,
            children: Some(
                many.iter()
                    .map(|root| render_node(root, &children_by_parent, &symbols_by_container))
                    .collect(),
            ),
            symbols: None,
            production_sloc: None,
        }),
    }
}

/// Recursively renders one container and its descendants.
fn render_node(
    container: &Container,
    children_by_parent: &BTreeMap<u32, Vec<&Container>>,
    symbols_by_container: &BTreeMap<u32, Vec<SymbolPlacement>>,
) -> ContainerNode {
    if container.level == ScopeLevel::File {
        let symbols = symbols_by_container
            .get(&container.id.0)
            .cloned()
            .unwrap_or_default();
        let production_sloc = symbols.len();
        return ContainerNode {
            name: container.name.to_string(),
            level: Level::from(container.level),
            children: None,
            symbols: Some(symbols),
            production_sloc: Some(u32::try_from(production_sloc).unwrap_or(u32::MAX)),
        };
    }

    let children = children_by_parent
        .get(&container.id.0)
        .map(|children| {
            children
                .iter()
                .map(|child| render_node(child, children_by_parent, symbols_by_container))
                .collect()
        })
        .unwrap_or_default();

    ContainerNode {
        name: container.name.to_string(),
        level: Level::from(container.level),
        children: Some(children),
        symbols: None,
        production_sloc: None,
    }
}

/// Groups symbol placements by their owning container id.
fn symbols_by_container(nodes: &[Node]) -> BTreeMap<u32, Vec<SymbolPlacement>> {
    let mut by_container: BTreeMap<u32, Vec<SymbolPlacement>> = BTreeMap::new();
    for node in nodes {
        by_container
            .entry(node.container.0)
            .or_default()
            .push(SymbolPlacement {
                name: node.name.to_string(),
                visibility: Level::from(node.visibility),
            });
    }
    by_container
}

/// Builds a node-id to name lookup keyed by the raw id.
fn node_names(snapshot: &Snapshot) -> BTreeMap<u32, String> {
    snapshot
        .ir()
        .nodes
        .iter()
        .map(|node| (node.id.0, node.name.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;
    use strata_ir::{
        ContainerId, Edge, EdgeKind, Hardness, IntermediateRepresentation, NodeId, NodeKind,
    };

    use super::*;

    /// Builds a symbol node owning a container.
    fn node(id: u32, name: &str, container: u32, polarity: Polarity) -> Node {
        Node {
            id: NodeId(id),
            name: SmolStr::new(name),
            kind: NodeKind::Symbol,
            polarity,
            container: ContainerId(container),
            visibility: ScopeLevel::File,
            effective_size: 1,
        }
    }

    /// Builds a hard call edge.
    fn edge(source: u32, target: u32) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind: EdgeKind::Call,
            hardness: Hardness::Hard,
            confidence: 1.0,
        }
    }

    /// Builds a container at a level under an optional parent.
    fn container(id: u32, name: &str, level: ScopeLevel, parent: Option<u32>) -> Container {
        Container {
            id: ContainerId(id),
            name: SmolStr::new(name),
            level,
            parent: parent.map(ContainerId),
        }
    }

    /// Assembles a snapshot from parts, falling back to a minimal valid one on
    /// failure so a test never panics on assembly.
    fn snapshot(nodes: Vec<Node>, edges: Vec<Edge>, containers: Vec<Container>) -> Snapshot {
        let ir = IntermediateRepresentation::new(nodes, edges, ContainerTree::new(containers));
        Snapshot::assemble(ir).unwrap_or_else(|_| minimal_snapshot())
    }

    /// Returns a minimal valid snapshot: a single empty root file container.
    fn minimal_snapshot() -> Snapshot {
        let ir = IntermediateRepresentation::new(
            vec![],
            vec![],
            ContainerTree::new(vec![container(0, "root", ScopeLevel::File, None)]),
        );
        Snapshot::assemble(ir).unwrap_or_else(|_| minimal_snapshot())
    }

    /// Builds a config requesting `k` candidates in both modes.
    fn config_with_k(k: u32) -> AnalyzeConfig {
        let mut config = AnalyzeConfig::default();
        config.analysis.candidates = k;
        config
    }

    #[test]
    fn should_report_a_cycle_as_a_violation() {
        let snapshot = snapshot(
            vec![
                node(0, "a", 0, Polarity::Production),
                node(1, "b", 0, Polarity::Production),
            ],
            vec![edge(0, 1), edge(1, 0)],
            vec![container(0, "file", ScopeLevel::File, None)],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let violations = result
            .map(|result| result.current.violations)
            .unwrap_or_default();

        assert!(violations.iter().any(|v| v.kind == ViolationKind::Cycle));
    }

    #[test]
    fn should_report_a_polarity_breach() {
        let snapshot = snapshot(
            vec![
                node(0, "prod", 0, Polarity::Production),
                node(1, "helper", 0, Polarity::TestSupport),
            ],
            vec![edge(0, 1)],
            vec![container(0, "file", ScopeLevel::File, None)],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let violations = result
            .map(|result| result.current.violations)
            .unwrap_or_default();

        assert!(violations.iter().any(|v| v.kind == ViolationKind::Polarity));
    }

    #[test]
    fn should_be_deterministic_in_its_hash() {
        let make = || {
            snapshot(
                vec![node(0, "a", 0, Polarity::Production)],
                vec![],
                vec![container(0, "file", ScopeLevel::File, None)],
            )
        };

        let first = analyze(&make(), &AnalyzeConfig::default())
            .map(|result| result.snapshot_hash)
            .unwrap_or_default();
        let second = analyze(&make(), &AnalyzeConfig::default())
            .map(|result| result.snapshot_hash)
            .unwrap_or_default();

        assert_eq!(first, second);
        assert!(!first.is_empty());
    }

    #[test]
    fn should_produce_both_modes_when_requested() {
        let snapshot = snapshot(
            vec![node(0, "a", 0, Polarity::Production)],
            vec![],
            vec![container(0, "file", ScopeLevel::File, None)],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let modes = result.map(|result| result.modes).unwrap_or_default();

        assert!(modes.anchored.is_some());
        assert!(modes.greenfield.is_some());
    }

    #[test]
    fn should_return_up_to_k_candidates_per_mode() {
        // a graph with several independent files gives the clusterer room to find
        // more than one distinct grouping, so diversification returns multiple.
        let nodes = (0..8)
            .map(|i| node(i, &format!("sym{i}"), i, Polarity::Production))
            .collect::<Vec<_>>();
        let containers = (0..8)
            .map(|i| container(i, &format!("file{i}"), ScopeLevel::File, None))
            .collect::<Vec<_>>();
        let snapshot = snapshot(nodes, vec![edge(0, 1), edge(2, 3), edge(4, 5)], containers);

        let anchored = analyze(&snapshot, &config_with_k(3))
            .ok()
            .and_then(|result| result.modes.anchored)
            .unwrap_or(ModeResult {
                candidates: Vec::new(),
                pairwise_distance: Vec::new(),
                solution_space_converged: false,
            });

        // never more than k, always at least one candidate is produced.
        assert!(!anchored.candidates.is_empty());
        assert!(anchored.candidates.len() <= 3);
        // candidates are ranked, best (lowest) score first.
        for window in anchored.candidates.windows(2) {
            let (Some(first), Some(second)) = (window.first(), window.get(1)) else {
                continue;
            };
            assert!(first.score <= second.score);
        }
        // the pairwise distance matrix is square over the returned candidates.
        assert_eq!(anchored.pairwise_distance.len(), anchored.candidates.len());
    }

    #[test]
    fn should_index_candidates_from_one() {
        let snapshot = snapshot(
            vec![
                node(0, "a", 0, Polarity::Production),
                node(1, "b", 1, Polarity::Production),
            ],
            vec![],
            vec![
                container(0, "file_a", ScopeLevel::File, None),
                container(1, "file_b", ScopeLevel::File, None),
            ],
        );

        let first_index = analyze(&snapshot, &config_with_k(2))
            .ok()
            .and_then(|result| result.modes.anchored)
            .and_then(|anchored| anchored.candidates.first().map(|candidate| candidate.index));

        assert_eq!(first_index, Some(1));
    }

    #[test]
    fn should_render_a_nested_tree_with_file_symbols() {
        let snapshot = snapshot(
            vec![node(0, "sym", 1, Polarity::Production)],
            vec![],
            vec![
                container(0, "pkg", ScopeLevel::Package, None),
                container(1, "file", ScopeLevel::File, Some(0)),
            ],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let tree = result.map(|result| result.current.tree);

        let root = tree.unwrap_or_else(|_| ContainerNode {
            name: String::new(),
            level: Level::File,
            children: None,
            symbols: None,
            production_sloc: None,
        });
        assert_eq!(root.level, Level::Package);
        let file = root
            .children
            .and_then(|children| children.into_iter().next());
        assert_eq!(file.and_then(|file| file.production_sloc), Some(1));
    }

    #[test]
    fn should_render_a_multi_root_forest_under_a_synthetic_group() {
        let snapshot = snapshot(
            vec![],
            vec![],
            vec![
                container(0, "a", ScopeLevel::Package, None),
                container(1, "b", ScopeLevel::Package, None),
            ],
        );

        let result = analyze(&snapshot, &AnalyzeConfig::default());
        let level = result.map_or(Level::File, |result| result.current.tree.level);

        assert_eq!(level, Level::PackageGroup);
    }

    #[test]
    fn should_be_deterministic_across_runs() {
        let make = || {
            let nodes = (0..6)
                .map(|i| node(i, &format!("sym{i}"), i, Polarity::Production))
                .collect::<Vec<_>>();
            let containers = (0..6)
                .map(|i| container(i, &format!("file{i}"), ScopeLevel::File, None))
                .collect::<Vec<_>>();
            snapshot(nodes, vec![edge(0, 1), edge(2, 3)], containers)
        };

        let scores = |snapshot: &Snapshot| {
            analyze(snapshot, &config_with_k(3))
                .ok()
                .and_then(|result| result.modes.anchored)
                .map(|mode| mode.candidates.iter().map(|c| c.score).collect::<Vec<_>>())
                .unwrap_or_default()
        };

        assert_eq!(scores(&make()), scores(&make()));
    }
}
