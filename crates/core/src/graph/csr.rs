//! Compressed sparse row (CSR) adjacency in a struct-of-arrays layout (ad-6).
//!
//! CSR stores the whole graph in three flat arrays — row offsets, edge targets,
//! and per-edge weights — giving cache-friendly sequential scans of any node's
//! neighbours. [`build_csr`] derives both the forward and the reverse view in a
//! single pass over snapshot edges, filtered by hardness, and sorts each row so
//! the layout is identical across runs and thread counts.

use strata_ir::{Edge, Hardness, NodeId, Snapshot};

use crate::score::KindWeights;

/// Which edges enter a CSR view.
///
/// The solver's acyclicity phases (condensation, layering) operate on hard
/// edges only; scoring phases see every edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HardnessFilter {
    /// Keep only [`Hardness::Hard`] edges.
    HardOnly,
    /// Keep every edge regardless of hardness.
    All,
}

impl HardnessFilter {
    /// Returns `true` if `edge` passes this filter.
    fn admits(self, edge: &Edge) -> bool {
        match self {
            Self::HardOnly => edge.hardness == Hardness::Hard,
            Self::All => true,
        }
    }
}

/// The weight assigned to a single edge: kind weight scaled by binder confidence.
fn edge_weight(edge: &Edge, weights: &KindWeights) -> f32 {
    // reason: edge weights are f32 by the CSR contract (ad-6); narrowing the f64 product is intentional and the only lossy step
    #[allow(clippy::cast_possible_truncation)]
    let weight = weights.edge_weight(edge.kind, edge.confidence) as f32;
    weight
}

/// Compressed sparse row adjacency in a struct-of-arrays layout (ad-6).
///
/// Row `i` spans `targets[offsets[i]..offsets[i + 1]]`, with `weights` aligned
/// to `targets`. `offsets` has length `vertex_count + 1`; targets within a row
/// are sorted ascending for determinism.
#[derive(Debug, Clone, PartialEq)]
pub struct Csr {
    /// Row offsets, length `vertex_count + 1`.
    offsets: Vec<u32>,
    /// Edge targets, sorted ascending within each row.
    targets: Vec<u32>,
    /// Per-edge weight (kind weight × binder confidence), aligned to `targets`.
    weights: Vec<f32>,
}

impl Csr {
    /// Builds a CSR from `edges` that are already sorted ascending by
    /// `(row, column)` and free of duplicates, over `vertex_count` vertices.
    ///
    /// Every weight is `0.0`: this constructor serves structural views such as
    /// the SCC quotient DAG, where scores are not carried on the edges.
    #[must_use]
    pub fn from_sorted_edges(vertex_count: usize, edges: &[(u32, u32)]) -> Self {
        let mut offsets = vec![0_u32; vertex_count + 1];
        for &(row, _) in edges {
            if let Some(slot) = offsets.get_mut(row as usize + 1) {
                *slot += 1;
            }
        }
        for index in 1..offsets.len() {
            let previous = offsets.get(index - 1).copied().unwrap_or(0);
            if let Some(slot) = offsets.get_mut(index) {
                *slot += previous;
            }
        }

        let targets: Vec<u32> = edges.iter().map(|&(_, column)| column).collect();
        let weights = vec![0.0_f32; targets.len()];

        Self {
            offsets,
            targets,
            weights,
        }
    }

    /// Builds a CSR from weighted `edges` over `vertex_count` vertices.
    ///
    /// Edges need not be sorted or unique: rows are sorted ascending and the
    /// weights of parallel edges are summed, so the result is deterministic for
    /// any input order. This constructor serves quotient views that must carry
    /// real cut weights (heavy-edge matching and FM gains read them).
    #[must_use]
    pub fn from_weighted_edges(vertex_count: usize, edges: &[(u32, u32, f32)]) -> Self {
        let mut sorted: Vec<(u32, u32, f32)> = edges.to_vec();
        sorted.sort_by_key(|edge| (edge.0, edge.1));

        // Merge parallel edges by summing their weights.
        let mut merged: Vec<(u32, u32, f32)> = Vec::with_capacity(sorted.len());
        for (row, column, weight) in sorted {
            match merged.last_mut() {
                Some(last) if last.0 == row && last.1 == column => last.2 += weight,
                _ => merged.push((row, column, weight)),
            }
        }

        let mut offsets = vec![0_u32; vertex_count + 1];
        for &(row, _, _) in &merged {
            if let Some(slot) = offsets.get_mut(row as usize + 1) {
                *slot += 1;
            }
        }
        for index in 1..offsets.len() {
            let previous = offsets.get(index - 1).copied().unwrap_or(0);
            if let Some(slot) = offsets.get_mut(index) {
                *slot += previous;
            }
        }

        let targets: Vec<u32> = merged.iter().map(|&(_, column, _)| column).collect();
        let weights: Vec<f32> = merged.iter().map(|&(_, _, weight)| weight).collect();

        Self {
            offsets,
            targets,
            weights,
        }
    }

