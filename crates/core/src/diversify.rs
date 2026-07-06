//! Multi-start diversification by variation of information.
//!
//! Returning the top-k candidates by score alone yields k near-clones of one
//! local optimum — useless when a human is meant to *choose* between structures.
//! Diversification instead runs `10 * k` deterministic seeded restarts of the
//! cluster + pack stages, keeps the local optima within a score-tolerance band of
//! the best, then greedily selects k that are genuinely different by max-min
//! *variation of information* (Meilă 2007), a true metric on partitions.
//!
//! Determinism is contractual: the same seeds always produce the same pool, and
//! the parallel restarts are reduced in seed order so thread scheduling can never
//! leak into the result. If the surviving pool collapses below k, fewer are
//! returned with [`ModeResult::solution_space_converged`] set — itself a
//! meaningful signal that the solution space is narrow (AD-2).
//!
//! The restart itself is supplied as a [`Solver`]: diversification owns the
//! oversampling, filtering, and selection logic but is agnostic to how a single
//! seed becomes a scored candidate, so the cluster + pack pipeline plugs in
//! without this module depending on every phase.

use std::collections::BTreeMap;

use rayon::prelude::*;

use crate::cluster::Partition;

/// Configuration for one diversification run.
///
/// `k` is the number of candidates the caller wants; `base_seed` anchors the
/// `pool_per_candidate * k` restarts at `base_seed + i`. `score_tolerance`
/// widens the keep-band to `(1 + tolerance) * best`; `min_distance` is the
/// minimum pairwise VI two selected candidates must exceed to both be kept.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeConfig {
    /// How many diverse candidates to return.
    pub k: usize,
    /// The base seed; restart `i` runs with `base_seed + i`.
    pub base_seed: u64,
    /// Relative score tolerance for the keep-band, e.g. `0.05` for 5%.
    pub score_tolerance: f64,
    /// Minimum pairwise variation of information between selected candidates.
    pub min_distance: f64,
    /// Restart pool multiplier: the pool holds `pool_per_candidate * k`
    /// restarts (the `[diversity].seeds-per-candidate` config key).
    pub pool_per_candidate: usize,
}

/// One solved candidate from a single restart: its induced symbol partition and
/// the objective score it achieved.
///
/// The partition drives VI selection; the score drives the tolerance filter and
/// the best-first ordering. Lower scores are better (the objective is minimised).
#[derive(Debug, Clone, PartialEq)]
pub struct SolvedCandidate {
    /// The induced partition of symbols into containers.
    pub partition: Partition,
    /// The objective score J(T); lower is better.
    pub score: f64,
}

/// A restartable solver: maps a seed to a single scored candidate.
///
/// Each call must be a pure, deterministic function of its seed (re-running
/// cluster + pack with that seed), so the pool is reproducible. The trait is the
/// only coupling between diversification and the rest of the pipeline.
pub trait Solver: Sync {
    /// Solves once with the given `seed`, returning the scored candidate.
    fn solve(&self, seed: u64) -> SolvedCandidate;
}

/// The result of a diversification run: the chosen candidates and whether the
/// solution space converged.
///
/// `candidates` are ordered best score first. `solution_space_converged` is true
/// exactly when fewer than `k` distinct candidates survived selection — a signal
/// that the structure is largely determined, not a failure.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeResult {
    /// The selected candidates, best score first.
    pub candidates: Vec<SolvedCandidate>,
    /// True iff fewer than `k` candidates survived.
    pub solution_space_converged: bool,
}

