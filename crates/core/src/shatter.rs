//! Cycle shattering — minimum feedback arc set (MFAS) suggestions per SCC.
//!
//! For every non-trivial strongly connected component the solver suggests the
//! cheapest set of edges whose removal makes that component acyclic — the most
//! actionable output the tool produces. Components at or under the ILP threshold
//! are solved exactly: an integer program minimises total cut weight, with cycle
//! constraints added lazily (start unconstrained, solve, find a residual cycle,
//! forbid it, repeat) so the exponentially many cycles are never enumerated up
//! front. Larger components, or components whose exact solve exhausts the time
//! budget, fall back to the Eades–Lin–Smyth ordering heuristic with single-vertex
//! local improvement.
//!
//! Every [`BreakSet`] carries an `exact` flag so a heuristic answer can never be
//! mistaken for a proof (AD-7). Per-SCC work runs in parallel via rayon, and the
//! results are reduced in SCC-index order so the output is byte-identical across
//! runs and thread counts.

mod cycle;
mod heuristic;
mod ilp;

use std::collections::HashMap;
use std::time::Duration;

use rayon::prelude::*;

use self::heuristic::solve_heuristic;
use self::ilp::solve_exact;

/// A directed edge identified by its endpoint vertices, in the SCC-local vertex
/// numbering of the [`SccView`] it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EdgeRef {
    /// The local index of the depending vertex.
    pub source: u32,
    /// The local index of the depended-upon vertex.
    pub target: u32,
}

/// A non-trivial strongly connected component, expressed in its own dense local
/// vertex numbering (`0..vertex_count`).
///
/// The solver phase before this one (condensation) yields each SCC's member
/// list; the caller renumbers members densely and records every internal hard
/// edge here. Edges are deduplicated and self-loops excluded by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SccView {
    /// The number of vertices, numbered `0..vertex_count`.
    pub vertex_count: u32,
    /// The internal directed edges of the component.
    pub edges: Vec<EdgeRef>,
}

impl SccView {
    /// Builds a view from a vertex count and an edge list, dropping self-loops
    /// and duplicate edges so the residual-graph reasoning stays simple.
    #[must_use]
    pub fn new(vertex_count: u32, edges: impl IntoIterator<Item = EdgeRef>) -> Self {
        let mut deduplicated: Vec<EdgeRef> = edges
            .into_iter()
            .filter(|edge| edge.source != edge.target)
            .collect();
        deduplicated.sort_unstable();
        deduplicated.dedup();
        Self {
            vertex_count,
            edges: deduplicated,
        }
    }
}

/// Per-edge cut weights consulted by the objective: removing an edge costs its
/// weight, so the solver minimises the total weight of the suggested break set.
///
/// A missing edge defaults to unit weight, which keeps the heuristic and exact
/// paths well defined even when the caller supplies a partial table.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EdgeWeights {
    /// The weight charged for cutting each edge.
    weights: HashMap<EdgeRef, f64>,
}

impl EdgeWeights {
    /// Builds a weight table from `(edge, weight)` pairs.
    #[must_use]
    pub fn from_pairs(pairs: impl IntoIterator<Item = (EdgeRef, f64)>) -> Self {
        Self {
            weights: pairs.into_iter().collect(),
        }
    }

    /// Returns the cut weight of `edge`, defaulting to `1.0` when absent.
    #[must_use]
    pub fn weight(&self, edge: EdgeRef) -> f64 {
        self.weights.get(&edge).copied().unwrap_or(1.0)
    }
}

/// A set of edges whose removal makes its SCC acyclic.
///
/// `exact` is `true` only when the ILP proved minimality within budget; a
/// heuristic answer always reports `false`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakSet {
    /// The edges to cut, in ascending `(source, target)` order.
    pub edges: Vec<EdgeRef>,
    /// Whether the cut is a proven minimum (`true`) or a heuristic (`false`).
    pub exact: bool,
}

/// Budgets governing the exact-versus-heuristic dispatch per SCC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SolverLimits {
    /// SCCs at or under this vertex count attempt the exact ILP (default 300).
    pub ilp_threshold: usize,
    /// Wall-clock budget for one SCC's exact solve; an overrun degrades to the
    /// heuristic with `exact: false`.
    pub timeout: Duration,
}

impl Default for SolverLimits {
    /// The shipped defaults: ILP up to 300 vertices, with a 30-second budget.
    fn default() -> Self {
        Self {
            ilp_threshold: 300,
            timeout: Duration::from_secs(30),
        }
    }
}

/// Shatters every SCC in `sccs`, returning one [`BreakSet`] per SCC in input
/// order.
///
/// Work is distributed across SCCs by rayon; because each SCC is solved
/// independently and the results are collected back in index order, the output
/// does not depend on the thread schedule.
#[must_use]
pub fn shatter_all(
    sccs: &[SccView],
    weights: &EdgeWeights,
    limits: &SolverLimits,
) -> Vec<BreakSet> {
    sccs.par_iter()
        .map(|scc| shatter(scc, weights, limits))
        .collect()
}

