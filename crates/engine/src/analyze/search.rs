use std::collections::BTreeMap;

use strata_core::condense::condense;
use strata_core::diversify::{ModeConfig, ModeResult as CoreModeResult, SolvedCandidate};
use strata_core::diversify::{diversify, vi_distance};
use strata_core::graph::csr::{HardnessFilter, build_csr};
use strata_core::score::KindWeights;
use strata_core::shatter::{BreakSet, EdgeRef, EdgeWeights, SccView, shatter};
use strata_ir::{Hardness, Node, NodeId, NodeKind, Polarity, Snapshot};

use crate::analyze::findings::{conditional_splits, hard_capacity_breaks};
use crate::analyze::relocation::mirror::PolishEvidence;
use crate::analyze::relocation::{PipelineSolver, TestPolicy};
use crate::analyze::scoring::{ProfileSource, score_current_with_affinity};
use crate::config::ProfileConfig;
use crate::error::StrataError;
use crate::result::{CurrentStanding, ProfileCurrent, ProfileResult, Violation};

#[cfg(test)]
mod tests;

/// Builds one mode's result by running the diversifying restructuring search.
///
/// The mode's [`Coefficients`] select anchored vs greenfield behaviour. The
/// search runs the full multilevel scheme per seed (multi-start), scores each
/// assembled five-level layout under the mode's objective, and diversifies to up
/// to `k` genuinely different candidates by max-min variation of information.
/// On a cap-clean tree the pool also carries the identity layout — "change
/// nothing", scored at the true current tree — so a suggested restructuring can
/// never silently lose to the current layout.
///
/// # Errors
///
/// Returns [`StrataError::SnapshotInvalid`] if a candidate tree cannot be rendered.
pub(in crate::analyze) fn build_profile_result(
    snapshot: &Snapshot,
    profile: &ProfileConfig,
    findings: &[Violation],
) -> Result<ProfileResult, StrataError> {
    let coefficients = profile.objective.coefficients();
    let tests = TestPolicy::new(&profile.tests)?;
    let capacity_breaks = hard_capacity_breaks(findings);
    let capacity_clean = capacity_breaks == 0;
    let cycles = solve_cycles(snapshot, profile, &profile.weights.kind_weights());
    let splits = conditional_splits(&cycles, snapshot, profile.capacity.file);
    let seed_identity = capacity_clean;
    let solver = PipelineSolver::new(snapshot, profile, coefficients, seed_identity, &tests);
    let mode_config = mode_config(profile);
    let CoreModeResult {
        candidates,
        solution_space_converged,
    } = diversify(&solver, &mode_config);

    let current_breakdown = score_current_with_affinity(
        snapshot,
        &coefficients,
        &profile.weights.kind_weights(),
        &profile.capacity,
        profile.weights.same_file_symbol,
        profile.weights.same_file_type,
    );
    // A suggestion must strictly beat keeping today's layout: the diversifier's
    // tolerance band measures against the pool's own best, so a diverse shape can
    // clear that bar yet still price above the current tree (the tie-cut withdrew
    // the test-edge pulls that used to keep every diversifier ahead). Candidates
    // scored above the current layout under this mode's own objective are
    // therefore never offered. This applies even when the current tree breaches
    // a hard cap: infeasibility is reported as a finding, not used to relabel a
    // score regression as a gain.
    let valid: Vec<&SolvedCandidate<PolishEvidence>> = candidates
        .iter()
        .filter(|solved| {
            solver
                .relocation_identity
                .accepts_with_mirrors(&solved.partition, &solved.evidence)
        })
        .collect();
    let offered: Vec<SolvedCandidate<PolishEvidence>> = valid
        .iter()
        .filter(|solved| solved.score < current_breakdown.total)
        .map(|solved| (*solved).clone())
        .collect();
    let current_tree = &snapshot.ir().containers;
    let mut built = Vec::with_capacity(offered.len());
    for (index, solved) in offered.iter().enumerate() {
        let mut candidate = solver.build_candidate(
            current_tree,
            solved,
            u32::try_from(index + 1).unwrap_or(u32::MAX),
            &splits,
        )?;
        candidate.improvement = current_breakdown.total - candidate.score;
        // an infeasible standing must say what each candidate actually fixes,
        // so its tree is re-checked against the same caps as the current one.
        candidate.capacity_remainder = (!capacity_clean).then(|| {
            solver.capacity_remainder_with_polish_evidence(
                &solved.partition,
                &solved.evidence,
                &candidate.tree,
                &profile.capacity,
            )
        });
        built.push(candidate);
    }

    let current_standing = if capacity_clean {
        // the current file layout wins only when nothing beats it: a faithful
        // partition whose symbol polish still lowered the score is an offer,
        // not the identity.
        let identity_won = valid.first().is_some_and(|best| {
            solver.identity.as_ref() == Some(&best.partition)
                && best.score >= current_breakdown.total
        });
        if identity_won {
            CurrentStanding::Optimal
        } else {
            CurrentStanding::Outscored
        }
    } else {
        CurrentStanding::Infeasible
    };

    let pairwise_distance = pairwise_distances(&offered);
    Ok(ProfileResult {
        parameters: profile.clone(),
        current: ProfileCurrent {
            score: current_breakdown.total,
            score_breakdown: current_breakdown.into(),
            unique_findings: findings.to_vec(),
            standing: current_standing,
            capacity_breaks,
        },
        candidates: built,
        pairwise_distance,
        solution_space_converged,
    })
}

