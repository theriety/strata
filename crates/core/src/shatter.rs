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

use std::collections::HashMap;
use std::time::{Duration, Instant};

use highs::{HighsModelStatus, RowProblem, Sense};
use rayon::prelude::*;

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

/// Solves the exact MFAS via an integer program with lazily added cycle
/// constraints. Returns `None` when the time budget is exhausted before the
/// residual graph becomes acyclic, signalling the caller to fall back.
fn solve_exact(scc: &SccView, weights: &EdgeWeights, timeout: Duration) -> Option<BreakSet> {
    // an edgeless component is provably acyclic with an empty cut; short-circuit
    // before invoking the solver, which rejects a variable-free model.
    if scc.edges.is_empty() {
        return Some(BreakSet {
            edges: Vec::new(),
            exact: true,
        });
    }

    let started = Instant::now();
    // each accumulated constraint is the set of edge indices forming one cycle;
    // at least one of them must be cut.
    let mut cycle_constraints: Vec<Vec<usize>> = Vec::new();

    loop {
        if started.elapsed() >= timeout {
            return None;
        }

        let cut_mask = solve_ilp(scc, weights, &cycle_constraints, timeout, started)?;

        let kept: Vec<EdgeRef> = scc
            .edges
            .iter()
            .zip(cut_mask.iter())
            .filter(|&(_, &cut)| !cut)
            .map(|(&edge, _)| edge)
            .collect();

        if let Some(cycle) = find_cycle(scc.vertex_count, &kept) {
            cycle_constraints.push(cycle_edge_indices(scc, &cycle));
        } else {
            let edges = scc
                .edges
                .iter()
                .zip(cut_mask.iter())
                .filter(|&(_, &cut)| cut)
                .map(|(&edge, _)| edge)
                .collect();
            return Some(BreakSet { edges, exact: true });
        }
    }
}

/// Builds and solves the cut-minimising ILP under the current cycle constraints.
///
/// One binary variable per edge marks whether it is cut; each constraint forces
/// at least one edge of a known residual cycle to be cut. Returns the per-edge
/// cut mask, or `None` when the solver does not reach an optimal status within
/// the remaining budget.
fn solve_ilp(
    scc: &SccView,
    weights: &EdgeWeights,
    cycle_constraints: &[Vec<usize>],
    timeout: Duration,
    started: Instant,
) -> Option<Vec<bool>> {
    let mut problem = RowProblem::default();
    let cut_vars: Vec<highs::Col> = scc
        .edges
        .iter()
        .map(|&edge| problem.add_integer_column(weights.weight(edge), 0..=1))
        .collect();

    for cycle in cycle_constraints {
        let factors: Vec<(highs::Col, f64)> = cycle
            .iter()
            .filter_map(|&index| cut_vars.get(index).map(|&col| (col, 1.0)))
            .collect();
        // at least one edge of the cycle must be cut: sum >= 1.
        problem.add_row(1.0.., &factors);
    }

    let mut model = problem.optimise(Sense::Minimise);
    model.make_quiet();
    let remaining = timeout.saturating_sub(started.elapsed());
    model.set_option("time_limit", remaining.as_secs_f64());

    let solved = model.solve();
    if solved.status() != HighsModelStatus::Optimal {
        return None;
    }

    let columns = solved.get_solution();
    let mask = columns.columns().iter().map(|&value| value > 0.5).collect();
    Some(mask)
}

/// Maps a cycle expressed as a vertex sequence to the indices of its edges
/// within `scc.edges`, so the resulting constraint references solver columns.
fn cycle_edge_indices(scc: &SccView, cycle: &[u32]) -> Vec<usize> {
    let mut index_of: HashMap<EdgeRef, usize> = HashMap::with_capacity(scc.edges.len());
    for (index, &edge) in scc.edges.iter().enumerate() {
        index_of.insert(edge, index);
    }

    let mut indices = Vec::with_capacity(cycle.len());
    for window in cycle.windows(2) {
        let (Some(&source), Some(&target)) = (window.first(), window.last()) else {
            continue;
        };
        if let Some(&index) = index_of.get(&EdgeRef { source, target }) {
            indices.push(index);
        }
    }
    indices
}

