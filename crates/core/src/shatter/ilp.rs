//! Exact minimum feedback arc set solving through a lazily constrained ILP.
//!
//! Minimises total cut weight with cycle constraints added only as residual
//! cycles are found, falling back to the caller when the time budget runs out.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use highs::{HighsModelStatus, RowProblem, Sense};

use super::cycle::find_cycle;
use super::{BreakSet, EdgeRef, EdgeWeights, SccView};

/// Solves the exact MFAS via an integer program with lazily added cycle
/// constraints. Returns `None` when the time budget is exhausted before the
/// residual graph becomes acyclic, signalling the caller to fall back.
pub(super) fn solve_exact(
    scc: &SccView,
    weights: &EdgeWeights,
    timeout: Duration,
) -> Option<BreakSet> {
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
    // Single-threaded solve: HiGHS multi-threading can pick different optimal
    // vertices run to run, and NFR-1 demands byte-identical output.
    model.set_option("threads", 1);
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
