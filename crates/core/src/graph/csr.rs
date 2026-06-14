//! Compressed sparse row (CSR) adjacency in a struct-of-arrays layout (ad-6).
//!
//! CSR stores the whole graph in three flat arrays — row offsets, edge targets,
//! and per-edge weights — giving cache-friendly sequential scans of any node's
//! neighbours. [`build_csr`] derives both the forward and the reverse view in a
//! single pass over snapshot edges, filtered by hardness, and sorts each row so
//! the layout is identical across runs and thread counts.

use strata_ir::{Edge, EdgeKind, Hardness, NodeId, Snapshot};

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

/// Default scoring weight for an edge kind, mirroring the `[weights]` config
/// defaults (a config-less run uses exactly these). Re-exports are flattened
/// during normalization and carry no weight here.
fn kind_weight(kind: EdgeKind) -> f32 {
    match kind {
        EdgeKind::ValueImport | EdgeKind::Call => 1.0,
        EdgeKind::Inheritance => 1.5,
        EdgeKind::TypeReference => 0.3,
        EdgeKind::ReExport => 0.0,
    }
}

/// The weight assigned to a single edge: kind weight scaled by binder confidence.
fn edge_weight(edge: &Edge) -> f32 {
    // Edge weights are f32 by the CSR contract (ad-6); narrowing the f64
    // confidence is intentional and the only lossy step, so the truncation lint
    // is allowed here alone.
    #[allow(clippy::cast_possible_truncation)]
    let confidence = edge.confidence as f32;
    kind_weight(edge.kind) * confidence
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
    let ir = snapshot.ir();
    let vertex_count = ir.nodes.len();

    let forward = collect_directed(&ir.edges, vertex_count, filter, Direction::Forward);
    let reverse = collect_directed(&ir.edges, vertex_count, filter, Direction::Reverse);

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
            (row.0, column.0, edge_weight(edge))
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
        Container, ContainerId, ContainerTree, IntermediateRepresentation, Node, NodeKind,
        Polarity, ScopeLevel,
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
    fn should_return_empty_slices_for_an_out_of_range_vertex() {
        let graph = views(1, vec![], HardnessFilter::All);

        assert_eq!(
            (graph.forward.neighbors(9), graph.forward.weights(9)),
            (&[][..], &[][..])
        );
    }
}