/// Shatters a single SCC, choosing the exact ILP when the component is at or
/// under the threshold and falling back to the heuristic otherwise or on
/// timeout.
#[must_use]
pub fn shatter(scc: &SccView, weights: &EdgeWeights, limits: &SolverLimits) -> BreakSet {
    if (scc.vertex_count as usize) <= limits.ilp_threshold
        && let Some(exact) = solve_exact(scc, weights, limits.timeout)
    {
        return exact;
    }

    solve_heuristic(scc, weights)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::cycle::find_cycle;
    use super::*;

    fn edge(source: u32, target: u32) -> EdgeRef {
        EdgeRef { source, target }
    }

    /// A heuristic-only limit, forcing the ELS path regardless of SCC size.
    fn heuristic_limits() -> SolverLimits {
        SolverLimits {
            ilp_threshold: 0,
            timeout: Duration::from_secs(1),
        }
    }

    /// An exact-only limit with a generous budget for small property graphs.
    fn exact_limits() -> SolverLimits {
        SolverLimits {
            ilp_threshold: 64,
            timeout: Duration::from_secs(5),
        }
    }

    /// Turns a proptest-generated edge list into an `SccView`, clamping endpoints
    /// to the vertex range.
    fn view(vertex_count: u32, raw: Vec<(u32, u32)>) -> SccView {
        let edges = raw
            .into_iter()
            .filter(|&(source, target)| source < vertex_count && target < vertex_count)
            .map(|(source, target)| edge(source, target));
        SccView::new(vertex_count, edges)
    }

    /// Builds the residual graph after cutting `cut` from `scc` and asserts it
    /// is acyclic.
    fn assert_acyclic(scc: &SccView, cut: &[EdgeRef]) {
        let kept: Vec<EdgeRef> = scc
            .edges
            .iter()
            .filter(|edge| !cut.contains(edge))
            .copied()
            .collect();
        assert!(
            find_cycle(scc.vertex_count, &kept).is_none(),
            "residual graph still contains a cycle after the cut"
        );
    }

    /// Brute-forces the minimum cut weight over every edge subset; only viable
    /// for tiny graphs (used to cross-check the exact solver).
    fn brute_force_min_weight(scc: &SccView, weights: &EdgeWeights) -> f64 {
        let edge_count = scc.edges.len();
        let mut best = f64::INFINITY;
        for mask in 0..(1_u32 << edge_count) {
            let cut: Vec<EdgeRef> = scc
                .edges
                .iter()
                .enumerate()
                .filter(|(index, _)| mask & (1 << index) != 0)
                .map(|(_, &edge)| edge)
                .collect();
            let kept: Vec<EdgeRef> = scc
                .edges
                .iter()
                .filter(|edge| !cut.contains(edge))
                .copied()
                .collect();
            if find_cycle(scc.vertex_count, &kept).is_none() {
                let weight: f64 = cut.iter().map(|&edge| weights.weight(edge)).sum();
                best = best.min(weight);
            }
        }
        best
    }

    #[test]
    fn should_default_an_absent_edge_weight_to_one() {
        let weights = EdgeWeights::default();

        assert!((weights.weight(edge(0, 1)) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn should_drop_self_loops_and_duplicates_when_building_a_view() {
        let scc = SccView::new(2, [edge(0, 1), edge(0, 1), edge(0, 0)]);

        assert_eq!(scc.edges, vec![edge(0, 1)]);
    }

    #[test]
    fn should_leave_an_acyclic_component_uncut() {
        let scc = SccView::new(3, [edge(0, 1), edge(1, 2)]);

        let result = shatter(&scc, &EdgeWeights::default(), &SolverLimits::default());

        assert!(result.edges.is_empty());
        assert!(result.exact);
    }

    #[test]
    fn should_break_a_two_cycle_exactly() {
        let scc = SccView::new(2, [edge(0, 1), edge(1, 0)]);

        let result = shatter(&scc, &EdgeWeights::default(), &SolverLimits::default());

        assert_eq!(result.edges.len(), 1);
        assert!(result.exact);
        assert_acyclic(&scc, &result.edges);
    }

    #[test]
    fn should_cut_the_cheapest_edge_of_a_cycle() {
        let scc = SccView::new(3, [edge(0, 1), edge(1, 2), edge(2, 0)]);
        let weights = EdgeWeights::from_pairs([(edge(1, 2), 0.25)]);

        let result = shatter(&scc, &weights, &SolverLimits::default());

        assert_eq!(result.edges, vec![edge(1, 2)]);
        assert!(result.exact);
    }

    #[test]
    fn should_match_brute_force_on_an_interlocking_knot() {
        // two interlocking triangles sharing the 0->1 edge.
        let scc = SccView::new(
            4,
            [
                edge(0, 1),
                edge(1, 2),
                edge(2, 0),
                edge(1, 3),
                edge(3, 0),
                edge(0, 2),
            ],
        );
        let weights = EdgeWeights::default();

        let result = shatter(&scc, &weights, &SolverLimits::default());
        let cut_weight: f64 = result.edges.iter().map(|&e| weights.weight(e)).sum();

        assert!(result.exact);
        assert!((cut_weight - brute_force_min_weight(&scc, &weights)).abs() < 1e-6);
        assert_acyclic(&scc, &result.edges);
    }

    #[test]
    fn should_flag_heuristic_results_as_inexact() {
        let scc = SccView::new(3, [edge(0, 1), edge(1, 2), edge(2, 0)]);
        let limits = SolverLimits {
            ilp_threshold: 0,
            timeout: Duration::from_secs(1),
        };

        let result = shatter(&scc, &EdgeWeights::default(), &limits);

        assert!(!result.exact);
        assert_acyclic(&scc, &result.edges);
    }

    #[test]
    fn should_break_every_cycle_with_the_heuristic() {
        // a tournament-like knot the heuristic must still fully break.
        let scc = SccView::new(
            4,
            [
                edge(0, 1),
                edge(1, 2),
                edge(2, 3),
                edge(3, 0),
                edge(2, 0),
                edge(3, 1),
            ],
        );
        let limits = SolverLimits {
            ilp_threshold: 0,
            timeout: Duration::from_secs(1),
        };

        let result = shatter(&scc, &EdgeWeights::default(), &limits);

        assert!(!result.exact);
        assert_acyclic(&scc, &result.edges);
    }

    #[test]
    fn should_reduce_sccs_in_input_order() {
        let sccs = vec![
            SccView::new(2, [edge(0, 1), edge(1, 0)]),
            SccView::new(3, [edge(0, 1), edge(1, 2)]),
        ];

        let results = shatter_all(&sccs, &EdgeWeights::default(), &SolverLimits::default());

        assert_eq!(results.len(), 2);
        assert_eq!(results.first().map(|b| b.edges.len()), Some(1));
        assert_eq!(results.get(1).map(|b| b.edges.len()), Some(0));
    }

    #[test]
    fn should_produce_identical_output_across_repeated_runs() {
        let scc = SccView::new(
            4,
            [
                edge(0, 1),
                edge(1, 2),
                edge(2, 3),
                edge(3, 0),
                edge(2, 0),
                edge(3, 1),
            ],
        );
        let weights = EdgeWeights::default();
        let limits = SolverLimits::default();

        let first = shatter(&scc, &weights, &limits);
        let second = shatter(&scc, &weights, &limits);

        assert_eq!(first, second);
    }

    proptest! {
        /// On every small graph, the exact ILP's cut weight must equal the
        /// brute-forced minimum feedback arc set weight, and the result must
        /// claim exactness.
        #[test]
        fn should_match_brute_force_on_small_graphs(
            vertex_count in 1_u32..6,
            raw in proptest::collection::vec((0_u32..6, 0_u32..6), 0..10),
        ) {
            let scc = view(vertex_count, raw);
            let weights = EdgeWeights::default();

            let result = shatter(&scc, &weights, &exact_limits());
            let cut_weight: f64 = result.edges.iter().map(|&e| weights.weight(e)).sum();

            prop_assert!(result.exact);
            prop_assert!(
                (cut_weight - brute_force_min_weight(&scc, &weights)).abs() < 1e-6
            );
        }

        /// Both solver paths must leave an acyclic residual graph: cutting the
        /// returned edges breaks every cycle.
        #[test]
        fn should_always_break_every_cycle(
            vertex_count in 1_u32..7,
            raw in proptest::collection::vec((0_u32..7, 0_u32..7), 0..14),
        ) {
            let scc = view(vertex_count, raw);

            let exact = shatter(&scc, &EdgeWeights::default(), &exact_limits());
            let heuristic = shatter(&scc, &EdgeWeights::default(), &heuristic_limits());

            assert_acyclic(&scc, &exact.edges);
            assert_acyclic(&scc, &heuristic.edges);
            prop_assert!(!heuristic.exact);
        }

        /// Repeated runs of either path produce byte-identical break sets,
        /// regardless of rayon's thread schedule.
        #[test]
        fn should_be_deterministic_across_runs(
            vertex_count in 1_u32..7,
            raw in proptest::collection::vec((0_u32..7, 0_u32..7), 0..14),
        ) {
            let sccs = vec![view(vertex_count, raw)];

            let first = shatter_all(&sccs, &EdgeWeights::default(), &exact_limits());
            let second = shatter_all(&sccs, &EdgeWeights::default(), &exact_limits());

            prop_assert_eq!(first, second);
        }
    }
}
