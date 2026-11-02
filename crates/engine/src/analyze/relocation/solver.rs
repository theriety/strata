use std::collections::BTreeMap;

use smol_str::SmolStr;
use strata_core::cluster::{ClusterId, Partition};
use strata_core::condense::condense;
use strata_core::diversify::{SolvedCandidate, Solver};
use strata_core::graph::csr::Csr;
use strata_core::score::{Coefficients, KindWeights, score};
use strata_ir::{Edge, Node, Polarity, ScopeLevel, Snapshot};

use crate::analyze::findings::{
    physical_folder_entries_repository_relative, physical_folder_findings, walk_all_capacity,
};
use crate::analyze::layout::{real_dir_partition, relieve_over_capacity, synthesize_roof_rebuild};
use crate::analyze::relocation::mirror::{MirrorEvidence, mirror_rules};
use crate::analyze::relocation::{
    CandidateTree, FileInfo, POLISH_SWEEPS, POLISH_TARGETS, PipelineSolver,
    RelocationIdentityGuard, TestPolicy, cycle_homes, file_facts_repository_relative,
    file_inventory,
};
use crate::analyze::scoring::{
    CycleCounts, ProfileSource, level_caps, move_distance, score_candidate,
};
use crate::config::CapacityConfig;
use crate::result::{CapacityRemainder, ContainerNode, Level, Severity};

impl<'a> PipelineSolver<'a> {
    fn physical_entry_counts(&self, parts: &Partition) -> BTreeMap<Vec<String>, u32> {
        let assembled = self.assemble(parts);
        physical_folder_entries_repository_relative(
            &assembled.tree,
            Some(&assembled.namespace_by_file),
        )
    }

    fn physical_entry_counts_with_mirror_evidence(
        &self,
        parts: &Partition,
        evidence: &MirrorEvidence,
    ) -> BTreeMap<Vec<String>, u32> {
        let assembled = self.assemble_with_mirror_evidence(parts, evidence);
        physical_folder_entries_repository_relative(
            &assembled.tree,
            Some(&assembled.namespace_by_file),
        )
    }

    pub(in crate::analyze) fn permits_physical_capacity(
        &self,
        parts: &Partition,
        scc: u32,
        target: ClusterId,
    ) -> bool {
        if self.caps.folder == 0 {
            return true;
        }
        let Some(source) = parts.cluster_of(scc) else {
            return false;
        };
        if source == target {
            return true;
        }
        let before = self.physical_entry_counts(parts);
        let mut assignment = parts.assignment().to_vec();
        let Some(slot) = assignment.get_mut(scc as usize) else {
            return false;
        };
        *slot = target;
        let moved = Partition::from_assignment(assignment, parts.cluster_count());
        let after = self.physical_entry_counts(&moved);
        before.keys().chain(after.keys()).all(|path| {
            let prior = before
                .get(path)
                .copied()
                .unwrap_or(0)
                .saturating_sub(self.caps.folder);
            let next = after
                .get(path)
                .copied()
                .unwrap_or(0)
                .saturating_sub(self.caps.folder);
            next <= prior
        })
    }

    pub(super) fn permits_mirror_physical_capacity(
        &self,
        parts: &Partition,
        scc: u32,
        target: ClusterId,
        evidence: &MirrorEvidence,
        prospective: &MirrorEvidence,
    ) -> bool {
        if self.caps.folder == 0 {
            return true;
        }
        let before = self.physical_entry_counts_with_mirror_evidence(parts, evidence);
        let mut assignment = parts.assignment().to_vec();
        let Some(slot) = assignment.get_mut(scc as usize) else {
            return false;
        };
        *slot = target;
        let moved = Partition::from_assignment(assignment, parts.cluster_count());
        let after = self.physical_entry_counts_with_mirror_evidence(&moved, prospective);
        before.keys().chain(after.keys()).all(|path| {
            let prior = before
                .get(path)
                .copied()
                .unwrap_or(0)
                .saturating_sub(self.caps.folder);
            let next = after
                .get(path)
                .copied()
                .unwrap_or(0)
                .saturating_sub(self.caps.folder);
            next <= prior
        })
    }