/// The Eades–Lin–Smyth heuristic with single-vertex local improvement.
///
/// A linear ordering is built by repeatedly stripping sinks (appended on the
/// right) and sources (prepended on the left); when neither exists, the vertex
/// maximising out-degree minus in-degree is removed next. The cut is the set of
/// backward edges in the final ordering, which is then improved by repositioning
/// single vertices while the total cut weight strictly drops.
fn solve_heuristic(scc: &SccView, weights: &EdgeWeights) -> BreakSet {
    let order = eades_lin_smyth(scc);
    let order = improve(scc, weights, order);
    let mut edges = backward_edges(scc, &order);
    edges.sort_unstable();
    BreakSet {
        edges,
        exact: false,
    }
}

/// Computes an Eades–Lin–Smyth vertex ordering of the SCC in `O(V + E)`.
fn eades_lin_smyth(scc: &SccView) -> Vec<u32> {
    let vertex_count = scc.vertex_count as usize;
    let mut out_degree = vec![0_i64; vertex_count];
    let mut in_degree = vec![0_i64; vertex_count];
    let mut alive = vec![true; vertex_count];
    for edge in &scc.edges {
        if let Some(slot) = out_degree.get_mut(edge.source as usize) {
            *slot += 1;
        }
        if let Some(slot) = in_degree.get_mut(edge.target as usize) {
            *slot += 1;
        }
    }

    let forward = adjacency(scc, Orientation::Outgoing);
    let backward = adjacency(scc, Orientation::Incoming);

    let mut left: Vec<u32> = Vec::new();
    let mut right: Vec<u32> = Vec::new();
    let mut remaining = vertex_count;

    while remaining > 0 {
        // strip sinks (no live out-edges) to the right.
        if let Some(vertex) = pick(&alive, &out_degree, Degree::Zero) {
            right.push(vertex);
            remove(
                vertex,
                &mut alive,
                &forward,
                &backward,
                &mut out_degree,
                &mut in_degree,
            );
            remaining -= 1;
            continue;
        }
        // strip sources (no live in-edges) to the left.
        if let Some(vertex) = pick(&alive, &in_degree, Degree::Zero) {
            left.push(vertex);
            remove(
                vertex,
                &mut alive,
                &forward,
                &backward,
                &mut out_degree,
                &mut in_degree,
            );
            remaining -= 1;
            continue;
        }
        // otherwise take the vertex of maximum out-degree minus in-degree,
        // breaking ties by smallest index for determinism.
        if let Some(vertex) = pick_max_delta(&alive, &out_degree, &in_degree) {
            left.push(vertex);
            remove(
                vertex,
                &mut alive,
                &forward,
                &backward,
                &mut out_degree,
                &mut in_degree,
            );
            remaining -= 1;
        } else {
            break;
        }
    }

    right.reverse();
    left.extend(right);
    left
}

/// Which incident edges an adjacency list records.
#[derive(Clone, Copy)]
enum Orientation {
    /// Successors: `vertex -> neighbour`.
    Outgoing,
    /// Predecessors: `neighbour -> vertex`.
    Incoming,
}

/// Builds a per-vertex neighbour list under the given orientation.
fn adjacency(scc: &SccView, orientation: Orientation) -> Vec<Vec<u32>> {
    let mut lists = vec![Vec::new(); scc.vertex_count as usize];
    for edge in &scc.edges {
        let (owner, neighbour) = match orientation {
            Orientation::Outgoing => (edge.source, edge.target),
            Orientation::Incoming => (edge.target, edge.source),
        };
        if let Some(list) = lists.get_mut(owner as usize) {
            list.push(neighbour);
        }
    }
    lists
}