/// Runs `pool_per_candidate * k` seeded restarts, filters to the
/// score-tolerance band, and greedily selects up to `k` candidates that are
/// maximally different by VI.
///
/// The restarts run in parallel via rayon but are reduced in seed order, so the
/// pool — and therefore the result — is independent of thread scheduling. The
/// keep-band is `score <= (1 + score_tolerance) * best`. Selection picks the best
/// candidate, then repeatedly the candidate maximising its minimum VI distance to
/// those already picked, requiring that minimum to be at least `min_distance`.
/// When fewer than `k` survive, all survivors are returned with
/// `solution_space_converged = true`.
///
/// A `k` of zero yields no candidates and is *not* a convergence signal — zero
/// were requested and zero returned, so `solution_space_converged` stays false.
#[must_use]
pub fn diversify(solver: &impl Solver, cfg: &ModeConfig) -> ModeResult {
    let pool = solve_pool(solver, cfg);
    let filtered = filter_by_tolerance(pool, cfg.score_tolerance);
    let mut picked = select_diverse(&filtered, cfg.k, cfg.min_distance);
    // selection order is diversity-greedy, not score order; re-sort so the
    // promised "best score first" contract holds for every candidate, not just
    // the first (the best stays index 0: it is minimal under the same key).
    picked.sort_by(candidate_order);
    let converged = picked.len() < cfg.k;

    ModeResult {
        candidates: picked,
        solution_space_converged: converged,
    }
}

/// Solves `pool_per_candidate * k` restarts at `base_seed + i` in parallel and
/// returns them in ascending seed order, so the pool is deterministic
/// regardless of scheduling.
fn solve_pool(solver: &impl Solver, cfg: &ModeConfig) -> Vec<SolvedCandidate> {
    let count = cfg.k.saturating_mul(cfg.pool_per_candidate);
    (0..count)
        .into_par_iter()
        .map(|i| {
            let seed = cfg
                .base_seed
                .wrapping_add(u64::try_from(i).unwrap_or(u64::MAX));
            solver.solve(seed)
        })
        .collect()
}

/// Keeps the candidates within `(1 + tolerance) * best` of the best score, sorted
/// best score first with ties broken deterministically by partition assignment.
///
/// An empty pool stays empty. The best score is the minimum; a non-negative
/// tolerance widens the band above it.
fn filter_by_tolerance(mut pool: Vec<SolvedCandidate>, tolerance: f64) -> Vec<SolvedCandidate> {
    pool.sort_by(candidate_order);

    let Some(best) = pool.first().map(|candidate| candidate.score) else {
        return pool;
    };
    let ceiling = tolerance_ceiling(best, tolerance);
    pool.retain(|candidate| candidate.score <= ceiling);
    pool
}

/// Total order on candidates: ascending score (best first), ties broken
/// deterministically by partition assignment.
fn candidate_order(a: &SolvedCandidate, b: &SolvedCandidate) -> std::cmp::Ordering {
    a.score
        .total_cmp(&b.score)
        .then_with(|| a.partition.assignment().cmp(b.partition.assignment()))
}

/// Returns the upper score bound of the keep-band, `(1 + tolerance) * best`,
/// handling the negative-best case so the band still widens upward.
fn tolerance_ceiling(best: f64, tolerance: f64) -> f64 {
    // For a positive best, (1 + t) * best lifts the ceiling; for a negative best
    // the same product would *lower* it, so add the slack as an absolute margin.
    if best >= 0.0 {
        best * (1.0 + tolerance)
    } else {
        best - best * tolerance
    }
}

/// Greedily selects up to `k` candidates from a score-sorted `pool` by max-min
/// variation of information.
///
/// The first pick is the best-scoring candidate. Each later pick is the candidate
/// whose minimum VI distance to the already-picked set is greatest, provided that
/// minimum is at least `min_distance`; ties break toward the better score (the
/// earlier pool index). Selection stops at `k` picks or when no remaining
/// candidate clears `min_distance`.
fn select_diverse(pool: &[SolvedCandidate], k: usize, min_distance: f64) -> Vec<SolvedCandidate> {
    if k == 0 {
        return Vec::new();
    }
    let Some(first) = pool.first() else {
        return Vec::new();
    };

    let mut picked_indices = vec![0_usize];
    let mut picked = vec![first.clone()];

    while picked.len() < k {
        let Some(next) = best_remaining(pool, &picked_indices, min_distance) else {
            break;
        };
        picked_indices.push(next);
        if let Some(candidate) = pool.get(next) {
            picked.push(candidate.clone());
        }
    }

    picked
}