    /// Returns the graph with every edge reversed, weights preserved.
    ///
    /// Self-loops are kept. Rows of the result are sorted ascending, so the
    /// predecessors of a vertex appear in ascending source order.
    #[must_use]
    pub fn reversed(&self) -> Self {
        let mut edges: Vec<(u32, u32, f32)> = Vec::with_capacity(self.edge_count());
        for vertex in 0..self.vertex_count() {
            let from = u32::try_from(vertex).unwrap_or(u32::MAX);
            let weights = self.weights(from);
            for (slot, &to) in self.neighbors(from).iter().enumerate() {
                edges.push((to, from, weights.get(slot).copied().unwrap_or(0.0)));
            }
        }
        Self::from_weighted_edges(self.vertex_count(), &edges)
    }

    /// Returns the number of vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        // `offsets` always carries the trailing sentinel, so length is V + 1.
        self.offsets.len().saturating_sub(1)
    }

    /// Returns the total number of edges.
    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.targets.len()
    }

    /// Returns the sorted target slice for vertex `node`, or an empty slice if
    /// `node` is out of range.
    #[must_use]
    pub fn neighbors(&self, node: u32) -> &[u32] {
        let (start, end) = self.row_bounds(node);
        self.targets.get(start..end).unwrap_or(&[])
    }

    /// Returns the weight slice for vertex `node`, aligned to [`Self::neighbors`].
    #[must_use]
    pub fn weights(&self, node: u32) -> &[f32] {
        let (start, end) = self.row_bounds(node);
        self.weights.get(start..end).unwrap_or(&[])
    }

    /// Resolves the `[start, end)` slice bounds of `node`'s row, clamped to an
    /// empty range when `node` is out of bounds.
    fn row_bounds(&self, node: u32) -> (usize, usize) {
        let index = node as usize;
        let start = self.offsets.get(index).copied().unwrap_or(0);
        let end = self.offsets.get(index + 1).copied().unwrap_or(start);
        (start as usize, end as usize)
    }
}

/// Forward and reverse CSR views of the same filtered edge set.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphViews {
    /// Adjacency in dependency direction: `source -> target`.
    pub forward: Csr,
    /// Adjacency reversed: `target -> source`.
    pub reverse: Csr,
}

/// Builds the forward and reverse CSR views in one pass over the snapshot's
/// edges, keeping only those that pass `filter`.
///
/// Vertices are the snapshot's nodes, identified by their [`NodeId`] (the dense
/// `0..node_count` range guaranteed by snapshot assembly). Targets within every
/// row are sorted ascending so the layout is deterministic.
#[must_use]
pub fn build_csr(snapshot: &Snapshot, filter: HardnessFilter) -> GraphViews {
    build_csr_with(snapshot, filter, &KindWeights::default())
}

/// [`build_csr`] with an explicit per-kind weight table (the `[weights]` config)
/// pricing every edge instead of the built-in defaults.
#[must_use]
fn build_csr_with(
    snapshot: &Snapshot,
    filter: HardnessFilter,
    weights: &KindWeights,
) -> GraphViews {
    let ir = snapshot.ir();
    let vertex_count = ir.nodes.len();

    let forward = collect_directed(&ir.edges, vertex_count, filter, Direction::Forward, weights);
    let reverse = collect_directed(&ir.edges, vertex_count, filter, Direction::Reverse, weights);

    GraphViews { forward, reverse }
}

