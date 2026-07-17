//! Strongly-connected-component condensation via iterative Tarjan.
//!
//! Tarjan's algorithm collapses every strongly connected component (SCC) of the
//! hard-edge graph into a single node, yielding the quotient DAG every later
//! solver phase operates on. The traversal uses an explicit frame stack instead
//! of recursion, so a 100k-node dependency chain cannot overflow the call stack.
//! SCCs are emitted in reverse topological order, which — together with sorted
//! CSR rows — makes the condensation byte-identical across runs and thread
//! counts.

use strata_ir::NodeId;

use crate::graph::csr::Csr;

/// Identifier of a strongly connected component within a [`Condensation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SccId(pub u32);

/// The quotient DAG over strongly connected components, plus the mapping back to
/// member nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct Condensation {
    /// Quotient DAG over SCCs, with deduplicated edges.
    pub dag: Csr,
    /// Owning SCC of each node, indexed by [`NodeId`].
    pub membership: Vec<SccId>,
    /// Member node lists per SCC, each in ascending node-id order.
    pub members: Vec<Vec<NodeId>>,
}

/// A single Tarjan DFS frame: the vertex under exploration and a cursor into its
/// neighbour slice.
struct Frame {
    /// The vertex this frame explores.
    node: u32,
    /// Index of the next neighbour to visit within the vertex's CSR row.
    cursor: usize,
}

/// Sentinel marking a vertex that has not yet been assigned a DFS index.
const UNVISITED: u32 = u32::MAX;

/// Condenses the hard-edge graph `csr` into its SCC quotient DAG.
///
/// The traversal is iterative: each `strongconnect` recursion becomes a push to
/// an explicit frame stack, so depth is bounded by heap memory rather than the
/// call stack. On a frame exit where `lowlink == index`, one SCC is popped off
/// the component stack. Components are numbered in the order they close, i.e.
/// reverse topological order of the quotient DAG.
#[must_use]
pub fn condense(csr: &Csr) -> Condensation {
    let vertex_count = csr.vertex_count();

    let mut state = TarjanState::new(vertex_count);
    for root in 0..vertex_count {
        let root = u32::try_from(root).unwrap_or(UNVISITED);
        if state.index_of(root) == UNVISITED {
            state.run_from(root, csr);
        }
    }

    state.finish(csr)
}

/// Mutable bookkeeping for one full Tarjan run.
struct TarjanState {
    /// DFS discovery index per vertex, or [`UNVISITED`].
    index: Vec<u32>,
    /// Lowest discovery index reachable from each vertex.
    lowlink: Vec<u32>,
    /// Whether each vertex is currently on the component stack.
    on_stack: Vec<bool>,
    /// The component (tentative-SCC) stack of vertices.
    component: Vec<u32>,
    /// The explicit DFS frame stack replacing recursion.
    frames: Vec<Frame>,
    /// Owning SCC of each vertex once assigned, or [`UNVISITED`] while pending.
    scc_of: Vec<u32>,
    /// Member node lists per SCC, in the order SCCs close.
    members: Vec<Vec<NodeId>>,
    /// Monotonic source of DFS discovery indices.
    next_index: u32,
}

impl TarjanState {
    /// Allocates bookkeeping for a graph of `vertex_count` vertices.
    fn new(vertex_count: usize) -> Self {
        Self {
            index: vec![UNVISITED; vertex_count],
            lowlink: vec![UNVISITED; vertex_count],
            on_stack: vec![false; vertex_count],
            component: Vec::new(),
            frames: Vec::new(),
            scc_of: vec![UNVISITED; vertex_count],
            members: Vec::new(),
            next_index: 0,
        }
    }

    /// Returns the DFS discovery index of `node`, or [`UNVISITED`].
    fn index_of(&self, node: u32) -> u32 {
        self.index.get(node as usize).copied().unwrap_or(UNVISITED)
    }

    /// Runs the iterative DFS rooted at the unvisited vertex `root`.
    fn run_from(&mut self, root: u32, csr: &Csr) {
        self.discover(root);
        self.frames.push(Frame {
            node: root,
            cursor: 0,
        });

        while let Some(frame) = self.frames.last_mut() {
            let node = frame.node;
            let neighbors = csr.neighbors(node);

            if let Some(&next) = neighbors.get(frame.cursor) {
                frame.cursor += 1;
                self.visit_edge(node, next);
            } else {
                self.close(node);
            }
        }
    }

    /// Assigns `node` its discovery index and pushes it onto the component stack.
    fn discover(&mut self, node: u32) {
        let id = self.next_index;
        self.next_index += 1;
        if let Some(slot) = self.index.get_mut(node as usize) {
            *slot = id;
        }
        if let Some(slot) = self.lowlink.get_mut(node as usize) {
            *slot = id;
        }
        if let Some(slot) = self.on_stack.get_mut(node as usize) {
            *slot = true;
        }
        self.component.push(node);
    }