    /// Re-checks a candidate with physical folder semantics while retaining
    /// the DTO walk for file and upper-level findings.
    pub(in crate::analyze) fn capacity_remainder_with_mirror_evidence(
        &self,
        parts: &Partition,
        evidence: &MirrorEvidence,
        rendered: &ContainerNode,
        capacity: &CapacityConfig,
    ) -> CapacityRemainder {
        let assembled = self.assemble_with_mirror_evidence(parts, evidence);
        Self::capacity_remainder_for_assembled(&assembled, rendered, capacity)
    }

    fn capacity_remainder_for_assembled(
        assembled: &CandidateTree,
        rendered: &ContainerNode,
        capacity: &CapacityConfig,
    ) -> CapacityRemainder {
        let folder_hard = physical_folder_findings(
            &physical_folder_entries_repository_relative(
                &assembled.tree,
                Some(&assembled.namespace_by_file),
            ),
            capacity.folder,
        )
        .into_iter()
        .filter(|finding| finding.severity == Severity::Violation)
        .count();
        let other: Vec<Level> = walk_all_capacity(rendered, capacity)
            .into_iter()
            .filter(|(level, finding)| {
                *level != Level::Folder && finding.severity == Severity::Violation
            })
            .map(|(level, _)| level)
            .collect();
        let remaining = folder_hard.saturating_add(other.len());
        let file_level = other.iter().filter(|&&level| level == Level::File).count();
        CapacityRemainder {
            remaining: u32::try_from(remaining).unwrap_or(u32::MAX),
            file_level: u32::try_from(file_level).unwrap_or(u32::MAX),
        }
    }