/// Returns the pool index of the unpicked candidate whose minimum VI distance to
/// the picked set is greatest and at least `min_distance`, or `None` when none
/// qualifies. Ties favour the lower index (the better score).
fn best_remaining(
    pool: &[SolvedCandidate],
    picked_indices: &[usize],
    min_distance: f64,
) -> Option<usize> {
    let mut best: Option<(f64, usize)> = None;
    for (index, candidate) in pool.iter().enumerate() {
        if picked_indices.contains(&index) {
            continue;
        }
        let min_vi = picked_indices
            .iter()
            .filter_map(|&picked| pool.get(picked))
            .map(|other| vi_distance(&candidate.partition, &other.partition))
            .fold(f64::INFINITY, f64::min);
        if min_vi < min_distance {
            continue;
        }
        if best.is_none_or(|(best_vi, _)| min_vi > best_vi) {
            best = Some((min_vi, index));
        }
    }
    best.map(|(_, index)| index)
}

/// Variation of information (Meilă 2007) between two partitions of the same
/// element set: `vi(x, y) = H(x) + H(y) - 2 I(x, y)`.
///
/// `H` is the entropy of a partition and `I` the mutual information between the
/// two; the result is a true metric on partitions — zero iff the partitions are
/// identical, larger as they diverge. Only the elements covered by both
/// partitions are compared; when that overlap is empty the distance is zero.
#[must_use]
pub fn vi_distance(a: &Partition, b: &Partition) -> f64 {
    let count = a.node_count().min(b.node_count());
    if count == 0 {
        return 0.0;
    }
    let total = f64::from(u32::try_from(count).unwrap_or(u32::MAX));

    let sizes_a = cluster_sizes(a, count);
    let sizes_b = cluster_sizes(b, count);
    let joint = joint_sizes(a, b, count);

    let entropy_a = entropy(&sizes_a, total);
    let entropy_b = entropy(&sizes_b, total);
    let mutual = mutual_information(&joint, &sizes_a, &sizes_b, total);

    // Floating-point error can push an exact match a hair below zero; clamp it so
    // VI stays a non-negative metric.
    (entropy_a + entropy_b - 2.0 * mutual).max(0.0)
}

/// Counts how many of the first `count` elements fall in each cluster of `part`.
fn cluster_sizes(part: &Partition, count: usize) -> BTreeMap<u32, u32> {
    let mut sizes: BTreeMap<u32, u32> = BTreeMap::new();
    for node in 0..count {
        if let Some(cluster) = part.cluster_of(u32::try_from(node).unwrap_or(u32::MAX)) {
            *sizes.entry(cluster.0).or_insert(0) += 1;
        }
    }
    sizes
}

/// Counts the co-occurrence of cluster pairs across the first `count` elements:
/// the size of each `(cluster in a, cluster in b)` intersection.
fn joint_sizes(a: &Partition, b: &Partition, count: usize) -> BTreeMap<(u32, u32), u32> {
    let mut joint: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    for node in 0..count {
        let key = u32::try_from(node).unwrap_or(u32::MAX);
        let (Some(cluster_a), Some(cluster_b)) = (a.cluster_of(key), b.cluster_of(key)) else {
            continue;
        };
        *joint.entry((cluster_a.0, cluster_b.0)).or_insert(0) += 1;
    }
    joint
}

/// Returns the Shannon entropy (in nats) of a partition described by its cluster
/// sizes over `total` elements.
fn entropy(sizes: &BTreeMap<u32, u32>, total: f64) -> f64 {
    sizes
        .values()
        .map(|&size| {
            let probability = f64::from(size) / total;
            if probability > 0.0 {
                -probability * probability.ln()
            } else {
                0.0
            }
        })
        .sum()
}