/// The orientation a CSR view records edges in.
#[derive(Clone, Copy)]
enum Direction {
    /// `source -> target`.
    Forward,
    /// `target -> source`.
    Reverse,
}

impl Direction {
    /// Returns the `(row, column)` endpoints for `edge` under this orientation.
    fn endpoints(self, edge: &Edge) -> (NodeId, NodeId) {
        match self {
            Self::Forward => (edge.source, edge.target),
            Self::Reverse => (edge.target, edge.source),
        }
    }
}

/// Builds one CSR view by counting per-row degrees, then placing sorted,
/// weight-aligned targets via a stable counting-sort pass.
fn collect_directed(
    edges: &[Edge],
    vertex_count: usize,
    filter: HardnessFilter,
    direction: Direction,
    kind_weights: &KindWeights,
) -> Csr {
    // Pass 1: per-row degree, accumulated into the offsets prefix sum.
    let mut offsets = vec![0_u32; vertex_count + 1];
    for edge in edges {
        if !filter.admits(edge) {
            continue;
        }
        let (row, _) = direction.endpoints(edge);
        if let Some(slot) = offsets.get_mut(row.0 as usize + 1) {
            *slot += 1;
        }
    }
    for index in 1..offsets.len() {
        let previous = offsets.get(index - 1).copied().unwrap_or(0);
        if let Some(slot) = offsets.get_mut(index) {
            *slot += previous;
        }
    }

    // Pass 2: place each edge at its row's running cursor. Iterating edges in
    // ascending (row, column) order keeps each row sorted without a re-sort,
    // since snapshot edges are canonically ordered by (source, target, ...).
    let edge_total = offsets.last().copied().unwrap_or(0) as usize;
    let mut targets = vec![0_u32; edge_total];
    let mut weights = vec![0.0_f32; edge_total];
    let mut cursor: Vec<u32> = offsets.iter().take(vertex_count).copied().collect();

    let mut ordered: Vec<(u32, u32, f32)> = edges
        .iter()
        .filter(|edge| filter.admits(edge))
        .map(|edge| {
            let (row, column) = direction.endpoints(edge);
            (row.0, column.0, edge_weight(edge, kind_weights))
        })
        .collect();
    ordered.sort_by_key(|&(row, column, _)| (row, column));

    for (row, column, weight) in ordered {
        let Some(slot) = cursor.get_mut(row as usize) else {
            continue;
        };
        let position = *slot as usize;
        *slot += 1;
        if let Some(target) = targets.get_mut(position) {
            *target = column;
        }
        if let Some(stored) = weights.get_mut(position) {
            *stored = weight;
        }
    }

    Csr {
        offsets,
        targets,
        weights,
    }
}

#[cfg(test)]
mod tests {
    use smol_str::SmolStr;
    use strata_ir::{
        Container, ContainerId, ContainerTree, EdgeKind, IntermediateRepresentation, Node,
        NodeKind, Polarity, ScopeLevel,
    };

    use super::*;

    fn node(id: u32) -> Node {
        Node {
            id: NodeId(id),
            name: SmolStr::new(format!("n{id}")),
            kind: NodeKind::Symbol,
            polarity: Polarity::Production,
            container: ContainerId(0),
            visibility: ScopeLevel::File,
            effective_size: 1,
            re_export: false,
        }
    }