    /// Builds the solver, computing every seed-independent pipeline input once.
    #[allow(clippy::too_many_lines)]
    pub(in crate::analyze) fn new<P: ProfileSource + ?Sized>(
        snapshot: &'a Snapshot,
        profile_source: &P,
        coefficients: Coefficients,
        seed_identity: bool,
        tests: &'a TestPolicy,
    ) -> Self {
        let profile = profile_source.profile();
        let ir = snapshot.ir();
        let weights = profile.weights.kind_weights();
        let (files, index_of) = file_inventory(ir);

        let test_zone = test_zone_marks(tests, &files, &ir.nodes);

        let file_graph = build_file_graph(
            &ir.edges,
            &ir.nodes,
            &index_of,
            files.len(),
            &weights,
            &test_zone,
        );
        let condensation = condense(&file_graph);
        let forbidden_file_patterns: Vec<glob::Pattern> = profile
            .relocation
            .forbid_file_moves
            .iter()
            .filter_map(|pattern| glob::Pattern::new(pattern).ok())
            .collect();
        let forbidden_symbol_files = profile
            .relocation
            .forbid_symbol_moves
            .iter()
            .filter_map(|pattern| glob::Pattern::new(pattern).ok())
            .collect();
        let pinned_vertex: Vec<bool> = files
            .iter()
            .enumerate()
            .map(|(index, file)| {
                (profile.relocation.pin_detected_test_files
                    && test_zone.get(index).copied().unwrap_or(false))
                    || forbidden_file_patterns.iter().any(|pattern| {
                        pattern.matches_path(std::path::Path::new(file.name.as_str()))
                    })
            })
            .collect();
        let pinned_scc: Vec<bool> = condensation
            .members
            .iter()
            .map(|members| {
                members.iter().any(|member| {
                    pinned_vertex
                        .get(member.0 as usize)
                        .copied()
                        .unwrap_or(false)
                })
            })
            .collect();
        let caps = level_caps(&profile.capacity);
        let reverse_dag = reverse_csr(&condensation.dag);

        // folders are reality: the identity layout and the search's folder
        // partition start as the same object — each file SCC in its real
        // directory — so anchored seeding just clones it.
        let (identity_partition, identity_names, identity_synthetic) =
            real_dir_partition(&files, &condensation);
        let cycle_home_by_vertex = cycle_homes(&files, &condensation);
        let identity = seed_identity.then(|| identity_partition.clone());

        // FIX03 relieves over-capacity binding in the search grain itself: a
        // physical folder over its direct-file-plus-direct-child budget is
        // pre-split along priced connectivity into evidence-backed child groups
        // named to path-extend their base folder. The objective prices what this
        // creates, so every non-identity seed starts from a layout the split can
        // win from instead of only being able to shed files out of the over-cap
        // folder.
        let (relieved_files, mut search_partition, mut folder_names, mut folder_synthetic) =
            relieve_over_capacity(
                files,
                &condensation,
                &file_graph,
                &identity_partition,
                &identity_names,
                &identity_synthetic,
                caps.folder,
            );
        if pinned_scc.iter().any(|pinned| *pinned) {
            let mut assignment = search_partition.assignment().to_vec();
            for (scc, pinned) in pinned_scc.iter().copied().enumerate() {
                if pinned
                    && let (Some(slot), Some(home)) = (
                        assignment.get_mut(scc),
                        identity_partition.cluster_of(u32::try_from(scc).unwrap_or(u32::MAX)),
                    )
                {
                    *slot = home;
                }
            }
            search_partition =
                Partition::from_assignment(assignment, search_partition.cluster_count());
        }
        // FIX09 (naming-incoherence): a misnamed roof is invisible to edge-driven
        // search — the strangers under it carry no priced edge to pull them out,
        // so every seed converges on the same welded layout and no candidate can
        // ever propose the split. Where the signature fires, this synthesizes one
        // alternative start: zero-priced strangers regrouped by their own shared
        // tokens, a residual roof that misdescribes its remaining residents
        // dissolved into priced-connected places, each new place labeled from
        // member names to path-extend its base folder.
        // The proposal enters the ordinary pool at offset 1 — polish still runs,
        // the vetoes still bind, the objective still decides — and when it does
        // not fire, nothing downstream changes at all.
        let roof_rebuild = synthesize_roof_rebuild(
            &relieved_files,
            &condensation,
            &file_graph,
            &test_zone,
            &pinned_scc,
            &search_partition,
            &mut folder_names,
            &mut folder_synthetic,
        );
        let relocation_identity = RelocationIdentityGuard::new(
            &relieved_files,
            &condensation,
            &identity_partition,
            &search_partition,
            roof_rebuild.as_ref(),
        );
        let relocation_identity = if profile.relocation.allow_cross_package_moves {
            relocation_identity.lift_package_wall()
        } else {
            relocation_identity
        };
        let root_name = ir
            .containers
            .containers()
            .iter()
            .find(|container| container.level == ScopeLevel::PackageGroup)
            .map_or_else(|| SmolStr::new("workspace"), |group| group.name.clone());
        let facts = file_facts_repository_relative(
            snapshot,
            &weights,
            profile.capacity.folder,
            &relieved_files,
            &test_zone,
        );
        let real_is_identity = search_partition == identity_partition;

        Self {
            snapshot,
            test_zone,
            pinned_scc,
            forbidden_symbol_files,
            mirror_rules: mirror_rules(profile),
            mirror_enabled: profile.relocation.test_mirroring.enabled,
            pin_test_symbols: profile.relocation.pin_detected_test_symbols,
            files: relieved_files,
            index_of,
            condensation,
            relocation_identity,
            reverse_dag,
            caps,
            capacity: profile.capacity,
            coefficients,
            weights,
            same_file_symbol: profile.weights.same_file_symbol,
            same_file_type: profile.weights.same_file_type,
            identity,
            pass_start_partition: identity_partition,
            cycle_home_by_vertex,
            real_is_identity,
            real_partition: search_partition,
            real_folder_names: folder_names,
            real_folder_synthetic: folder_synthetic,
            base_seed: profile.seed,
            root_name,
            facts,
            roof_rebuild,
        }
    }

    /// Scores the five-level layout `parts` induces under this mode's
    /// coefficients.
    pub(in crate::analyze) fn evaluate(&self, parts: &Partition) -> f64 {
        let assembled = self.assemble(parts);
        self.evaluate_assembled(&assembled)
    }

    #[cfg(test)]
    pub(in crate::analyze) fn evaluate_with_mirror_evidence(
        &self,
        parts: &Partition,
        evidence: &MirrorEvidence,
    ) -> f64 {
        let assembled = self.assemble_with_mirror_evidence(parts, evidence);
        self.evaluate_assembled(&assembled)
    }