/// Returns the mutual information (in nats) between two partitions from their
/// joint and marginal cluster sizes over `total` elements.
fn mutual_information(
    joint: &BTreeMap<(u32, u32), u32>,
    sizes_a: &BTreeMap<u32, u32>,
    sizes_b: &BTreeMap<u32, u32>,
    total: f64,
) -> f64 {
    joint
        .iter()
        .map(|(&(cluster_a, cluster_b), &shared)| {
            let joint_p = f64::from(shared) / total;
            let marginal_a = f64::from(sizes_a.get(&cluster_a).copied().unwrap_or(0)) / total;
            let marginal_b = f64::from(sizes_b.get(&cluster_b).copied().unwrap_or(0)) / total;
            if joint_p > 0.0 && marginal_a > 0.0 && marginal_b > 0.0 {
                joint_p * (joint_p / (marginal_a * marginal_b)).ln()
            } else {
                0.0
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::ClusterId;

    /// Builds a partition from a raw cluster label per node.
    fn partition(labels: &[u32]) -> Partition {
        let count = labels
            .iter()
            .copied()
            .max()
            .map_or(0, |max| max as usize + 1);
        Partition::from_assignment(labels.iter().copied().map(ClusterId).collect(), count)
    }

    /// Returns whether two distances are equal within floating-point tolerance.
    fn close(left: f64, right: f64) -> bool {
        (left - right).abs() < f64::EPSILON
    }

    /// Builds a solved candidate from labels and a score.
    fn candidate(labels: &[u32], score: f64) -> SolvedCandidate {
        SolvedCandidate {
            partition: partition(labels),
            score,
        }
    }

    /// A solver that replays a fixed list of candidates, indexed by `seed - base`,
    /// recording each seed it is asked to solve.
    struct ScriptedSolver {
        base: u64,
        candidates: Vec<SolvedCandidate>,
    }

    impl Solver for ScriptedSolver {
        fn solve(&self, seed: u64) -> SolvedCandidate {
            let index = usize::try_from(seed.wrapping_sub(self.base)).unwrap_or(usize::MAX);
            self.candidates
                .get(index)
                .cloned()
                .unwrap_or_else(|| candidate(&[0], f64::INFINITY))
        }
    }

    #[test]
    fn should_be_zero_for_identical_partitions() {
        let part = partition(&[0, 0, 1, 1]);

        assert!(close(vi_distance(&part, &part), 0.0));
    }

    #[test]
    fn should_be_positive_for_differing_partitions() {
        let a = partition(&[0, 0, 1, 1]);
        let b = partition(&[0, 1, 0, 1]);

        assert!(vi_distance(&a, &b) > 0.0);
    }

    #[test]
    fn should_be_symmetric() {
        let a = partition(&[0, 0, 1, 2]);
        let b = partition(&[0, 1, 1, 1]);

        let forward = vi_distance(&a, &b);
        let backward = vi_distance(&b, &a);
        assert!(close(forward, backward));
    }

    #[test]
    fn should_be_zero_when_there_is_no_overlap() {
        let a = partition(&[]);
        let b = partition(&[0, 1]);

        assert!(close(vi_distance(&a, &b), 0.0));
    }

    #[test]
    fn should_select_the_best_then_the_most_distant() {
        // three candidates: best (score 1) clustered [0,0,1,1]; a near-clone of it
        // (score 1.01); and a distant one (score 2). With k=2 and a min distance,
        // selection must pick the best then the distant, skipping the clone.
        let candidates = vec![
            candidate(&[0, 0, 1, 1], 1.0),
            candidate(&[0, 0, 1, 1], 1.01),
            candidate(&[0, 1, 0, 1], 2.0),
        ];
        let solver = ScriptedSolver {
            base: 100,
            candidates,
        };
        let cfg = ModeConfig {
            k: 2,
            base_seed: 100,
            score_tolerance: 10.0,
            min_distance: 0.1,
            pool_per_candidate: 10,
        };

        let result = diversify(&solver, &cfg);

        assert_eq!(result.candidates.len(), 2);
        assert_eq!(result.candidates.first().map(|c| c.score), Some(1.0));
        assert_eq!(result.candidates.get(1).map(|c| c.score), Some(2.0));
        assert!(!result.solution_space_converged);
    }

    #[test]
    fn should_drop_candidates_outside_the_score_tolerance() {
        // only the first candidate is within 5% of the best; the rest are pruned,
        // so fewer than k survive and the run reports convergence.
        let candidates = vec![
            candidate(&[0, 0, 1, 1], 1.0),
            candidate(&[0, 1, 0, 1], 2.0),
            candidate(&[0, 1, 1, 0], 3.0),
        ];
        let solver = ScriptedSolver {
            base: 0,
            candidates,
        };
        let cfg = ModeConfig {
            k: 3,
            base_seed: 0,
            score_tolerance: 0.05,
            min_distance: 0.0,
            pool_per_candidate: 10,
        };

        let result = diversify(&solver, &cfg);

        assert_eq!(result.candidates.len(), 1);
        assert!(result.solution_space_converged);
    }

    #[test]
    fn should_converge_when_the_pool_collapses_below_k() {
        // every candidate is identical, so after the first pick none clears the
        // min distance: fewer than k survive, convergence is signalled.
        let candidates = vec![
            candidate(&[0, 0, 1, 1], 1.0),
            candidate(&[0, 0, 1, 1], 1.0),
            candidate(&[0, 0, 1, 1], 1.0),
        ];
        let solver = ScriptedSolver {
            base: 0,
            candidates,
        };
        let cfg = ModeConfig {
            k: 3,
            base_seed: 0,
            score_tolerance: 1.0,
            min_distance: 0.5,
            pool_per_candidate: 10,
        };

        let result = diversify(&solver, &cfg);

        assert_eq!(result.candidates.len(), 1);
        assert!(result.solution_space_converged);
    }

    #[test]
    fn should_return_best_first_regardless_of_seed_order() {
        // the best score is produced by a later seed; the result must still lead
        // with it, proving the reduction is score-ordered, not seed-ordered.
        let candidates = vec![candidate(&[0, 1, 0, 1], 5.0), candidate(&[0, 0, 1, 1], 1.0)];
        let solver = ScriptedSolver {
            base: 0,
            candidates,
        };
        let cfg = ModeConfig {
            k: 2,
            base_seed: 0,
            score_tolerance: 10.0,
            min_distance: 0.0,
            pool_per_candidate: 10,
        };

        let result = diversify(&solver, &cfg);

        assert_eq!(result.candidates.first().map(|c| c.score), Some(1.0));
    }

    #[test]
    fn should_sort_selected_candidates_by_score_after_diversity_selection() {
        // greedy VI selection picks best (1.0) then the most distant, which is
        // the *worst*-scoring candidate (3.0), then the middle one (2.0). The
        // returned list must nevertheless ascend by score: 1.0, 2.0, 3.0.
        let candidates = vec![
            candidate(&[0, 0, 1, 1], 1.0),
            candidate(&[0, 0, 1, 2], 2.0),
            candidate(&[0, 1, 0, 1], 3.0),
        ];
        let solver = ScriptedSolver {
            base: 0,
            candidates,
        };
        let cfg = ModeConfig {
            k: 3,
            base_seed: 0,
            score_tolerance: 10.0,
            min_distance: 0.1,
            pool_per_candidate: 10,
        };

        let result = diversify(&solver, &cfg);

        assert_eq!(result.candidates.len(), 3);
        assert_eq!(result.candidates.first().map(|c| c.score), Some(1.0));
        assert_eq!(result.candidates.get(1).map(|c| c.score), Some(2.0));
        assert_eq!(result.candidates.get(2).map(|c| c.score), Some(3.0));
    }

    #[test]
    fn should_return_nothing_for_zero_k() {
        let solver = ScriptedSolver {
            base: 0,
            candidates: vec![candidate(&[0], 1.0)],
        };
        let cfg = ModeConfig {
            k: 0,
            base_seed: 0,
            score_tolerance: 0.0,
            min_distance: 0.0,
            pool_per_candidate: 10,
        };

        let result = diversify(&solver, &cfg);

        assert!(result.candidates.is_empty());
        // zero requested, zero returned: exactly satisfied, not a convergence.
        assert!(!result.solution_space_converged);
    }
}