    fn edge(source: u32, target: u32, kind: EdgeKind, hardness: Hardness) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind,
            hardness,
            confidence: 1.0,
        }
    }

    fn views(node_count: u32, edges: Vec<Edge>, filter: HardnessFilter) -> GraphViews {
        let nodes = (0..node_count).map(node).collect();
        let tree = ContainerTree::new(vec![Container {
            id: ContainerId(0),
            name: SmolStr::new("root"),
            level: ScopeLevel::File,
            parent: None,
            synthetic: false,
        }]);
        let ir = IntermediateRepresentation::new(nodes, edges, tree);

        let assembled = Snapshot::assemble(ir);
        assert!(
            assembled.is_ok(),
            "hand-built test ir failed to assemble: {assembled:?}"
        );

        assembled.ok().map_or_else(
            || GraphViews {
                forward: Csr::from_sorted_edges(0, &[]),
                reverse: Csr::from_sorted_edges(0, &[]),
            },
            |snapshot| build_csr(&snapshot, filter),
        )
    }

    #[test]
    fn should_build_forward_adjacency_with_sorted_rows() {
        let graph = views(
            3,
            vec![
                edge(0, 2, EdgeKind::Call, Hardness::Hard),
                edge(0, 1, EdgeKind::Call, Hardness::Hard),
            ],
            HardnessFilter::All,
        );

        assert_eq!(graph.forward.neighbors(0), &[1, 2]);
    }

    #[test]
    fn should_reverse_edge_direction_in_the_reverse_view() {
        let graph = views(
            2,
            vec![edge(0, 1, EdgeKind::Call, Hardness::Hard)],
            HardnessFilter::All,
        );

        assert_eq!(graph.reverse.neighbors(1), &[0]);
    }

    #[test]
    fn should_keep_only_hard_edges_under_the_hard_filter() {
        let graph = views(
            3,
            vec![
                edge(0, 1, EdgeKind::Call, Hardness::Hard),
                edge(0, 2, EdgeKind::Call, Hardness::Soft),
            ],
            HardnessFilter::HardOnly,
        );

        assert_eq!(graph.forward.neighbors(0), &[1]);
    }

    #[test]
    fn should_weight_each_edge_by_kind_and_confidence() {
        let mut inheritance = edge(0, 1, EdgeKind::Inheritance, Hardness::Hard);
        inheritance.confidence = 0.5;

        let graph = views(2, vec![inheritance], HardnessFilter::All);

        // 1.5 (inheritance) × 0.5 (confidence) = 0.75, exactly representable in f32.
        let weights: Vec<u32> = graph
            .forward
            .weights(0)
            .iter()
            .map(|w| w.to_bits())
            .collect();
        assert_eq!(weights, vec![0.75_f32.to_bits()]);
    }

    #[test]
    fn should_report_vertex_and_edge_counts() {
        let graph = views(
            3,
            vec![
                edge(0, 1, EdgeKind::Call, Hardness::Hard),
                edge(1, 2, EdgeKind::Call, Hardness::Hard),
            ],
            HardnessFilter::All,
        );

        assert_eq!(
            (graph.forward.vertex_count(), graph.forward.edge_count()),
            (3, 2)
        );
    }

    #[test]
    fn should_reverse_edges_keeping_self_loops_and_weights() {
        let graph = Csr::from_weighted_edges(3, &[(0, 1, 2.0), (1, 1, 3.0), (2, 1, 4.0)]);
        let reversed = graph.reversed();
        assert_eq!(reversed.neighbors(1), &[0, 1, 2]);
        assert_eq!(reversed.weights(1), &[2.0, 3.0, 4.0]);
        assert!(reversed.neighbors(0).is_empty());
        assert_eq!(reversed.reversed(), graph);
    }

    #[test]
    fn should_sum_parallel_edge_weights_in_the_weighted_constructor() {
        // two parallel 0 -> 1 edges (1.0 + 0.5) and one 0 -> 2 edge, unsorted.
        let csr = Csr::from_weighted_edges(3, &[(0, 2, 2.0), (0, 1, 1.0), (0, 1, 0.5)]);

        assert_eq!(csr.neighbors(0), &[1, 2]);
        let weights: Vec<u32> = csr.weights(0).iter().map(|w| w.to_bits()).collect();
        assert_eq!(weights, vec![1.5_f32.to_bits(), 2.0_f32.to_bits()]);
    }

    #[test]
    fn should_sort_rows_in_the_weighted_constructor() {
        let csr = Csr::from_weighted_edges(3, &[(2, 0, 1.0), (1, 0, 1.0), (2, 1, 1.0)]);

        assert_eq!(
            (csr.neighbors(1), csr.neighbors(2), csr.edge_count()),
            (&[0][..], &[0, 1][..], 3)
        );
    }

    #[test]
    fn should_return_empty_slices_for_an_out_of_range_vertex() {
        let graph = views(1, vec![], HardnessFilter::All);

        assert_eq!(
            (graph.forward.neighbors(9), graph.forward.weights(9)),
            (&[][..], &[][..])
        );
    }
}