    /// Processes the edge `node -> next`: descend into unvisited targets, or
    /// relax `node`'s lowlink against an on-stack back/cross edge.
    fn visit_edge(&mut self, node: u32, next: u32) {
        if self.index_of(next) == UNVISITED {
            self.discover(next);
            self.frames.push(Frame {
                node: next,
                cursor: 0,
            });
        } else if self.on_stack.get(next as usize).copied().unwrap_or(false) {
            let candidate = self.index_of(next);
            self.relax_lowlink(node, candidate);
        }
    }

    /// Lowers `node`'s lowlink to `candidate` when that is smaller.
    fn relax_lowlink(&mut self, node: u32, candidate: u32) {
        if let Some(slot) = self.lowlink.get_mut(node as usize) {
            *slot = (*slot).min(candidate);
        }
    }

    /// Pops the DFS frame for `node`; if `node` roots an SCC, emits it, otherwise
    /// propagates its lowlink up to its parent frame.
    fn close(&mut self, node: u32) {
        self.frames.pop();

        let node_low = self
            .lowlink
            .get(node as usize)
            .copied()
            .unwrap_or(UNVISITED);
        if node_low == self.index_of(node) {
            self.emit_scc(node);
        }

        if let Some(parent) = self.frames.last() {
            let parent = parent.node;
            self.relax_lowlink(parent, node_low);
        }
    }

    /// Pops the component stack down to `root`, forming one SCC.
    fn emit_scc(&mut self, root: u32) {
        let scc_index = u32::try_from(self.members.len()).unwrap_or(UNVISITED);
        let mut group: Vec<NodeId> = Vec::new();

        while let Some(member) = self.component.pop() {
            if let Some(slot) = self.on_stack.get_mut(member as usize) {
                *slot = false;
            }
            if let Some(slot) = self.scc_of.get_mut(member as usize) {
                *slot = scc_index;
            }
            group.push(NodeId(member));
            if member == root {
                break;
            }
        }

        group.sort_by_key(|node| node.0);
        self.members.push(group);
    }

    /// Builds the [`Condensation`] result: membership, ordered member lists, and
    /// the deduplicated quotient DAG.
    fn finish(self, csr: &Csr) -> Condensation {
        let membership: Vec<SccId> = self.scc_of.iter().map(|&scc| SccId(scc)).collect();
        let dag = build_quotient(csr, &self.scc_of, self.members.len());
        Condensation {
            dag,
            membership,
            members: self.members,
        }
    }
}