    fn evaluate_assembled(&self, assembled: &CandidateTree) -> f64 {
        let placement = |id: u32| assembled.placement.get(&id).copied();
        // the assembly's placement maps every node to its current file's
        // candidate id, so this distance is pure file-grain movement.
        let distance = move_distance(self.snapshot, &assembled.tree, &placement);
        let candidate = score_candidate(
            self.snapshot,
            &placement,
            &assembled.pass_start_file_by_candidate,
            &assembled.tree,
            &assembled.namespace_by_file,
            distance,
            &self.capacity,
            self.same_file_symbol,
            self.same_file_type,
        );
        score(&candidate, &self.coefficients, &self.weights).total
    }

    /// The J(T)-polish pass: sweeps every file SCC in deterministic order and
    /// greedily relocates it to the strongest-pulling folder whenever the move
    /// strictly lowers the full objective. Physical immediate-entry capacity
    /// (direct files plus direct child directories) and quotient cyclicity stay
    /// hard vetoes, never penalties — but the cyclicity veto is relative, not
    /// absolute: a move is barred when it *grows* either the number of folders
    /// caught in quotient cycles or the edges held inside those cycles, never
    /// for cyclicity the current layout already has. Misplaced files routinely
    /// entangle real folder graphs in cycles no single move can dissolve; an
    /// absolute veto would price every move at infinity on such a base and
    /// freeze the pass wholesale. On an acyclic base the two vetoes agree.
    /// At most [`POLISH_SWEEPS`] passes, stopping early once a sweep applies
    /// no move. Returns the final score so `solve` never re-evaluates.
    pub(in crate::analyze) fn polish(&self, parts: &mut Partition) -> f64 {
        let mut best = self.evaluate(parts);
        let initial_quotient = parts.quotient(&self.condensation.dag);
        let mut cyclic_base = CycleCounts::from_graph(&initial_quotient);
        for _ in 0..POLISH_SWEEPS {
            let mut improved = false;
            for scc in 0..self.condensation.members.len() {
                let scc32 = u32::try_from(scc).unwrap_or(u32::MAX);
                if self.pinned_scc.get(scc).copied().unwrap_or(false) {
                    continue;
                }
                let Some(source) = parts.cluster_of(scc32) else {
                    continue;
                };
                for target in self.pull_targets(parts, scc32, source) {
                    if !self.relocation_identity.permits_join(parts, scc32, target) {
                        continue;
                    }
                    // FIX05 (WS-D anchored-inversion): a bridge is not a member of
                    // the thing it bridges. When an SCC's priced edges reach a
                    // folder besides the pair (current, target) — main.py importing
                    // three features, pipeline.py bridging billing and telemetry —
                    // absorbing it into one side strands the rest of its boundary,
                    // yet every locally-scored statistic of the absorber improves:
                    // the adopted edges drop to folder height while the abandoned
                    // ones keep whatever height they already had. The objective
                    // alone therefore ratifies the absorption and greenfield
                    // out-churns anchored, inverting the product promise (the
                    // eval corpus's inversion witness). The veto is structural,
                    // not scored: it fires before evaluation, needs no reference
                    // to the current layout, and so binds both modes equally —
                    // the FIX04 pattern of enforcing contract intent where
                    // admission cannot see it. Zero-priced edges nominate nothing
                    // here either: they never bind placement.
                    if self.absorbs_a_foreign_anchor(parts, scc32, source, target) {
                        continue;
                    }
                    // FIX05 (companion veto): the mirror image of bridge
                    // absorption. An SCC whose current folder pulls at least as
                    // hard as the destination is being torn from measured
                    // company for speculative proximity — the channel that
                    // survived the bridge veto: with the facade unabsorbable,
                    // greedy polish instead walked the feature members out of
                    // their real directories toward it, one transiently cheap
                    // step at a time. Relocation is honest only when the
                    // destination out-pulls what would be stranded (the
                    // satellite joining its sole anchor); a tie resolves to
                    // staying, because folders are reality until priced
                    // evidence says otherwise.
                    if self.strands_a_comparable_anchor(parts, scc32, source, target) {
                        continue;
                    }
                    // FIX05 (third veto): the synthetic bucket is not a place.
                    // `workspace` is the fallback name for files whose real
                    // directory is the project root — an absence of structure,
                    // not a structure. Once the first two vetoes sealed the
                    // feature folders, greedy polish found the remaining exit:
                    // feature members fleeing their real directories INTO the
                    // bucket, because sitting beside the unabsorbable hub
                    // cheapens their hub edges while the bucket prices nothing
                    // back. That flight is the collapse defect itself (real
                    // directories swallowed by an invented container). A file
                    // with priced company in its own folder therefore may not
                    // relocate into the bucket at all; only files reality left
                    // loose belong there, and they are already home.
                    if self.flees_into_the_synthetic_bucket(parts, scc32, source, target) {
                        continue;
                    }
                    if !self.permits_physical_capacity(parts, scc32, target) {
                        continue;
                    }
                    if !parts.move_node(scc32, target) {
                        continue;
                    }
                    let tentative_quotient = parts.quotient(&self.condensation.dag);
                    let cyclic_now = CycleCounts::from_graph(&tentative_quotient);
                    let total = if cyclic_now.exceeds(cyclic_base) {
                        f64::INFINITY
                    } else {
                        self.evaluate(parts)
                    };
                    if total < best {
                        best = total;
                        cyclic_base = cyclic_now;
                        improved = true;
                        break;
                    }
                    parts.move_node(scc32, source);
                }
            }
            if !improved {
                break;
            }
        }
        best
    }