/// A degree predicate used when scanning for the next removable vertex.
#[derive(Clone, Copy)]
enum Degree {
    /// Matches a vertex whose tracked degree has fallen to zero.
    Zero,
}

/// Returns the lowest-indexed live vertex whose `degree` entry satisfies
/// `predicate`, or `None`.
fn pick(alive: &[bool], degree: &[i64], predicate: Degree) -> Option<u32> {
    alive.iter().enumerate().find_map(|(index, &is_alive)| {
        let value = degree.get(index).copied().unwrap_or(0);
        let matches = match predicate {
            Degree::Zero => value == 0,
        };
        if is_alive && matches {
            u32::try_from(index).ok()
        } else {
            None
        }
    })
}

/// Returns the live vertex maximising `out_degree - in_degree`, breaking ties by
/// smallest index, or `None` when no vertex is live.
fn pick_max_delta(alive: &[bool], out_degree: &[i64], in_degree: &[i64]) -> Option<u32> {
    let mut best: Option<(i64, u32)> = None;
    for (index, &is_alive) in alive.iter().enumerate() {
        if !is_alive {
            continue;
        }
        let Ok(vertex) = u32::try_from(index) else {
            continue;
        };
        let delta = out_degree.get(index).copied().unwrap_or(0)
            - in_degree.get(index).copied().unwrap_or(0);
        if best.is_none_or(|(best_delta, _)| delta > best_delta) {
            best = Some((delta, vertex));
        }
    }
    best.map(|(_, vertex)| vertex)
}

/// Removes `vertex` from the live set and decrements the live degree of every
/// neighbour that still references it.
fn remove(
    vertex: u32,
    alive: &mut [bool],
    forward: &[Vec<u32>],
    backward: &[Vec<u32>],
    out_degree: &mut [i64],
    in_degree: &mut [i64],
) {
    if let Some(slot) = alive.get_mut(vertex as usize) {
        *slot = false;
    }
    if let Some(successors) = forward.get(vertex as usize) {
        for &successor in successors {
            if alive.get(successor as usize).copied().unwrap_or(false)
                && let Some(slot) = in_degree.get_mut(successor as usize)
            {
                *slot -= 1;
            }
        }
    }
    if let Some(predecessors) = backward.get(vertex as usize) {
        for &predecessor in predecessors {
            if alive.get(predecessor as usize).copied().unwrap_or(false)
                && let Some(slot) = out_degree.get_mut(predecessor as usize)
            {
                *slot -= 1;
            }
        }
    }
}

/// Improves `order` by repositioning single vertices, accepting any move that
/// strictly lowers the total backward-edge weight; repeats until no move helps.
fn improve(scc: &SccView, weights: &EdgeWeights, mut order: Vec<u32>) -> Vec<u32> {
    let mut current = backward_weight(scc, weights, &order);
    loop {
        let mut improved = false;
        for from in 0..order.len() {
            for to in 0..order.len() {
                if from == to {
                    continue;
                }
                let candidate = reposition(&order, from, to);
                let weight = backward_weight(scc, weights, &candidate);
                if weight + f64::EPSILON < current {
                    order = candidate;
                    current = weight;
                    improved = true;
                }
            }
        }
        if !improved {
            return order;
        }
    }
}

/// Returns a copy of `order` with the vertex at index `from` removed and
/// reinserted at index `to`.
fn reposition(order: &[u32], from: usize, to: usize) -> Vec<u32> {
    let mut next: Vec<u32> = order.to_vec();
    if from >= next.len() {
        return next;
    }
    let vertex = next.remove(from);
    let insert_at = to.min(next.len());
    next.insert(insert_at, vertex);
    next
}

/// Sums the cut weight of the backward edges induced by `order`.
fn backward_weight(scc: &SccView, weights: &EdgeWeights, order: &[u32]) -> f64 {
    backward_edges(scc, order)
        .into_iter()
        .map(|edge| weights.weight(edge))
        .sum()
}