/// Builds the quotient DAG over `scc_count` SCCs: an edge `a -> b` exists when
/// some member edge crosses from SCC `a` to a distinct SCC `b`. Self-loops are
/// dropped and parallel crossings collapsed with their weights summed, so the
/// quotient carries the aggregate pull between SCCs — the currency heavy-edge
/// matching and FM refinement rank moves by.
fn build_quotient(csr: &Csr, scc_of: &[u32], scc_count: usize) -> Csr {
    let mut crossings: Vec<(u32, u32, f32)> = Vec::new();
    for node in 0..scc_of.len() {
        let from = scc_of.get(node).copied().unwrap_or(UNVISITED);
        let node = u32::try_from(node).unwrap_or(UNVISITED);
        let weights = csr.weights(node);
        for (slot, &target) in csr.neighbors(node).iter().enumerate() {
            let to = scc_of.get(target as usize).copied().unwrap_or(UNVISITED);
            if from != to {
                let weight = weights.get(slot).copied().unwrap_or(0.0);
                crossings.push((from, to, weight));
            }
        }
    }

    Csr::from_weighted_edges(scc_count, &crossings)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use smol_str::SmolStr;
    use strata_ir::{
        Container, ContainerId, ContainerTree, Edge, EdgeKind, Hardness,
        IntermediateRepresentation, Node, NodeKind, Polarity, ScopeLevel, Snapshot,
    };

    use super::*;
    use crate::graph::csr::{HardnessFilter, build_csr};

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

    fn edge(source: u32, target: u32) -> Edge {
        Edge {
            source: NodeId(source),
            target: NodeId(target),
            kind: EdgeKind::Call,
            hardness: Hardness::Hard,
            confidence: 1.0,
        }
    }

    fn forward(node_count: u32, edges: Vec<Edge>) -> Csr {
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
            || Csr::from_sorted_edges(0, &[]),
            |snap| build_csr(&snap, HardnessFilter::HardOnly).forward,
        )
    }

    /// Returns the SCC ids of `nodes` under `condensation`.
    fn scc_ids(condensation: &Condensation, nodes: &[u32]) -> Vec<u32> {
        nodes
            .iter()
            .filter_map(|&n| condensation.membership.get(n as usize))
            .map(|scc| scc.0)
            .collect()
    }

    #[test]
    fn should_place_each_node_of_a_dag_in_its_own_scc() {
        let csr = forward(3, vec![edge(0, 1), edge(1, 2)]);

        let condensation = condense(&csr);

        assert_eq!(condensation.members.len(), 3);
    }

    #[test]
    fn should_collapse_a_cycle_into_one_scc() {
        let csr = forward(3, vec![edge(0, 1), edge(1, 2), edge(2, 0)]);

        let condensation = condense(&csr);

        assert_eq!(
            condensation.members,
            vec![vec![NodeId(0), NodeId(1), NodeId(2)]]
        );
    }

    #[test]
    fn should_assign_cycle_members_the_same_scc() {
        let csr = forward(4, vec![edge(0, 1), edge(1, 0), edge(1, 2), edge(2, 3)]);

        let condensation = condense(&csr);

        let pair = scc_ids(&condensation, &[0, 1]);
        assert_eq!(pair.first(), pair.last());
    }

    #[test]
    fn should_emit_sccs_in_reverse_topological_order() {
        // 0 -> 1 -> 2: sinks close first, so scc(2) < scc(1) < scc(0).
        let csr = forward(3, vec![edge(0, 1), edge(1, 2)]);

        let condensation = condense(&csr);

        let ids = scc_ids(&condensation, &[0, 1, 2]);
        assert_eq!(ids, vec![2, 1, 0]);
    }

    #[test]
    fn should_build_a_quotient_edge_between_distinct_sccs() {
        let csr = forward(2, vec![edge(0, 1)]);

        let condensation = condense(&csr);

        // node 1 is the sink (scc 0); node 0 is scc 1 with an edge into scc 0.
        assert_eq!(condensation.dag.neighbors(1), &[0]);
    }

    #[test]
    fn should_drop_self_loops_from_the_quotient_dag() {
        let csr = forward(2, vec![edge(0, 1), edge(1, 0)]);

        let condensation = condense(&csr);

        assert_eq!(condensation.dag.edge_count(), 0);
    }

    #[test]
    fn should_deduplicate_parallel_crossings_in_the_quotient() {
        // Two edges from the {0,1} cycle into node 2 collapse to one quotient edge.
        let csr = forward(3, vec![edge(0, 1), edge(1, 0), edge(0, 2), edge(1, 2)]);

        let condensation = condense(&csr);

        assert_eq!(condensation.dag.edge_count(), 1);
    }

    #[test]
    fn should_sum_member_edge_weights_onto_the_quotient_edge() {
        // Two call edges (weight 1.0 each) cross from the {0,1} cycle into node 2:
        // the single quotient edge must carry their summed weight, not zero.
        let csr = forward(3, vec![edge(0, 1), edge(1, 0), edge(0, 2), edge(1, 2)]);

        let condensation = condense(&csr);

        // node 2 is the sink (scc 0); the cycle is scc 1 with one edge into it.
        let weight = condensation.dag.weights(1).first().copied().unwrap_or(0.0);
        assert!((weight - 2.0).abs() < f32::EPSILON);
    }

    #[test]
    fn should_survive_a_deep_chain_without_stack_overflow() {
        let depth = 100_000;
        let edges = (0..depth - 1).map(|i| edge(i, i + 1)).collect();
        let csr = forward(depth, edges);

        let condensation = condense(&csr);

        assert_eq!(condensation.members.len(), depth as usize);
    }

    proptest! {
        /// Over arbitrary graphs, condensation must partition the vertices:
        /// every node lands in exactly one SCC, membership ids stay within the
        /// SCC count, and the member lists cover every vertex exactly once.
        #[test]
        fn should_partition_every_vertex_into_exactly_one_scc(
            node_count in 1_u32..12,
            raw_edges in proptest::collection::vec((0_u32..12, 0_u32..12), 0..40),
        ) {
            let edges = raw_edges
                .into_iter()
                .filter(|&(source, target)| source < node_count && target < node_count)
                .map(|(source, target)| edge(source, target))
                .collect();
            let csr = forward(node_count, edges);

            let condensation = condense(&csr);

            prop_assert_eq!(condensation.membership.len(), node_count as usize);
            for scc in &condensation.membership {
                prop_assert!((scc.0 as usize) < condensation.members.len());
            }

            let mut covered: Vec<NodeId> =
                condensation.members.iter().flatten().copied().collect();
            covered.sort_by_key(|node| node.0);
            let expected: Vec<NodeId> = (0..node_count).map(NodeId).collect();
            prop_assert_eq!(covered, expected);
        }
    }
}