    /// Ranks the folders pulling hardest on `scc` — summed edge weight over both
    /// directions — and returns up to [`POLISH_TARGETS`] of them, strongest
    /// first, ties broken by the lower cluster id. Zero-priced edges nominate
    /// nothing: they never bind placement (the FIX04 doctrine), so a folder
    /// connected only through re-exports is never offered as a move target.
    pub(in crate::analyze) fn pull_targets(
        &self,
        parts: &Partition,
        scc: u32,
        source: ClusterId,
    ) -> Vec<ClusterId> {
        let mut pull: BTreeMap<ClusterId, f64> = BTreeMap::new();
        for graph in [&self.condensation.dag, &self.reverse_dag] {
            let weights = graph.weights(scc);
            for (slot, &neighbour) in graph.neighbors(scc).iter().enumerate() {
                let Some(cluster) = parts.cluster_of(neighbour) else {
                    continue;
                };
                if cluster == source {
                    continue;
                }
                let weight = weights.get(slot).copied().unwrap_or(0.0);
                if weight <= 0.0 {
                    // FIX04 doctrine: a zero-priced edge never binds placement.
                    continue;
                }
                *pull.entry(cluster).or_insert(0.0) += f64::from(weight);
            }
        }
        let mut ranked: Vec<(ClusterId, f64)> = pull.into_iter().collect();
        ranked.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.0.cmp(&right.0))
        });
        ranked
            .into_iter()
            .take(POLISH_TARGETS)
            .map(|(cluster, _)| cluster)
            .collect()
    }
}

impl Solver for PipelineSolver<'_> {
    type Evidence = MirrorEvidence;

    fn solve(&self, seed: u64) -> SolvedCandidate<MirrorEvidence> {
        let offset = seed.wrapping_sub(self.base_seed);
        if offset == 0
            && let Some(identity) = &self.identity
        {
            return self.identity_entry(identity);
        }
        // lean: every non-identity seed converges on the same polished layout —
        // folders are reality, so the folder-level seed perturbation that used
        // to differentiate restarts is gone and the pool collapses toward
        // identity plus one improvement candidate. FIX09 re-sources diversity at
        // exactly one point: when the naming-incoherence signature fired at
        // construction, offset 1 starts from the synthesized roof rebuild and
        // runs the identical polish/score path on it, so the pool carries a
        // genuinely different shape that the objective ratifies or rejects like
        // any other. Every other offset polishes reality unchanged.
        let start = match (&self.roof_rebuild, offset) {
            (Some(rebuild), 1) => rebuild,
            _ => &self.real_partition,
        };
        let mut parts = start.clone();
        self.polish(&mut parts);
        // The shadow pass runs on the polished layout so a test file follows
        // the placement its subject actually earned, not the one reality
        // suggested; production placements are never moved by it.
        let mirror_evidence = self.shadow_tests(&mut parts);
        // FIX08: the file polish's layout is refined by the symbol-grain pass
        // before scoring, so pool ranking prices symbol relocation too. The
        // outcome itself is not threaded out — `build_candidate` re-runs this
        // pure, deterministic pass on the identical partition and gets the
        // identical overlay, so ranking score and DTO score agree by
        // construction. (The polish's own total is subsumed: the symbol pass
        // re-prices the identical layout before improving on it.)
        let symbols = self.symbol_polish_with_mirror_evidence(&parts, &mirror_evidence);
        self.finish(parts, symbols.total, mirror_evidence)
    }
}