/// Returns the variation-of-information matrix over the diversified candidates.
fn pairwise_distances(candidates: &[SolvedCandidate<PolishEvidence>]) -> Vec<Vec<f64>> {
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
fn mode_config(profile: &ProfileConfig) -> ModeConfig {
    ModeConfig {
        k: profile.candidates as usize,
        base_seed: profile.seed,
        score_tolerance: profile.diversity.score_tolerance,
        min_distance: profile.diversity.min_distance,
        pool_per_candidate: profile.diversity.seeds_per_candidate as usize,
    }
}

/// One file container of the current tree: the movable atom of the search.
///
/// Clustering, capacity, narration, and move distance all treat the file as
/// indivisible in this slice (symbol-level packing is the one unglued phase), so
/// the search graph's vertices are files rather than symbols. Folder capacity is
/// measured separately from the rendered physical tree as direct files plus
/// distinct direct child directories.
/// Builds a node-id to name lookup keyed by the raw id.
pub(in crate::analyze) fn node_names(snapshot: &Snapshot) -> BTreeMap<u32, String> {
    snapshot
        .ir()
        .nodes
        .iter()
        .map(|node| (node.id.0, node.name.to_string()))
        .collect()
}

/// One solved multi-member SCC: its members, config-priced internal edges, a
/// minimum-weight break set, and the members' total production SLOC.
///
/// `pair_weights` keys are *local* indices into `members` — the same numbering
/// the break set's [`EdgeRef`]s use.
pub(in crate::analyze) struct SccSolution {
    /// The SCC's member nodes, ascending id.
    pub(in crate::analyze) members: Vec<NodeId>,
    /// Summed edge weight per directed local pair.
    pub(in crate::analyze) pair_weights: BTreeMap<(u32, u32), f64>,
    /// The MFAS solution over the SCC.
    pub(in crate::analyze) break_set: BreakSet,
    /// Total production SLOC across members (drives conditional splits).
    pub(in crate::analyze) production_sloc: u64,
}

/// Runs MFAS over every multi-member SCC of the hard-edge graph.
///
/// SCCs solve sequentially in condensation order: `shatter_all` is unusable
/// here because each SCC gets its own dense local numbering, and one shared
/// weight table would collide the [`EdgeRef`]s. Edge prices come from the
/// config's kind-weight table so break suggestions rank by the same currency
/// the objective charges.
pub(in crate::analyze) fn solve_cycles<P: ProfileSource + ?Sized>(
    snapshot: &Snapshot,
    profile_source: &P,
    weights: &KindWeights,
) -> Vec<SccSolution> {
    let profile = profile_source.profile();
    let views = build_csr(snapshot, HardnessFilter::HardOnly);
    let condensation = condense(&views.forward);
    let ir = snapshot.ir();
    let limits = profile.solver.limits();
    let node_by_id: BTreeMap<u32, &Node> = ir.nodes.iter().map(|node| (node.id.0, node)).collect();

    condensation
        .members
        .iter()
        .filter(|members| members.len() > 1)
        .map(|members| {
            let local: BTreeMap<u32, u32> = members
                .iter()
                .enumerate()
                .map(|(index, node)| (node.0, u32::try_from(index).unwrap_or(u32::MAX)))
                .collect();

            let mut pair_weights: BTreeMap<(u32, u32), f64> = BTreeMap::new();
            for edge in &ir.edges {
                if edge.hardness != Hardness::Hard {
                    continue;
                }
                let (Some(&source), Some(&target)) =
                    (local.get(&edge.source.0), local.get(&edge.target.0))
                else {
                    continue;
                };
                if source == target {
                    continue;
                }
                let affinity = match (
                    node_by_id.get(&edge.source.0),
                    node_by_id.get(&edge.target.0),
                ) {
                    (Some(source_node), Some(target_node))
                        if source_node.container == target_node.container =>
                    {
                        if source_node.kind == NodeKind::Type || target_node.kind == NodeKind::Type
                        {
                            profile.weights.same_file_type
                        } else {
                            profile.weights.same_file_symbol
                        }
                    }
                    _ => 1.0,
                };
                *pair_weights.entry((source, target)).or_insert(0.0) +=
                    weights.edge_weight(edge.kind, edge.confidence) * affinity;
            }

            let view = SccView::new(
                u32::try_from(members.len()).unwrap_or(u32::MAX),
                pair_weights
                    .keys()
                    .map(|&(source, target)| EdgeRef { source, target }),
            );
            let table = EdgeWeights::from_pairs(
                pair_weights
                    .iter()
                    .map(|(&(source, target), &weight)| (EdgeRef { source, target }, weight)),
            );
            let break_set = shatter(&view, &table, &limits);

            let production_sloc = members
                .iter()
                .filter_map(|node| node_by_id.get(&node.0))
                .filter(|node| node.polarity == Polarity::Production)
                .map(|node| u64::from(node.effective_size))
                .sum();

            SccSolution {
                members: members.clone(),
                pair_weights,
                break_set,
                production_sloc,
            }
        })
        .collect()
}