/// Returns the edges that point backward in `order`, i.e. whose target precedes
/// their source; cutting exactly these makes the ordering a valid topological
/// order of the residual graph.
fn backward_edges(scc: &SccView, order: &[u32]) -> Vec<EdgeRef> {
    let mut position = vec![0_usize; scc.vertex_count as usize];
    for (rank, &vertex) in order.iter().enumerate() {
        if let Some(slot) = position.get_mut(vertex as usize) {
            *slot = rank;
        }
    }

    scc.edges
        .iter()
        .filter(|edge| {
            let source = position.get(edge.source as usize).copied().unwrap_or(0);
            let target = position.get(edge.target as usize).copied().unwrap_or(0);
            target < source
        })
        .copied()
        .collect()
}

/// Finds a directed cycle in the graph on `vertex_count` vertices with `edges`,
/// returning it as a vertex sequence whose first and last entries coincide, or
/// `None` when the graph is acyclic. Plain iterative DFS with a recursion stack.
fn find_cycle(vertex_count: u32, edges: &[EdgeRef]) -> Option<Vec<u32>> {
    let count = vertex_count as usize;
    let mut successors = vec![Vec::new(); count];
    for edge in edges {
        if let Some(list) = successors.get_mut(edge.source as usize) {
            list.push(edge.target);
        }
    }

    // 0 = unvisited, 1 = on the active stack, 2 = fully explored.
    let mut color = vec![0_u8; count];

    for root in 0..count {
        if color.get(root).copied().unwrap_or(2) != 0 {
            continue;
        }
        if let Some(cycle) = dfs_cycle(root, &successors, &mut color) {
            return Some(cycle);
        }
    }
    None
}

/// One iterative DFS from `root`, detecting a back edge to a vertex still on the
/// active path and reconstructing the cycle from the path stack.
fn dfs_cycle(root: usize, successors: &[Vec<u32>], color: &mut [u8]) -> Option<Vec<u32>> {
    let mut path: Vec<usize> = Vec::new();
    let mut cursor: Vec<usize> = Vec::new();
    path.push(root);
    cursor.push(0);
    if let Some(slot) = color.get_mut(root) {
        *slot = 1;
    }

    while let Some(&vertex) = path.last() {
        let index = cursor.len().saturating_sub(1);
        let next = cursor
            .get(index)
            .copied()
            .and_then(|position| successors.get(vertex).and_then(|list| list.get(position)))
            .copied();

        let Some(target) = next else {
            // exhausted `vertex`: mark it explored and backtrack.
            if let Some(slot) = color.get_mut(vertex) {
                *slot = 2;
            }
            path.pop();
            cursor.pop();
            continue;
        };

        if let Some(slot) = cursor.get_mut(index) {
            *slot += 1;
        }
        let target_index = target as usize;
        match color.get(target_index).copied().unwrap_or(2) {
            0 => {
                path.push(target_index);
                cursor.push(0);
                if let Some(slot) = color.get_mut(target_index) {
                    *slot = 1;
                }
            }
            1 => return Some(reconstruct_cycle(&path, target_index)),
            _ => {}
        }
    }
    None
}

/// Reconstructs a cycle from the active `path` once a back edge to `target` is
/// found: the slice from `target`'s first occurrence to the end, closed by
/// repeating `target`.
fn reconstruct_cycle(path: &[usize], target: usize) -> Vec<u32> {
    let start = path
        .iter()
        .position(|&vertex| vertex == target)
        .unwrap_or(0);
    let mut cycle: Vec<u32> = path
        .iter()
        .skip(start)
        .filter_map(|&vertex| u32::try_from(vertex).ok())
        .collect();
    if let Some(&first) = cycle.first() {
        cycle.push(first);
    }
    cycle
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

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