/// One symbol relocation the FIX08 symbol polish accepted, in candidate-tree
/// file ids. `delta` is the strict J improvement it earned at acceptance time.
pub(in crate::analyze) fn test_zone_marks(
    tests: &TestPolicy,
    files: &[FileInfo],
    nodes: &[Node],
) -> Vec<bool> {
    let mut marks: Vec<bool> = files
        .iter()
        .map(|file| {
            tests.matches(&file.name)
                || (tests.builtins && TestPolicy::matches_builtin_path(&file.name))
        })
        .collect();
    if tests.builtins {
        let mut case_only: BTreeMap<u32, bool> = BTreeMap::new();
        for node in nodes {
            let entry = case_only.entry(node.container.0).or_insert(true);
            *entry &= node.polarity != Polarity::Production;
        }
        for (index, file) in files.iter().enumerate() {
            if !case_only.get(&file.container).copied().unwrap_or(false) {
                continue;
            }
            if let Some(mark) = marks.get_mut(index) {
                *mark = true;
            }
        }
    }
    marks
}

pub(in crate::analyze) fn build_file_graph(
    edges: &[Edge],
    nodes: &[Node],
    index_of: &BTreeMap<u32, u32>,
    file_count: usize,
    weights: &KindWeights,
    test_zone: &[bool],
) -> Csr {
    let container_of: BTreeMap<u32, u32> = nodes
        .iter()
        .map(|node| (node.id.0, node.container.0))
        .collect();
    let mut crossings: Vec<(u32, u32, f32)> = Vec::new();
    for edge in edges {
        // admit every edge at its configured price — not Hard edges alone — so
        // the search optimizes the same cut the score reports and soft-only
        // files (e.g. type-reference-only TS) get a non-empty move-set. A zero-
        // priced edge stays in the graph but binds nothing: polish never
        // nominates it as a move target and matching never contracts across it
        // (the FIX04 doctrine).
        let (Some(source), Some(target)) = (
            container_of.get(&edge.source.0),
            container_of.get(&edge.target.0),
        ) else {
            continue;
        };
        let (Some(&from), Some(&to)) = (index_of.get(source), index_of.get(target)) else {
            continue;
        };
        if from == to {
            continue;
        }
        // reason: csr weights are f32 by contract (ad-6); narrowing the f64 price is the one lossy step
        #[allow(clippy::cast_possible_truncation)]
        let weight = weights.edge_weight(edge.kind, edge.confidence) as f32;
        // The test tie-cut prices an edge touching a test-zone file at zero —
        // both directions, test↔test included — so test coupling can neither
        // weld a spec to its subject nor bond test files into a place of their
        // own. This is the single pricing choke point every downstream stage
        // (relief piles, polish moves, heavy-edge matching) reads.
        let weight = if test_zone.get(from as usize).copied().unwrap_or(false)
            || test_zone.get(to as usize).copied().unwrap_or(false)
        {
            0.0
        } else {
            weight
        };
        crossings.push((from, to, weight));
    }
    Csr::from_weighted_edges(file_count, &crossings)
}

/// Returns `graph` with every edge reversed, weights preserved.
pub(in crate::analyze) fn reverse_csr(graph: &Csr) -> Csr {
    let mut edges: Vec<(u32, u32, f32)> = Vec::with_capacity(graph.edge_count());
    for vertex in 0..graph.vertex_count() {
        let from = u32::try_from(vertex).unwrap_or(u32::MAX);
        let weights = graph.weights(from);
        for (slot, &to) in graph.neighbors(from).iter().enumerate() {
            edges.push((to, from, weights.get(slot).copied().unwrap_or(0.0)));
        }
    }
    Csr::from_weighted_edges(graph.vertex_count(), &edges)
}
